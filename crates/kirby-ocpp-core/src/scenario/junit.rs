//! JUnit XML rendering of scenario reports, for CI systems.
//!
//! One `<testsuite>` per station run, one `<testcase>` per step. A run that never produced a
//! report (the station could not connect or boot) is a suite with a single errored `setup` case.

use std::fmt::Write as _;

use super::runner::{ScenarioReport, StepStatus};

/// One station's run, as input for [`junit_xml`].
#[derive(Debug, Clone, Copy)]
pub struct JunitSuite<'a> {
    /// Station id in fleet runs; `None` for a single station.
    pub station: Option<&'a str>,
    pub scenario: &'a str,
    /// The report, or why the run could not start.
    pub outcome: Result<&'a ScenarioReport, &'a str>,
}

#[derive(Default, Clone, Copy)]
struct Counts {
    tests: usize,
    failures: usize,
    errors: usize,
    skipped: usize,
    millis: u64,
}

impl Counts {
    fn add(&mut self, other: Counts) {
        self.tests += other.tests;
        self.failures += other.failures;
        self.errors += other.errors;
        self.skipped += other.skipped;
        self.millis = self.millis.saturating_add(other.millis);
    }

    fn attributes(&self) -> String {
        format!(
            "tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{}\"",
            self.tests,
            self.failures,
            self.errors,
            self.skipped,
            seconds(self.millis)
        )
    }
}

/// Renders the suites as one JUnit XML document (`<testsuites>` root).
#[must_use]
pub fn junit_xml(suites: &[JunitSuite<'_>]) -> String {
    let mut total = Counts::default();
    let mut body = String::new();
    for suite in suites {
        let (xml, counts) = render_suite(suite);
        total.add(counts);
        body.push_str(&xml);
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites {}>\n{body}</testsuites>\n",
        total.attributes()
    )
}

fn render_suite(suite: &JunitSuite<'_>) -> (String, Counts) {
    let name = match suite.station {
        Some(station) => format!("{} [{station}]", suite.scenario),
        None => suite.scenario.to_owned(),
    };
    let classname = escape(&name);
    let mut cases = String::new();
    let mut counts = Counts::default();
    match suite.outcome {
        Ok(report) => {
            counts.millis = report.duration_ms;
            for step in &report.steps {
                counts.tests += 1;
                let case_name = escape(&format!("{}. {}", step.index + 1, step.name));
                let time = seconds(step.duration_ms);
                let _ = write!(
                    cases,
                    "    <testcase classname=\"{classname}\" name=\"{case_name}\" time=\"{time}\""
                );
                match &step.status {
                    StepStatus::Passed => cases.push_str("/>\n"),
                    StepStatus::Failed(reason) => {
                        counts.failures += 1;
                        let reason = escape(reason);
                        let _ = writeln!(
                            cases,
                            ">\n      <failure message=\"{reason}\" type=\"StepFailed\">{reason}</failure>\n    </testcase>"
                        );
                    }
                    StepStatus::Skipped => {
                        counts.skipped += 1;
                        cases.push_str(">\n      <skipped/>\n    </testcase>\n");
                    }
                }
            }
        }
        Err(error) => {
            counts.tests = 1;
            counts.errors = 1;
            let error = escape(error);
            let _ = writeln!(
                cases,
                "    <testcase classname=\"{classname}\" name=\"setup\" time=\"0.000\">\n      <error message=\"{error}\" type=\"SetupError\">{error}</error>\n    </testcase>"
            );
        }
    }
    let xml = format!(
        "  <testsuite name=\"{classname}\" {}>\n{cases}  </testsuite>\n",
        counts.attributes()
    );
    (xml, counts)
}

fn seconds(millis: u64) -> String {
    format!("{}.{:03}", millis / 1000, millis % 1000)
}

/// Escapes text for XML attributes and content; characters XML 1.0 cannot carry are dropped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if u32::from(c) < 0x20 || matches!(c, '\u{FFFE}' | '\u{FFFF}') => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::runner::StepReport;
    use super::*;

    fn step(index: usize, name: &str, status: StepStatus, duration_ms: u64) -> StepReport {
        StepReport {
            index,
            name: name.to_owned(),
            status,
            duration_ms,
        }
    }

    fn report(steps: Vec<StepReport>, passed: bool) -> ScenarioReport {
        ScenarioReport {
            scenario: "demo".to_owned(),
            passed,
            cancelled: false,
            steps,
            duration_ms: 1500,
        }
    }

    #[test]
    fn passing_run_has_no_failure_elements() {
        let report = report(vec![step(0, "boot", StepStatus::Passed, 20)], true);
        let xml = junit_xml(&[JunitSuite {
            station: None,
            scenario: "demo",
            outcome: Ok(&report),
        }]);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites "));
        assert!(xml.contains(
            "<testsuites tests=\"1\" failures=\"0\" errors=\"0\" skipped=\"0\" time=\"1.500\">"
        ));
        assert!(xml.contains("<testsuite name=\"demo\" tests=\"1\""));
        assert!(xml.contains("<testcase classname=\"demo\" name=\"1. boot\" time=\"0.020\"/>"));
        assert!(!xml.contains("<failure"));
    }

    #[test]
    fn failed_and_skipped_steps_are_counted_and_escaped() {
        let report = report(
            vec![
                step(0, "a", StepStatus::Passed, 1),
                step(
                    1,
                    "b <x>",
                    StepStatus::Failed("got \"Rejected\" & more\nlines\u{1}".to_owned()),
                    2,
                ),
                step(2, "c", StepStatus::Skipped, 0),
            ],
            false,
        );
        let xml = junit_xml(&[JunitSuite {
            station: Some("CP-1"),
            scenario: "demo",
            outcome: Ok(&report),
        }]);
        assert!(xml.contains(
            "<testsuite name=\"demo [CP-1]\" tests=\"3\" failures=\"1\" errors=\"0\" skipped=\"1\""
        ));
        assert!(xml.contains("name=\"2. b &lt;x&gt;\""));
        assert!(xml.contains(
            "<failure message=\"got &quot;Rejected&quot; &amp; more&#10;lines\" type=\"StepFailed\">"
        ));
        assert!(xml.contains("<skipped/>"));
    }

    #[test]
    fn run_without_report_is_one_errored_setup_case() {
        let good = report(vec![step(0, "boot", StepStatus::Passed, 5)], true);
        let xml = junit_xml(&[
            JunitSuite {
                station: Some("CP-1"),
                scenario: "demo",
                outcome: Ok(&good),
            },
            JunitSuite {
                station: Some("CP-2"),
                scenario: "demo",
                outcome: Err("connection refused"),
            },
        ]);
        assert!(xml.contains("<testsuites tests=\"2\" failures=\"0\" errors=\"1\" skipped=\"0\""));
        assert!(xml.contains("name=\"setup\" time=\"0.000\">"));
        assert!(xml.contains("<error message=\"connection refused\" type=\"SetupError\">"));
    }
}

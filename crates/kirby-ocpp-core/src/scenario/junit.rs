//! JUnit XML rendering of scenario reports, for CI systems.
//!
//! One `<testsuite>` per station run, one `<testcase>` per step. The `classname` is a grouping
//! label (`ocpp<version>.<scenario> [<station>]`), not a structure to parse. A run that never produced a
//! report (the station could not connect or boot) is a suite with a single errored `setup` case.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use super::runner::{ScenarioReport, StepReport, StepStatus};
use super::vars;
use super::{OCPP_VERSION, TOOL_VERSION};

/// One station's run, as input for [`junit_xml`].
#[derive(Debug, Clone, Copy)]
pub struct JunitSuite<'a> {
    /// Station id in fleet runs; `None` for a single station.
    pub station: Option<&'a str>,
    pub scenario: &'a str,
    /// The report, or why the run could not start.
    pub outcome: Result<&'a ScenarioReport, &'a str>,
}

/// A [`JunitSuite`] with what is known about the run even when it produced no report: the
/// scenario's `source` (file path or built-in name) and the time spent. A suite that has a
/// report takes both from the report.
#[derive(Debug, Clone, Copy)]
pub struct JunitRun<'a> {
    pub suite: JunitSuite<'a>,
    pub source: Option<&'a str>,
    pub duration_ms: u64,
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
    let runs: Vec<JunitRun<'_>> = suites
        .iter()
        .map(|suite| JunitRun {
            suite: *suite,
            source: None,
            duration_ms: 0,
        })
        .collect();
    junit_xml_runs(&runs)
}

/// [`junit_xml`] for runs that also know their source and duration, which fill the `source`
/// property and the `time` of a suite that never produced a report.
#[must_use]
pub fn junit_xml_runs(runs: &[JunitRun<'_>]) -> String {
    let mut total = Counts::default();
    let mut body = String::new();
    for run in runs {
        let (xml, counts) = render_suite(run);
        total.add(counts);
        body.push_str(&xml);
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites {}>\n{body}</testsuites>\n",
        total.attributes()
    )
}

fn render_suite(run: &JunitRun<'_>) -> (String, Counts) {
    let suite = &run.suite;
    let name = match suite.station {
        Some(station) => clean(&format!("{} [{station}]", suite.scenario)),
        None => clean(suite.scenario),
    };
    let ocpp_version = suite
        .outcome
        .map_or(OCPP_VERSION, |r| r.ocpp_version.as_str());
    // A label for grouping, not a parsed structure: names may contain `.` and `]`.
    let classname = escape(&clean(&format!("ocpp{ocpp_version}.{name}")));
    let mut cases = String::new();
    let mut counts = Counts::default();
    let mut properties = vec![("ocpp_version", ocpp_version.to_owned())];
    let mut station = suite.station.map(str::to_owned);
    let mut tool_version = TOOL_VERSION.to_owned();
    let mut source = run.source.map(str::to_owned);
    match suite.outcome {
        Ok(report) => {
            counts.millis = report.duration_ms;
            station = station.or_else(|| report.station.clone());
            tool_version.clone_from(&report.tool_version);
            if report.source.is_some() {
                source.clone_from(&report.source);
            }
            // Cleaned before the uniqueness pass, so two labels that differ only in
            // invisible characters get distinct ` #N` suffixes.
            let bases: Vec<String> = report
                .steps
                .iter()
                .map(|step| {
                    clean(if step.detail.label.is_empty() {
                        step.name.as_str()
                    } else {
                        step.detail.label.as_str()
                    })
                })
                .collect();
            let bases: Vec<&str> = bases.iter().map(String::as_str).collect();
            // Every written label is taken up front, so a generated ` #N` never collides with
            // an explicit label, whichever comes first.
            let mut taken: HashSet<String> = bases.iter().map(|b| (*b).to_owned()).collect();
            let mut seen: HashMap<&str, usize> = HashMap::new();
            for (step, base) in report.steps.iter().zip(bases) {
                counts.tests += 1;
                let count = seen.entry(base).or_default();
                *count += 1;
                let label = if *count > 1 {
                    let mut n = *count;
                    let label = loop {
                        let candidate = format!("{base} #{n}");
                        if taken.insert(candidate.clone()) {
                            break candidate;
                        }
                        n += 1;
                    };
                    *count = n;
                    label
                } else {
                    base.to_owned()
                };
                render_case(&mut cases, &mut counts, &classname, step, &label);
            }
        }
        Err(error) => {
            counts.tests = 1;
            counts.errors = 1;
            counts.millis = run.duration_ms;
            let error = escape(error);
            let time = seconds(run.duration_ms);
            let _ = writeln!(
                cases,
                "    <testcase classname=\"{classname}\" name=\"setup\" time=\"{time}\">\n      <error message=\"{error}\" type=\"SetupError\">{error}</error>\n    </testcase>"
            );
        }
    }
    if let Some(station) = station {
        properties.push(("station", station));
    }
    properties.push(("tool_version", tool_version));
    if let Some(source) = source {
        properties.push(("source", source));
    }
    let mut props = String::from("    <properties>\n");
    for (key, value) in &properties {
        let _ = writeln!(
            props,
            "      <property name=\"{key}\" value=\"{}\"/>",
            escape(&clean(value))
        );
    }
    props.push_str("    </properties>\n");
    let xml = format!(
        "  <testsuite name=\"{}\" {}>\n{props}{cases}  </testsuite>\n",
        escape(&name),
        counts.attributes()
    );
    (xml, counts)
}

fn render_case(
    cases: &mut String,
    counts: &mut Counts,
    classname: &str,
    step: &StepReport,
    label: &str,
) {
    let case_name = escape(label);
    let time = seconds(step.duration_ms);
    let mut case_properties = format!(
        "      <properties>\n        <property name=\"index\" value=\"{}\"/>\n",
        step.index + 1
    );
    if !step.detail.action.is_empty() {
        let _ = writeln!(
            case_properties,
            "        <property name=\"action\" value=\"{}\"/>",
            escape(&step.detail.action)
        );
    }
    case_properties.push_str("      </properties>\n");
    let _ = write!(
        cases,
        "    <testcase classname=\"{classname}\" name=\"{case_name}\" time=\"{time}\">\n{case_properties}"
    );
    match &step.status {
        StepStatus::Passed => {}
        StepStatus::Failed(reason) => {
            counts.failures += 1;
            let text = escape(&failure_text(step, reason));
            let reason = escape(reason);
            let _ = writeln!(
                cases,
                "      <failure message=\"{reason}\" type=\"StepFailed\">{text}</failure>"
            );
        }
        StepStatus::Skipped => {
            counts.skipped += 1;
            cases.push_str("      <skipped/>\n");
        }
    }
    cases.push_str("    </testcase>\n");
}

/// The text of a `<failure>`: the reason plus the step's evidence, one item per line.
fn failure_text(step: &StepReport, reason: &str) -> String {
    let mut text = reason.to_owned();
    if !step.detail.action.is_empty() {
        let _ = write!(text, "\naction: {}", step.detail.action);
    }
    if let Some(params) = &step.detail.params {
        let _ = write!(text, "\nparams: {}", vars::clip_json(params));
    }
    if let Some(expected) = &step.detail.expected {
        let _ = write!(text, "\nexpected: {}", vars::clip_json(expected));
    }
    if let Some(actual) = &step.detail.actual {
        let _ = write!(text, "\nactual: {actual}");
    }
    text
}

/// `text` with every character that is unsafe in a terminal or invisible (controls, line
/// separators, bidirectional and zero-width marks) replaced by a space, for names and
/// labels.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if crate::text::is_unsafe_char(c) {
                ' '
            } else {
                c
            }
        })
        .collect()
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
        StepReport::new(index, name.to_owned(), status, duration_ms)
    }

    fn report(steps: Vec<StepReport>, passed: bool) -> ScenarioReport {
        ScenarioReport::new("demo".to_owned(), passed, false, steps, 1500)
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
        assert!(xml.contains(
            "<testcase classname=\"ocpp1.6.demo\" name=\"boot\" time=\"0.020\">\n      <properties>\n        <property name=\"index\" value=\"1\"/>"
        ));
        assert!(xml.contains("<property name=\"ocpp_version\" value=\"1.6\"/>"));
        assert!(xml.contains("<property name=\"tool_version\" value="));
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
        assert!(xml.contains("name=\"b &lt;x&gt;\""));
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

    #[test]
    fn duplicate_labels_get_a_counter_and_failures_carry_evidence() {
        let mut failing = step(
            2,
            "expect_call Heartbeat out",
            StepStatus::Failed("timed out".into()),
            3,
        );
        failing.detail.action = "expect_call".into();
        failing.detail.params = Some(serde_json::json!({"call": "Heartbeat"}));
        failing.detail.expected = Some(serde_json::json!({"call": "Heartbeat"}));
        failing.detail.actual = Some("{\"x\":1}".into());
        let mut report = report(
            vec![
                step(0, "heartbeat", StepStatus::Passed, 1),
                step(1, "heartbeat", StepStatus::Passed, 1),
                failing,
            ],
            false,
        );
        report.station = Some("CP-9".into());
        report.source = Some("builtin:demo".into());
        let xml = junit_xml(&[JunitSuite {
            station: None,
            scenario: "demo",
            outcome: Ok(&report),
        }]);
        assert!(xml.contains("name=\"heartbeat\""));
        assert!(xml.contains("name=\"heartbeat #2\""));
        assert!(xml.contains("<property name=\"index\" value=\"3\"/>"));
        assert!(xml.contains("<property name=\"station\" value=\"CP-9\"/>"));
        assert!(xml.contains("<property name=\"source\" value=\"builtin:demo\"/>"));
        assert!(
            xml.contains(
                "action: expect_call&#10;params: {&quot;call&quot;:&quot;Heartbeat&quot;}"
            )
        );
        assert!(xml.contains("&#10;expected: "));
        assert!(xml.contains("&#10;actual: {&quot;x&quot;:1}"));
    }

    #[test]
    fn generated_suffix_never_collides_with_an_explicit_label() {
        let report = report(
            vec![
                step(0, "heartbeat", StepStatus::Passed, 1),
                step(1, "heartbeat", StepStatus::Passed, 1),
                step(2, "heartbeat #2", StepStatus::Passed, 1),
            ],
            true,
        );
        let xml = junit_xml(&[JunitSuite {
            station: None,
            scenario: "demo",
            outcome: Ok(&report),
        }]);
        let names: Vec<&str> = xml
            .split("<testcase ")
            .skip(1)
            .filter_map(|c| c.split(" name=\"").nth(1)?.split('"').next())
            .collect();
        assert_eq!(names, ["heartbeat", "heartbeat #3", "heartbeat #2"]);
    }

    #[test]
    fn classname_has_no_line_breaks() {
        let report = report(vec![step(0, "a", StepStatus::Passed, 1)], true);
        let xml = junit_xml(&[JunitSuite {
            station: Some("CP\n1"),
            scenario: "de.mo]",
            outcome: Ok(&report),
        }]);
        assert!(xml.contains("classname=\"ocpp1.6.de.mo] [CP 1]\""), "{xml}");
    }

    #[test]
    fn setup_error_suites_carry_source_and_time_when_known() {
        let run = JunitRun {
            suite: JunitSuite {
                station: None,
                scenario: "demo",
                outcome: Err("never booted"),
            },
            source: Some("scenarios/demo.toml"),
            duration_ms: 2345,
        };
        let xml = junit_xml_runs(&[run]);
        assert!(
            xml.contains("<property name=\"source\" value=\"scenarios/demo.toml\"/>"),
            "{xml}"
        );
        assert!(xml.contains("name=\"setup\" time=\"2.345\""), "{xml}");
        assert!(xml.contains("<testsuite name=\"demo\" tests=\"1\" failures=\"0\" errors=\"1\" skipped=\"0\" time=\"2.345\">"), "{xml}");
        // Without the extra facts the output is what `junit_xml` always produced.
        let plain = junit_xml(&[run.suite]);
        assert!(
            plain.contains("time=\"0.000\"") && !plain.contains("\"source\""),
            "{plain}"
        );
    }

    #[test]
    fn names_lose_invisible_and_bidi_characters() {
        let report = report(
            vec![
                step(0, "a\u{202e}b\u{200b}c", StepStatus::Passed, 1),
                step(1, "a b c", StepStatus::Passed, 1),
            ],
            true,
        );
        let xml = junit_xml(&[JunitSuite {
            station: Some("CP\u{feff}1"),
            scenario: "de\u{2067}mo",
            outcome: Ok(&report),
        }]);
        for c in ['\u{202e}', '\u{200b}', '\u{feff}', '\u{2067}'] {
            assert!(!xml.contains(c), "{c:?} in {xml}");
        }
        assert!(xml.contains("name=\"a b c\""), "{xml}");
        // The cleaned labels collide and are made distinct.
        assert!(xml.contains("name=\"a b c #2\""), "{xml}");
        assert!(xml.contains("<testsuite name=\"de mo [CP 1]\""), "{xml}");
    }
}

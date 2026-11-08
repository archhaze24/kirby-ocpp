//! `check`: the connection doctor. The engine lives in `kirby_ocpp_core::ws::probe`; here are
//! the output and the exit code.

use std::process::ExitCode;
use std::time::Duration;

use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::ws::probe::{self, ProbeOptions, Report, Step, StepStatus};

use crate::cli::CheckArgs;
use crate::headless::stdout_line;

/// One step as a line of the checklist.
fn step_line(step: &Step) -> String {
    format!("{}  {:<9} {}", step.status.label(), step.name, step.detail)
}

/// The closing line: `PASSED` or `FAILED` and the counts.
fn summary_line(report: &Report) -> String {
    let count = |status: StepStatus| report.steps.iter().filter(|s| s.status == status).count();
    let (passed, warnings, failed, skipped) = (
        count(StepStatus::Passed),
        count(StepStatus::Warning),
        count(StepStatus::Failed),
        count(StepStatus::Skipped),
    );
    let verdict = if report.passed() { "PASSED" } else { "FAILED" };
    let mut parts = vec![format!("{passed} passed")];
    if warnings > 0 {
        parts.push(format!("{warnings} warning{}", plural(warnings)));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    format!("{verdict}  {}", parts.join(", "))
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Runs the checklist, prints it and returns the exit code (0 no step failed, 1 a step failed).
pub async fn run(config: &StationConfig, args: &CheckArgs, json: bool) -> ExitCode {
    let options = ProbeOptions::from_config(config, Duration::from_secs(args.timeout));
    let mut live = |step: &Step| {
        if !json {
            stdout_line(&step_line(step), false);
            if let Some(hint) = &step.hint {
                stdout_line(&format!("      hint: {hint}"), false);
            }
        }
    };
    let report = probe::run(&options, &mut live).await;
    if json {
        match serde_json::to_string(&report) {
            Ok(text) => stdout_line(&text, true),
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        stdout_line(&summary_line(&report), false);
    }
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &'static str, status: StepStatus) -> Step {
        Step {
            name,
            status,
            detail: format!("{name} detail"),
            hint: None,
            duration_ms: 1,
        }
    }

    fn report(steps: Vec<Step>) -> Report {
        let failed = steps.iter().any(|s| s.status == StepStatus::Failed);
        Report {
            schema: 1,
            ocpp_version: "1.6",
            tool_version: "x",
            url: "ws://h/ocpp/CP".to_owned(),
            status: if failed { "failed" } else { "passed" },
            steps,
        }
    }

    #[test]
    fn lines_have_a_fixed_layout() {
        let line = step_line(&step("dns", StepStatus::Passed));
        assert_eq!(line, "PASS  dns       dns detail");
        assert!(step_line(&step("tls", StepStatus::Skipped)).starts_with("SKIP  tls "));
    }

    #[test]
    fn the_summary_counts_each_kind() {
        let passed = report(vec![
            step("url", StepStatus::Passed),
            step("boot", StepStatus::Warning),
        ]);
        assert_eq!(summary_line(&passed), "PASSED  1 passed, 1 warning");
        let failed = report(vec![
            step("url", StepStatus::Passed),
            step("tcp", StepStatus::Failed),
            step("tls", StepStatus::Skipped),
            step("boot", StepStatus::Skipped),
        ]);
        assert_eq!(
            summary_line(&failed),
            "FAILED  1 passed, 1 failed, 2 skipped"
        );
    }
}

//! `replay` and `scenarios from-recording`: what a recording becomes after the session.

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::recording::{self, Recording, ReplayOptions};
use kirby_ocpp_tui::stdout_pump::print_lossy;
use kirby_ocpp_tui::text::sanitize;

use crate::cli::ReplayArgs;
use crate::headless::{self, Level};
use crate::record::RecordTarget;
use crate::{RunOutputs, execute_scenario, same_file, usage_error};

/// Most warnings printed (the converters keep at most 100).
const SHOWN_WARNINGS: usize = 20;

fn read(path: &Path) -> Result<Recording, ExitCode> {
    Recording::read(path).map_err(|error| usage_error(&sanitize(&error.to_string())))
}

fn print_warnings(warnings: &[String]) {
    if headless::level() == Level::Quiet {
        return;
    }
    for warning in warnings.iter().take(SHOWN_WARNINGS) {
        eprintln!("warning: {}", sanitize(warning));
    }
    if warnings.len() > SHOWN_WARNINGS {
        eprintln!("warning: ... and {} more", warnings.len() - SHOWN_WARNINGS);
    }
}

/// `replay`: send the CALLs the emulator originated in a recording to the CSMS again.
pub async fn run(
    config: StationConfig,
    args: &ReplayArgs,
    json: bool,
    log_file: Option<&Path>,
    record: Option<&RecordTarget>,
) -> ExitCode {
    let recorded = match read(&args.recording) {
        Ok(recorded) => recorded,
        Err(code) => return code,
    };
    if let Some(target) = record
        && same_file(&target.path, &args.recording)
    {
        return usage_error("--record would overwrite the recording that is being replayed");
    }
    let options = ReplayOptions {
        speed: args.speed,
        no_delay: args.no_delay,
    };
    let mut plan = match recording::plan_replay(&recorded, &options) {
        Ok(plan) => plan,
        Err(error) => return usage_error(&sanitize(&error.to_string())),
    };
    print_warnings(&plan.warnings);
    if headless::level() != Level::Quiet {
        eprintln!(
            "note: replaying {} CALLs from {}{}",
            plan.replayed,
            sanitize(&args.recording.display().to_string()),
            if plan.skipped > 0 {
                format!(" ({} skipped)", plan.skipped)
            } else {
                String::new()
            }
        );
    }
    plan.scenario.source = Some(args.recording.display().to_string());
    let outputs = RunOutputs {
        report: args.report.as_deref(),
        junit: args.junit.as_deref(),
        timeout: args.timeout,
        vars: &[],
    };
    execute_scenario(config, plan.scenario, &outputs, json, log_file, record).await
}

/// `scenarios from-recording`: write the scenario to `output`, or to stdout.
pub fn from_recording(recording: &Path, output: Option<&Path>, overwrite: bool) -> ExitCode {
    let recorded = match read(recording) {
        Ok(recorded) => recorded,
        Err(code) => return code,
    };
    if let Some(output) = output
        && same_file(output, recording)
    {
        return usage_error("--output would overwrite the recording");
    }
    let conversion = match recording::to_scenario(&recorded) {
        Ok(conversion) => conversion,
        Err(error) => return usage_error(&sanitize(&error.to_string())),
    };
    print_warnings(&conversion.warnings);
    let Some(output) = output else {
        print_lossy(&conversion.toml);
        return ExitCode::SUCCESS;
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let shown = sanitize(&output.display().to_string());
    match options
        .open(output)
        .and_then(|mut file| file.write_all(conversion.toml.as_bytes()))
    {
        Ok(()) => {
            if headless::level() != Level::Quiet {
                eprintln!("note: wrote {} steps to {shown}", conversion.steps);
            }
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => usage_error(&format!(
            "{shown} already exists (use --overwrite to replace it)"
        )),
        Err(error) => usage_error(&format!("cannot write {shown}: {}", error.kind())),
    }
}

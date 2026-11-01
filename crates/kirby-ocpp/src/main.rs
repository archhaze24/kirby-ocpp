//! `kirby-ocpp`: terminal OCPP 1.6J charge point emulator.

mod cli;

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use kirby_ocpp_core::{Station, SystemClock};
use kirby_ocpp_tui::TuiOptions;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let print_config = cli.print_config;
    let mut config = match cli.into_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    config.normalize();
    if let Err(error) = config.validate() {
        eprintln!("error: {error}");
        return ExitCode::from(2);
    }

    if print_config {
        return match serde_json::to_string_pretty(&config) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
    let result = kirby_ocpp_tui::run(handle.clone(), TuiOptions::default()).await;
    // The station may already be gone; either way we are leaving.
    let _ = handle.shutdown().await;
    let _ = join.await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

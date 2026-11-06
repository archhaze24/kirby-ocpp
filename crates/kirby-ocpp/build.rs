//! Embeds the git commit, the build date and the target triple for `kirby-ocpp version`.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&manifest)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=KIRBY_OCPP_GIT_COMMIT={commit}");

    let seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
        .unwrap_or(0);
    println!(
        "cargo:rustc-env=KIRBY_OCPP_BUILD_DATE={}",
        utc_date(seconds)
    );

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=KIRBY_OCPP_TARGET={target}");

    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-changed=build.rs");
    // Rebuild when the checked-out commit changes (only in a git checkout).
    let git_dir = Path::new(&manifest).join("../../.git");
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(text) = std::fs::read_to_string(&head)
            && let Some(reference) = text.trim().strip_prefix("ref: ")
        {
            let path = git_dir.join(reference);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}

/// `YYYY-MM-DD` (UTC) of a Unix time (civil-from-days algorithm).
fn utc_date(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

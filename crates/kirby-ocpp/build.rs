//! Embeds the git commit, the build date and the target triple for `kirby-ocpp version`.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    // Only a checkout of this workspace counts: `git` walks up, so a copy under `~/.cargo`
    // or `target/package` inside some other repository must not report that repository's commit.
    let root = Path::new(&manifest).join("../..");
    let commit = if git(&root, &["rev-parse", "--show-toplevel"])
        .and_then(|top| Some(Path::new(&top).canonicalize().ok()? == root.canonicalize().ok()?))
        .unwrap_or(false)
    {
        git(&root, &["rev-parse", "--short", "HEAD"])
    } else {
        None
    }
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
    // Rebuild when the checked-out commit changes. `--git-path` resolves worktrees (where
    // `.git` is a file) and the packed refs.
    if commit != "unknown" {
        let mut paths = vec![git(&root, &["rev-parse", "--git-path", "HEAD"])];
        paths.push(git(&root, &["rev-parse", "--git-path", "packed-refs"]));
        if let Some(reference) = git(&root, &["symbolic-ref", "-q", "HEAD"]) {
            paths.push(git(&root, &["rev-parse", "--git-path", &reference]));
        }
        for path in paths.into_iter().flatten() {
            let path = Path::new(&path);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

/// Trimmed stdout of a successful `git` run in `dir`.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
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

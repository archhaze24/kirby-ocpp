use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kirby_ocpp_core::recording::{self, RecordOptions, RecordSink, Role};

use crate::cli::Cli;

pub struct Target {
    path: PathBuf,
    overwrite: bool,
    protected: Vec<(&'static str, PathBuf)>,
}

pub fn target(cli: &Cli) -> Result<Option<Target>, String> {
    let Some(path) = &cli.record else {
        return Ok(None);
    };
    let mut protected: Vec<_> = [
        ("the script", cli.script.as_deref()),
        ("the replay recording", cli.replay.as_deref()),
        ("the report", cli.report.as_deref()),
        ("the JUnit report", cli.junit.as_deref()),
        ("the log file", cli.log_file.as_deref()),
        ("the configuration file", cli.config.as_deref()),
        ("the API token file", cli.api_token_file.as_deref()),
        ("the API TLS certificate", cli.api_tls_cert.as_deref()),
        ("the API TLS key", cli.api_tls_key.as_deref()),
        ("the API client CA", cli.api_tls_client_ca.as_deref()),
        ("the TLS certificate", cli.tls_cert.as_deref()),
        ("the TLS key", cli.tls_key.as_deref()),
        ("the TLS client CA", cli.tls_client_ca.as_deref()),
        (
            "the authentication keys file",
            cli.auth_keys_file.as_deref(),
        ),
        (
            "the default authentication key file",
            cli.default_auth_key_file.as_deref(),
        ),
    ]
    .into_iter()
    .filter_map(|(name, other)| other.map(|path| (name, path.to_path_buf())))
    .collect();
    protected.extend(cli.blocks.stations.iter().filter_map(|station| {
        station
            .auth_key_file
            .as_ref()
            .map(|path| ("a station authentication key file", path.clone()))
    }));
    check_collisions(path, &protected)?;
    Ok(Some(Target {
        path: path.clone(),
        overwrite: cli.record_overwrite,
        protected,
    }))
}

fn check_collisions(path: &Path, protected: &[(&str, PathBuf)]) -> Result<(), String> {
    for (name, other) in protected {
        if same_file(path, other) {
            return Err(format!("--record would overwrite {name}"));
        }
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    fn resolve(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            match (std::fs::canonicalize(parent), path.file_name()) {
                (Ok(parent), Some(name)) => parent.join(name),
                _ => path.to_path_buf(),
            }
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b))
            && a.dev() == b.dev()
            && a.ino() == b.ino()
        {
            return true;
        }
    }
    resolve(a) == resolve(b)
}

pub async fn scope<F, Fut>(target: Option<Target>, body: F) -> ExitCode
where
    F: FnOnce(Option<RecordSink>) -> Fut,
    Fut: Future<Output = ExitCode>,
{
    let Some(target) = target else {
        return body(None).await;
    };
    let options = RecordOptions {
        // Reserve a new destination directly so filesystem aliases can be checked before
        // another output opens it. Existing destinations retain atomic replacement.
        overwrite: target.overwrite && target.path.exists(),
        ..RecordOptions::default()
    };
    let (sink, guard) = match recording::start(&target.path, "1.6", Role::Csms, "", &options) {
        Ok(started) => started,
        Err(error) => return crate::usage_error(&crate::headless::sanitize(&error.to_string())),
    };
    let collision = check_collisions(&target.path, &target.protected)
        .and_then(|()| check_collisions(guard.writing_path(), &target.protected));
    if let Err(error) = collision {
        let _ = tokio::task::spawn_blocking(move || guard.finish_unless_empty()).await;
        return crate::usage_error(&error);
    }
    let code = body(Some(sink)).await;
    let Ok(summary) = tokio::task::spawn_blocking(move || guard.finish_unless_empty()).await else {
        eprintln!("error: the recording could not be finished");
        return ExitCode::FAILURE;
    };
    if let Some(reason) = summary.incomplete {
        eprintln!(
            "error: the recording is incomplete: {}",
            crate::headless::sanitize(&reason)
        );
        return ExitCode::FAILURE;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn temporary_recording_cannot_alias_another_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "preserve").unwrap();
        let temporary = dir
            .path()
            .join(format!(".session.jsonl.{}.tmp", std::process::id()));
        let target = Target {
            path: path.clone(),
            overwrite: true,
            protected: vec![("the log file", temporary.clone())],
        };
        let code = scope(Some(target), |_| async {
            panic!("collision must be detected before opening outputs")
        })
        .await;
        assert_eq!(code, ExitCode::from(2));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "preserve");
        assert!(!temporary.exists());
    }

    #[tokio::test]
    async fn new_recording_checks_filesystem_case_aliases_before_opening_outputs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("case-probe"), "probe").unwrap();
        if !dir.path().join("CASE-PROBE").exists() {
            return;
        }
        for overwrite in [false, true] {
            let path = dir.path().join("session.jsonl");
            let target = Target {
                path: path.clone(),
                overwrite,
                protected: vec![("the log file", dir.path().join("SESSION.JSONL"))],
            };
            let code = scope(Some(target), |_| async {
                panic!("case alias must be detected before opening outputs")
            })
            .await;
            assert_eq!(code, ExitCode::from(2));
            assert!(!path.exists());
        }
    }
}

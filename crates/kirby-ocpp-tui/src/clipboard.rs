//! Clipboard access that never panics: external tools first, OSC 52 as the fallback.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::app::NoticeLevel;

/// Most terminals cap OSC 52 payloads around 100 kB.
pub const OSC52_MAX_BYTES: usize = 74_000;

/// The OSC 52 "set clipboard" escape sequence for `text`; `None` if it is too large.
pub fn osc52(text: &str) -> Option<String> {
    // Check the size before encoding: base64 turns 3 bytes into 4.
    if text.len().div_ceil(3).saturating_mul(4) > OSC52_MAX_BYTES {
        return None;
    }
    let encoded = STANDARD.encode(text);
    Some(format!("\x1b]52;c;{encoded}\x07"))
}

/// How long all external clipboard tools together get before they are killed.
const EXTERNAL_BUDGET: Duration = Duration::from_millis(500);

/// A clipboard tool: program name and arguments.
type Tool = (&'static str, &'static [&'static str]);

const TOOLS: &[Tool] = &[
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
    ("pbcopy", &[]),
    ("clip.exe", &[]),
];

/// Pipes `text` to the first clipboard tool that works; returns its name. A tool that has not
/// finished within 500 ms (in total) is killed, so it can neither block nor outlive the caller.
pub fn copy_external(text: &str) -> Option<&'static str> {
    copy_via(TOOLS, text, EXTERNAL_BUDGET)
}

fn copy_via(tools: &[Tool], text: &str, budget: Duration) -> Option<&'static str> {
    let deadline = Instant::now() + budget;
    tools.iter().find_map(|(name, args)| {
        let mut child = Command::new(name)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // A separate thread feeds stdin: a tool that never reads it must not block us.
        let writer = child.stdin.take().map(|mut stdin| {
            let data = text.to_owned();
            std::thread::spawn(move || stdin.write_all(data.as_bytes()).is_ok())
        });
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => break None,
            }
        };
        let Some(status) = status else {
            // Killing closes the pipe, which also ends the writer thread.
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        let wrote = writer.is_some_and(|w| w.join().unwrap_or(false));
        (wrote && status.success()).then_some(*name)
    })
}

/// Copies `text` (described as `what` in the notice): an external tool within 500 ms, else
/// the OSC 52 sequence written to `terminal`. Returns the notice to show.
pub async fn copy_with_fallback(
    text: &str,
    what: &str,
    terminal: &mut impl Write,
) -> (NoticeLevel, String) {
    // `copy_external` bounds itself, so the blocking task always ends and never delays exit.
    let ext = tokio::task::spawn_blocking({
        let text = text.to_owned();
        move || copy_external(&text)
    })
    .await;
    if let Ok(Some(tool)) = ext {
        (NoticeLevel::Success, format!("copied {what} ({tool})"))
    } else if let Some(seq) = osc52(text) {
        let _ = terminal.write_all(seq.as_bytes());
        let _ = terminal.flush();
        (
            NoticeLevel::Success,
            format!("copied {what} (OSC 52, needs terminal support)"),
        )
    } else {
        (
            NoticeLevel::Error,
            format!("{what} is too large to copy; use export"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_wraps_base64() {
        assert_eq!(osc52("hi").unwrap(), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn osc52_refuses_huge_payloads() {
        assert!(osc52(&"x".repeat(100_000)).is_none());
    }

    #[test]
    fn osc52_size_is_checked_before_encoding() {
        // Just under and over the limit: 55 500 bytes encode to exactly 74 000.
        assert!(osc52(&"x".repeat(55_500)).is_some());
        assert!(osc52(&"x".repeat(55_501)).is_none());
        assert!(osc52(&"x".repeat(50_000_000)).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn hung_helper_is_killed_at_the_budget() {
        // `sh -c 'sleep 30'` never reads stdin; the large text would fill the pipe.
        let tools: &[Tool] = &[("sh", &["-c", "sleep 30"])];
        let started = Instant::now();
        let got = copy_via(tools, &"x".repeat(1 << 20), Duration::from_millis(200));
        assert!(got.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn working_helper_reports_its_name() {
        let tools: &[Tool] = &[("cat", &[])];
        let got = copy_via(tools, "hello", Duration::from_secs(5));
        assert_eq!(got, Some("cat"));
    }
}

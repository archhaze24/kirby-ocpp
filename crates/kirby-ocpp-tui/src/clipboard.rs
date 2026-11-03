//! Clipboard access that never panics: external tools first, OSC 52 as the fallback.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::app::NoticeLevel;

/// Most terminals cap OSC 52 payloads around 100 kB.
pub const OSC52_MAX_BYTES: usize = 74_000;

/// The OSC 52 "set clipboard" escape sequence for `text`; `None` if it is too large.
pub fn osc52(text: &str) -> Option<String> {
    let encoded = STANDARD.encode(text);
    (encoded.len() <= OSC52_MAX_BYTES).then(|| format!("\x1b]52;c;{encoded}\x07"))
}

/// Pipes `text` to the first clipboard tool that works; returns its name.
pub fn copy_external(text: &str) -> Option<&'static str> {
    const TOOLS: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("pbcopy", &[]),
        ("clip.exe", &[]),
    ];
    TOOLS.iter().find_map(|(name, args)| {
        let mut child = Command::new(name)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        let ok = child.wait().is_ok_and(|s| s.success());
        (wrote && ok).then_some(*name)
    })
}

/// Copies `text` (described as `what` in the notice): an external tool within 500 ms, else
/// the OSC 52 sequence written to `terminal`. Returns the notice to show.
pub async fn copy_with_fallback(
    text: &str,
    what: &str,
    terminal: &mut impl Write,
) -> (NoticeLevel, String) {
    let ext = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::task::spawn_blocking({
            let text = text.to_owned();
            move || copy_external(&text)
        }),
    )
    .await;
    if let Ok(Ok(Some(tool))) = ext {
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
}

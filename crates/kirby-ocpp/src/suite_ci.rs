//! Pure helpers of `kirby-ocpp suite`: scenario selection (globs and tags) and the CI outputs
//! (GitHub workflow commands and the Markdown summary).

use std::fmt::Write as _;

use kirby_ocpp_core::text::escape_unsafe;

/// Whether `text` matches `pattern`, where `*` stands for any run of characters (also none)
/// and `?` for exactly one. Every other character matches itself, case-sensitively.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    // Position of the last `*` and the text position it currently covers up to.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || Some(&c) == text.get(t) => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern
        .get(p..)
        .is_some_and(|rest| rest.iter().all(|&c| c == '*'))
}

/// The selection options of a suite.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Selection<'a> {
    pub(crate) filter: &'a [String],
    pub(crate) skip: &'a [String],
    pub(crate) tag: &'a [String],
    pub(crate) skip_tag: &'a [String],
}

impl Selection<'_> {
    /// Whether any option is set.
    pub(crate) fn is_active(&self) -> bool {
        !(self.filter.is_empty()
            && self.skip.is_empty()
            && self.tag.is_empty()
            && self.skip_tag.is_empty())
    }

    /// Applies the options in order: `--filter` (any glob matches; no glob selects all), then
    /// `--skip` (any glob drops), then `--tag` (every tag is needed), then `--skip-tag`
    /// (any tag drops).
    pub(crate) fn selects(&self, name: &str, tags: &[String]) -> bool {
        (self.filter.is_empty() || self.filter.iter().any(|glob| glob_match(glob, name)))
            && !self.skip.iter().any(|glob| glob_match(glob, name))
            && self.tag.iter().all(|wanted| tags.contains(wanted))
            && !self.skip_tag.iter().any(|unwanted| tags.contains(unwanted))
    }
}

/// What the CI outputs need to know about one scenario row.
#[derive(Debug, Clone)]
pub(crate) struct CiRow {
    pub(crate) name: String,
    /// File path of the scenario; `None` for built-ins and rows without a file.
    pub(crate) file: Option<String>,
    /// `passed`, `failed`, `error` or `skipped`.
    pub(crate) status: &'static str,
    pub(crate) duration_ms: u64,
    pub(crate) attempts: u32,
    pub(crate) flaky: bool,
    /// Why the scenario failed, or why an earlier attempt of a flaky one failed.
    pub(crate) message: Option<String>,
}

/// Escapes the message of a workflow command (`%`, CR and LF).
fn escape_data(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escapes a property of a workflow command: the message escapes plus `:` and `,`.
fn escape_property(text: &str) -> String {
    escape_data(text).replace(':', "%3A").replace(',', "%2C")
}

/// One GitHub Actions workflow command. Control characters and invisible marks in the
/// untrusted texts are written as `\u{XXXX}` first.
pub(crate) fn workflow_command(
    level: &str,
    file: Option<&str>,
    title: &str,
    message: &str,
) -> String {
    let mut line = format!("::{level} ");
    if let Some(file) = file {
        let _ = write!(line, "file={},", escape_property(&escape_unsafe(file)));
    }
    let _ = write!(
        line,
        "title={}::{}",
        escape_property(&escape_unsafe(title)),
        escape_data(&escape_unsafe(message))
    );
    line
}

/// The workflow commands for the rows that failed (`error`) or passed after a failure
/// (`warning`), in row order.
pub(crate) fn annotations(rows: &[CiRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| {
            let file = row.file.as_deref();
            match row.status {
                "failed" | "error" => Some(workflow_command(
                    "error",
                    file,
                    &row.name,
                    row.message.as_deref().unwrap_or("failed"),
                )),
                "passed" if row.flaky => Some(workflow_command(
                    "warning",
                    file,
                    &row.name,
                    &format!(
                        "flaky: passed on attempt {} after a failure ({})",
                        row.attempts,
                        row.message.as_deref().unwrap_or("failed")
                    ),
                )),
                _ => None,
            }
        })
        .collect()
}

/// Longest failure message in a summary cell, in characters.
const CELL_LIMIT: usize = 300;

/// Text for a Markdown table cell: unsafe characters escaped, clipped, and `|`, `<`, `>`,
/// and `&` neutralized.
fn cell(text: &str) -> String {
    let clean = escape_unsafe(text);
    let clipped: String = if clean.chars().count() > CELL_LIMIT {
        let mut short: String = clean.chars().take(CELL_LIMIT).collect();
        short.push_str("...");
        short
    } else {
        clean
    };
    clipped
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "\\|")
        .replace('`', "'")
}

/// A duration: milliseconds below one second, else seconds with one decimal.
fn secs(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
    }
}

/// The Markdown summary of a suite: a headline and one table row per scenario.
pub(crate) fn summary_markdown(rows: &[CiRow], duration_ms: u64) -> String {
    let count = |status: &str| rows.iter().filter(|row| row.status == status).count();
    let passed = count("passed");
    let flaky = rows.iter().filter(|row| row.flaky).count();
    let mut out = String::from("## kirby-ocpp suite\n\n");
    let _ = write!(out, "{passed} of {} scenarios passed", rows.len());
    let failed = count("failed") + count("error");
    if failed > 0 {
        let _ = write!(out, ", {failed} failed");
    }
    if flaky > 0 {
        let _ = write!(out, ", {flaky} flaky");
    }
    if count("skipped") > 0 {
        let _ = write!(out, ", {} skipped", count("skipped"));
    }
    let _ = write!(out, " ({}).\n\n", secs(duration_ms));
    out.push_str("| Scenario | Status | Time | Attempts | Message |\n");
    out.push_str("| --- | --- | --- | --- | --- |\n");
    for row in rows {
        let status = if row.flaky { "flaky" } else { row.status };
        let _ = writeln!(
            out,
            "| {} | {status} | {} | {} | {} |",
            cell(&row.name),
            secs(row.duration_ms),
            row.attempts,
            row.message.as_deref().map(cell).unwrap_or_default()
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn globs_match_star_and_question_mark() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("charge-*", "charge-session"));
        assert!(!glob_match("charge-*", "uncharge-session"));
        assert!(glob_match("*-session", "charge-session"));
        assert!(glob_match("c*e-s*n", "charge-session"));
        assert!(glob_match("h?artbeat", "heartbeat"));
        assert!(!glob_match("h?artbeat", "hartbeat"));
        assert!(glob_match("a*b*c", "aXbYbZc"));
        assert!(!glob_match("a*b*c", "aXbYbZ"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exact "));
        assert!(!glob_match("", "x"));
        assert!(glob_match("", ""));
        assert!(!glob_match("Charge", "charge"), "case-sensitive");
        assert!(glob_match("**x", "ax"));
    }

    #[test]
    fn selection_applies_filter_skip_tag_and_skip_tag_in_order() {
        let tags = strings(&["smoke", "slow"]);
        let none: Vec<String> = Vec::new();
        let sel = |filter: &[&str], skip: &[&str], tag: &[&str], skip_tag: &[&str]| {
            let (filter, skip, tag, skip_tag) = (
                strings(filter),
                strings(skip),
                strings(tag),
                strings(skip_tag),
            );
            Selection {
                filter: &filter,
                skip: &skip,
                tag: &tag,
                skip_tag: &skip_tag,
            }
            .selects("charge-session", &tags)
        };
        assert!(sel(&[], &[], &[], &[]));
        assert!(sel(&["heartbeat", "charge-*"], &[], &[], &[]));
        assert!(!sel(&["heartbeat"], &[], &[], &[]));
        assert!(
            !sel(&["charge-*"], &["*session"], &[], &[]),
            "skip wins over filter"
        );
        assert!(sel(&[], &[], &["smoke", "slow"], &[]));
        assert!(
            !sel(&[], &[], &["smoke", "other"], &[]),
            "every --tag is needed"
        );
        assert!(
            !sel(&[], &[], &[], &["slow", "other"]),
            "any --skip-tag drops"
        );
        assert!(sel(&[], &[], &[], &["other"]));
        let default = Selection::default();
        assert!(!default.is_active());
        assert!(default.selects("x", &none));
        assert!(
            Selection {
                tag: &strings(&["a"]),
                ..Selection::default()
            }
            .is_active()
        );
        assert!(
            !Selection {
                tag: &strings(&["a"]),
                ..Selection::default()
            }
            .selects("x", &none),
            "a scenario without tags fails --tag"
        );
    }

    #[test]
    fn workflow_commands_escape_the_special_characters() {
        let line = workflow_command(
            "error",
            Some("dir/a,b:c.toml"),
            "100% a,b:c",
            "line one\nline two\r100%",
        );
        // Control characters are written out before the percent escapes apply, so the
        // message cannot start a second workflow command.
        assert_eq!(
            line,
            "::error file=dir/a%2Cb%3Ac.toml,title=100%25 a%2Cb%3Ac::line one\\u{000a}line two\\u{000d}100%25"
        );
        assert!(!line.contains('\n') && !line.contains('\r'));
        assert_eq!(escape_data("a\r\nb%"), "a%0D%0Ab%25");
        assert_eq!(escape_property("a:b,c"), "a%3Ab%2Cc");
    }

    fn ci(name: &str, status: &'static str, flaky: bool, message: Option<&str>) -> CiRow {
        CiRow {
            name: name.to_owned(),
            file: Some(format!("{name}.toml")),
            status,
            duration_ms: 1234,
            attempts: if flaky { 2 } else { 1 },
            flaky,
            message: message.map(str::to_owned),
        }
    }

    #[test]
    fn annotations_cover_failures_and_flaky_passes_only() {
        let rows = vec![
            ci("ok", "passed", false, None),
            ci(
                "bad",
                "failed",
                false,
                Some("authorize - expected Accepted"),
            ),
            ci("down", "error", false, Some("no boot")),
            ci("wobbly", "passed", true, Some("heartbeat - timeout")),
            ci("later", "skipped", false, None),
        ];
        let lines = annotations(&rows);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines[0],
            "::error file=bad.toml,title=bad::authorize - expected Accepted"
        );
        assert!(lines[1].starts_with("::error file=down.toml,title=down::no boot"));
        assert!(lines[2].starts_with(
            "::warning file=wobbly.toml,title=wobbly::flaky: passed on attempt 2 after a failure"
        ));
    }

    #[test]
    fn summary_table_lists_every_row_and_neutralizes_markup() {
        let rows = vec![
            ci("ok", "passed", false, None),
            ci("a|b <x>", "failed", false, Some("step - `x` | y\nz")),
            ci("wobbly", "passed", true, Some("late")),
            ci("later", "skipped", false, None),
        ];
        let md = summary_markdown(&rows, 61_500);
        assert!(md.starts_with("## kirby-ocpp suite\n\n2 of 4 scenarios passed, 1 failed, 1 flaky, 1 skipped (61.5 s).\n"));
        assert!(md.contains("| Scenario | Status | Time | Attempts | Message |"));
        assert!(md.contains("| ok | passed | 1.2 s | 1 |  |"), "{md}");
        assert!(
            md.contains("| a\\|b &lt;x&gt; | failed | 1.2 s | 1 | step - 'x' \\| y\\u{000a}z |"),
            "{md}"
        );
        assert!(md.contains("| wobbly | flaky | 1.2 s | 2 | late |"), "{md}");
        assert!(md.contains("| later | skipped |"), "{md}");
        let long = vec![ci("long", "failed", false, Some(&"x".repeat(1000)))];
        let md = summary_markdown(&long, 1);
        assert!(md.contains(&format!("{}...", "x".repeat(CELL_LIMIT))));
        assert!(!md.contains(&"x".repeat(CELL_LIMIT + 1)));
    }
}

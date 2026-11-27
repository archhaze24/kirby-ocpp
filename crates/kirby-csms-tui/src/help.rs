//! The text of the help popup (`?`): the section of the focused pane first, then the common
//! tasks on the selected station, then the keys that work everywhere. Scrolling and drawing
//! are the shared popup of `kirby_ocpp_tui::help`.

use kirby_ocpp_tui::help::{HelpSection, row};

use crate::actions::ActionDef;
use crate::app::{App, Focus};

/// The tasks a station operator reaches for most: the OCPP actions behind them in 1.6 and
/// in 2.x, and what the row says. The key comes from the action table of the selected
/// station, so the help always shows the key that works.
const TASKS: &[(&[&str], &str)] = &[
    (
        &["RemoteStartTransaction", "RequestStartTransaction"],
        "Start a transaction",
    ),
    (
        &["RemoteStopTransaction", "RequestStopTransaction"],
        "Stop a transaction",
    ),
    (&["Reset"], "Reset the station (asks first)"),
    (&["TriggerMessage"], "Ask the station to send a message"),
    (
        &["ChangeAvailability"],
        "Set a connector or the station operative or inoperative",
    ),
    (&["UnlockConnector"], "Unlock a connector"),
];

fn key_of(catalog: &[ActionDef], names: &[&str]) -> Option<char> {
    catalog
        .iter()
        .find(|a| a.ocpp.is_some_and(|o| names.contains(&o)))
        .and_then(|a| a.key)
}

/// The sections of the help for the current screen: the focused pane, "Common tasks",
/// "General".
pub fn sections(app: &App) -> Vec<HelpSection> {
    vec![pane_section(app), tasks_section(app), general_section()]
}

fn pane_section(app: &App) -> HelpSection {
    match app.focus {
        Focus::Stations => HelpSection::new(
            "Stations",
            [
                row("↑/↓ j/k", "Select a station"),
                row("PgUp/PgDn", "A page up or down"),
                row("Home/End", "First or last station"),
                row("/", "Filter the stations by id; Esc clears the filter"),
                row("→ Tab", "Go to the details, then the log"),
                row(
                    "",
                    "A charge point connects to ws://<host>:<port>/<prefix>/<station id>. The prefix is optional and the last path segment is the station id.",
                ),
            ],
        ),
        Focus::Details => HelpSection::new(
            "Details",
            [
                row("↑/↓ j/k", "Select a transaction"),
                row("Enter", "Open the selected transaction"),
                row("Home/End", "First or last transaction"),
                row("z", "Zoom the pane to see more rows"),
                row("←", "Back to the stations"),
            ],
        ),
        Focus::Log => HelpSection::new(
            "Log",
            [
                row("↑/↓ j/k", "Select a row"),
                row("PgUp/PgDn", "A page up or down"),
                row("Home", "The oldest row"),
                row("End G", "The newest row; follow new rows"),
                row("Enter", "Open the detail of the row; c copies it"),
                row("y/Y", "Copy the row / the visible rows"),
                row("f", "Filter: all, calls, errors, from stations"),
                row("s /", "Search the log; Esc clears the search"),
                row("p", "Pause or follow"),
                row("a", "Log of the selected station or of all stations"),
            ],
        ),
    }
}

fn tasks_section(app: &App) -> HelpSection {
    let catalog = app.catalog();
    let mut lines: Vec<String> = TASKS
        .iter()
        .filter_map(|(names, text)| key_of(&catalog, names).map(|k| row(&k.to_string(), text)))
        .collect();
    // 1.6 changes and reads configuration keys, 2.x sets and gets variables.
    let (set, get, what) = if key_of(&catalog, &["SetVariables"]).is_some() {
        ("SetVariables", "GetVariables", "variables")
    } else {
        ("ChangeConfiguration", "GetConfiguration", "configuration")
    };
    if let (Some(c), Some(g)) = (key_of(&catalog, &[set]), key_of(&catalog, &[get])) {
        lines.push(row(
            &format!("{c} {g}"),
            &format!("Change / read the {what}"),
        ));
    }
    lines.push(row(
        "m i M",
        "Scripted behavior: m sets a response rule, i injects a fault, M clears them all",
    ));
    lines.push(row(
        ":",
        "Command palette: every other action (profiles, firmware, certificates, ...); type to filter",
    ));
    lines.push(row(
        "",
        "Each task opens a form: Enter sends, Esc cancels, Tab moves to the next field, ←/→ or a letter picks an option. The answer shows in the status line.",
    ));
    HelpSection::new("Common tasks", lines)
}

fn general_section() -> HelpSection {
    HelpSection::new(
        "General",
        [
            row("W", "Commands waiting for a station; x cancels one"),
            row("w", "Send a raw call: any action with a JSON payload"),
            row(
                "r",
                "Last answers from the selected station; Enter opens one",
            ),
            row("n", "Copy the station id"),
            row("F", "Forget a disconnected station (asks first)"),
            row("a", "Log of the selected station or of all stations"),
            row("←/→ Tab", "Move between the panes"),
            row(
                "z",
                "Zoom the focused pane to the whole body; z or Esc goes back",
            ),
            row("?", "This help"),
            row(
                "q",
                "Quit and stop the server; it asks first while stations are connected or commands are pending",
            ),
            row(
                "",
                "Without color (--color never or NO_COLOR): reverse video, bold and markers (+ ok, ! warning, x error).",
            ),
        ],
    )
}

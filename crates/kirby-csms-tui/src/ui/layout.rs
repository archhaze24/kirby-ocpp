//! Where the panes sit. Drawing and the keys that depend on what is visible (`Enter` on a
//! transaction row) both ask this module, so they agree on the geometry.

use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::{App, Focus};

/// Width from which the list sits left of details and log.
pub const WIDE: u16 = 100;

/// Header, body and footer of a screen of `area`.
pub fn screen(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area)
}

/// The body of a terminal of `size` (width, height).
pub fn body_of(size: (u16, u16)) -> Rect {
    let [_, body, _] = screen(Rect::new(0, 0, size.0, size.1));
    body
}

/// The rectangle of each pane; a pane that is not shown (another one is zoomed) is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panes {
    pub stations: Rect,
    pub details: Rect,
    pub log: Rect,
}

/// Rows of the station list when the terminal is short: the selected row, and the title
/// carries the count.
const SHORT_LIST: u16 = 3;
/// Body height below which the station list shrinks to [`SHORT_LIST`].
const SHORT_BODY: u16 = 26;
/// Fewest rows the log pane gets in the stacked layout (borders included).
const MIN_LOG: u16 = 4;

/// Splits `body` between the panes. A zoomed pane (the focused one) takes the whole body.
pub fn panes(app: &App, body: Rect) -> Panes {
    let none = Rect::new(body.x, body.y, 0, 0);
    if app.zoom {
        return match app.focus {
            Focus::Stations => Panes {
                stations: body,
                details: none,
                log: none,
            },
            Focus::Details => Panes {
                stations: none,
                details: body,
                log: none,
            },
            Focus::Log => Panes {
                stations: none,
                details: none,
                log: body,
            },
        };
    }
    if body.width >= WIDE {
        let list_w = (body.width * 3 / 10).clamp(24, 44);
        let [stations, right] =
            Layout::horizontal([Constraint::Length(list_w), Constraint::Min(10)]).areas(body);
        let [details, log] =
            Layout::vertical([Constraint::Percentage(60), Constraint::Min(3)]).areas(right);
        return Panes {
            stations,
            details,
            log,
        };
    }
    // Stacked: the details are the part one reads most, so they get what the list and the
    // log leave. A short terminal shrinks the list to the selected row.
    let list_h = if body.height < SHORT_BODY {
        SHORT_LIST
    } else {
        (body.height / 4).clamp(SHORT_LIST, 8)
    };
    let log_h = (body.height / 4).clamp(MIN_LOG, 12);
    let [stations, details, log] = Layout::vertical([
        Constraint::Length(list_h),
        Constraint::Min(3),
        Constraint::Length(log_h),
    ])
    .areas(body);
    Panes {
        stations,
        details,
        log,
    }
}

/// How the inside of the details pane is divided, in rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Split {
    /// Lines at the top (summary, connection, counters).
    pub head: u16,
    /// Further information lines.
    pub extras: u16,
    /// Rows of the connector table, header included (0: none).
    pub connectors: u16,
    /// Rows of the transaction table, header included (0: none).
    pub transactions: u16,
}

/// What the details pane has to show: line counts and table sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content {
    pub head: usize,
    pub extras: usize,
    pub connectors: usize,
    pub transactions: usize,
}

/// Rows of a table of `height` (header included) that hold data when it has `total` rows. A
/// table that does not fit keeps one row for its `▲` / `▼` marker.
pub fn rows_shown(height: u16, total: usize) -> usize {
    let room = usize::from(height.saturating_sub(1));
    if total <= room {
        total
    } else {
        room.saturating_sub(1)
    }
}

/// The first of `shown` rows out of `total` that keeps row `selected` in view (the list
/// scrolls only as far as the selection needs).
pub fn window_start(selected: usize, total: usize, shown: usize) -> usize {
    if shown == 0 {
        return 0;
    }
    (selected + 1)
        .saturating_sub(shown)
        .min(total.saturating_sub(shown))
}

/// Divides `height` rows of the details pane. Tables come before the extra lines; a table
/// that does not fit is cut (the transactions keep at least four rows when they have that
/// many), and side by side (`wide`) the two tables share the same rows.
pub fn split(height: u16, content: Content, wide: bool) -> Split {
    let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    let head = clamp(content.head).min(height);
    let rest = height - head;
    let table = |rows: usize| if rows == 0 { 0 } else { clamp(rows + 1) };
    let (want_c, want_t) = (table(content.connectors), table(content.transactions));
    let want = if wide {
        want_c.max(want_t)
    } else {
        want_c.saturating_add(want_t)
    };
    let extras = clamp(content.extras).min(rest - want.min(rest));
    let tables = rest - extras;
    if wide {
        return Split {
            head,
            extras,
            connectors: want_c.min(tables),
            transactions: want_t.min(tables),
        };
    }
    let (connectors, transactions) = if want_c + want_t <= tables {
        (want_c, want_t)
    } else {
        let t_min = want_t.min(4);
        let c = want_c.min(tables.saturating_sub(t_min));
        // A header with no row is of no use.
        let c = if c == 1 { 0 } else { c };
        (c, tables - c)
    };
    Split {
        head,
        extras,
        connectors,
        transactions: transactions.min(want_t),
    }
}

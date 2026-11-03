//! Drawing of the data browser (a near full-screen popup).

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::dim;
use super::overlays::{SELECTED, popup};
use crate::app::App;
use crate::browser::{Browser, Page};
use crate::text::{sanitize, truncate};

pub fn draw(f: &mut Frame<'_>, app: &App, b: &Browser, area: Rect) {
    let rect = Rect::new(
        area.x + 1.min(area.width / 4),
        area.y + 1.min(area.height / 4),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let inner = popup(f, rect, "Station data");
    if inner.height < 4 {
        return;
    }
    let w = usize::from(inner.width);
    let snap = &app.snapshot;
    let rows = b.rows(snap);

    // Page tabs with counts.
    let tabs: Vec<Span<'_>> = Page::ALL
        .iter()
        .flat_map(|p| {
            let count = match p {
                Page::Config => snap.config.len(),
                Page::LocalList => snap.local_list.total,
                Page::Profiles => snap.charging_profiles.len(),
                Page::Certificates => snap.certificates.len(),
            };
            let on = *p == b.page;
            let style = if on {
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            [
                Span::styled(
                    format!(" {} {} ({count}) ", p.index() + 1, p.title()),
                    style,
                ),
                Span::raw(" "),
            ]
        })
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(tabs)),
        Rect { height: 1, ..inner },
    );

    // Filter line.
    let filter_rect = Rect {
        y: inner.y + 1,
        height: 1,
        ..inner
    };
    if b.filtering || !b.filter.is_empty() {
        let (shown, col) = b.filter.window(w.saturating_sub(3));
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/ ", Style::new().fg(Color::Cyan)),
                Span::raw(shown),
                Span::styled(format!("   {} of {}", rows.len(), all_count(b, app)), dim()),
            ])),
            filter_rect,
        );
        if b.filtering {
            f.set_cursor_position(Position::new(
                inner.x + 2 + u16::try_from(col).unwrap_or(0),
                filter_rect.y,
            ));
        }
    } else {
        f.render_widget(
            Paragraph::new(Line::styled(truncate(&summary(b, app), w), dim())),
            filter_rect,
        );
    }

    // The list.
    let list_rect = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(4),
        ..inner
    };
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|r| {
            let style = if r.dim { dim() } else { Style::new() };
            ListItem::new(truncate(&r.text, w)).style(style)
        })
        .collect();
    let sel = b.sel[b.page.index()].min(rows.len().saturating_sub(1));
    let mut state = ListState::default().with_selected((!rows.is_empty()).then_some(sel));
    f.render_stateful_widget(
        List::new(items).highlight_style(SELECTED),
        list_rect,
        &mut state,
    );
    if rows.is_empty() {
        let text = if b.filter.is_empty() {
            "(nothing here yet)"
        } else {
            "(no match)"
        };
        f.render_widget(Paragraph::new(Line::styled(text, dim())), list_rect);
    }

    // Details of the selected row.
    let detail_rect = Rect {
        y: inner.y + inner.height - 2,
        height: 2,
        ..inner
    };
    let lines: Vec<Line<'_>> = selected_detail(b, app)
        .into_iter()
        .map(|l| Line::styled(truncate(&sanitize(&l), w), Style::new().fg(Color::Yellow)))
        .collect();
    f.render_widget(Paragraph::new(lines), detail_rect);
}

fn all_count(b: &Browser, app: &App) -> usize {
    match b.page {
        Page::Config => app.snapshot.config.len(),
        Page::LocalList => app.snapshot.local_list.entries.len(),
        Page::Profiles => app.snapshot.charging_profiles.len(),
        Page::Certificates => app.snapshot.certificates.len(),
    }
}

fn summary(b: &Browser, app: &App) -> String {
    let snap = &app.snapshot;
    match b.page {
        Page::Config => {
            let ro = snap.config.iter().filter(|i| i.readonly).count();
            format!(
                "{} keys, {ro} read-only (ro); Enter edits a writable key",
                snap.config.len()
            )
        }
        Page::LocalList => {
            let l = &snap.local_list;
            let shown = if l.total > l.entries.len() {
                format!(", showing the first {}", l.entries.len())
            } else {
                String::new()
            };
            format!("version {}, {} entries{shown}", l.version, l.total)
        }
        Page::Profiles => format!("{} installed", snap.charging_profiles.len()),
        Page::Certificates => format!(
            "SecurityProfile {}, {} certificate(s){}",
            snap.security_profile,
            snap.certificates.len(),
            if snap.certificate_request_pending {
                ", a certificate request is pending"
            } else {
                ""
            }
        ),
    }
}

fn selected_detail(b: &Browser, app: &App) -> Vec<String> {
    let snap = &app.snapshot;
    let Some(row) = b.selected_row(snap) else {
        return Vec::new();
    };
    match b.page {
        Page::Config => snap
            .config
            .get(row.index)
            .map(|i| {
                vec![
                    format!("{} = {}", i.key, i.value),
                    format!("accepts: {}", i.hint),
                ]
            })
            .unwrap_or_default(),
        Page::LocalList => snap
            .local_list
            .entries
            .get(row.index)
            .map(|e| {
                vec![format!(
                    "{}: {}, expires {}, parent {}",
                    e.id_tag,
                    e.status,
                    e.expiry_date
                        .map_or_else(|| "never".to_owned(), |d| d.to_rfc3339()),
                    e.parent_id_tag.as_deref().unwrap_or("-")
                )]
            })
            .unwrap_or_default(),
        Page::Certificates => snap
            .certificates
            .get(row.index)
            .map(|c| {
                vec![
                    format!("{}: {}", c.kind, c.subject),
                    format!(
                        "serial {}, expires {}, SHA-256 key hash {}",
                        c.serial_number,
                        c.not_after.to_rfc3339(),
                        c.hash.issuer_key_hash
                    ),
                ]
            })
            .unwrap_or_default(),
        Page::Profiles => snap
            .charging_profiles
            .get(row.index)
            .map(|p| {
                vec![
                    format!(
                        "valid {} .. {}; {} period(s) in {}",
                        p.valid_from
                            .map_or_else(|| "-".to_owned(), |d| d.to_rfc3339()),
                        p.valid_to
                            .map_or_else(|| "-".to_owned(), |d| d.to_rfc3339()),
                        p.periods,
                        p.unit
                    ),
                    format!("schedule {}", p.schedule),
                ]
            })
            .unwrap_or_default(),
    }
}

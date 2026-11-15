//! The device-model browser: the station-level variables of `Snapshot2::variables` with a
//! filter. The model only opens it and reacts to its [`BrowserEvent`]s; this file holds the
//! state, the key handling and the drawing.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirby_ocpp_core::ocpp::v2::messages::Mutability;
use kirby_ocpp_core::ocpp::v2::types::WireEnum;
use kirby_ocpp_core::station2::VariableView;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::forms::is_secret_variable;
use crate::text::{TextInput, sanitize, truncate};
use crate::ui::dim;
use crate::ui::overlays::{SELECTED, popup};

/// What a key press in the browser asks of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserEvent {
    None,
    /// Leave the browser.
    Close,
    /// Open the form that sets the value of `Snapshot2::variables[index]` (a variable that is
    /// not `ReadOnly`).
    Edit(usize),
    /// Show the detail of `Snapshot2::variables[index]` (a `ReadOnly` row).
    Show(usize),
}

/// The browser state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceBrowser {
    /// The filter text.
    pub query: TextInput,
    /// Whether keys edit the filter.
    pub filtering: bool,
    /// The selected row among the rows that match the filter.
    pub selected: usize,
}

/// `component[.instance] / variable = value (mutability)`, sanitized. Secret values are never
/// part of the text.
pub fn row_text(variable: &VariableView) -> String {
    let component = match &variable.instance {
        Some(instance) => format!("{}.{instance}", variable.component),
        None => variable.component.clone(),
    };
    let value = if is_secret_variable(variable) {
        "(hidden)"
    } else {
        variable.value.as_deref().unwrap_or("-")
    };
    sanitize(&format!(
        "{component} / {} = {value} ({})",
        variable.variable,
        variable.mutability.as_str()
    ))
}

impl DeviceBrowser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rows that match the filter, best match first, with their indices in `variables`. An
    /// empty filter keeps the snapshot order.
    pub fn rows(&self, variables: &[VariableView]) -> Vec<(usize, String)> {
        let all = variables.iter().enumerate().map(|(i, v)| (i, row_text(v)));
        let query = self.query.text().trim();
        if query.is_empty() {
            return all.collect();
        }
        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut rows: Vec<(u32, usize, String)> = all
            .filter_map(|(index, text)| {
                pattern
                    .score(Utf32Str::new(&text, &mut buf), &mut matcher)
                    .map(|score| (score, index, text))
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        rows.into_iter()
            .map(|(_, index, text)| (index, text))
            .collect()
    }

    /// Applies a key press; `variables` are the rows of the current snapshot.
    pub fn handle_key(&mut self, key: KeyEvent, variables: &[VariableView]) -> BrowserEvent {
        let typing = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let last = self.rows(variables).len().saturating_sub(1);
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.query = TextInput::default();
                    self.filtering = false;
                    self.selected = 0;
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => self.selected = (self.selected + 1).min(last),
                KeyCode::Char(c) if typing => {
                    self.query.insert_char(c);
                    self.selected = 0;
                }
                KeyCode::Backspace => {
                    self.query.backspace();
                    self.selected = 0;
                }
                KeyCode::Delete => {
                    self.query.delete();
                    self.selected = 0;
                }
                KeyCode::Left => self.query.left(),
                KeyCode::Right => self.query.right(),
                KeyCode::Home => self.query.home(),
                KeyCode::End => self.query.end(),
                _ => {}
            }
            return BrowserEvent::None;
        }
        self.selected = self.selected.min(last);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return BrowserEvent::Close,
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Enter => {
                let rows = self.rows(variables);
                if let Some((index, variable)) = rows
                    .get(self.selected)
                    .and_then(|(i, _)| variables.get(*i).map(|v| (*i, v)))
                {
                    return if variable.mutability == Mutability::ReadOnly {
                        BrowserEvent::Show(index)
                    } else {
                        BrowserEvent::Edit(index)
                    };
                }
            }
            _ => {}
        }
        BrowserEvent::None
    }
}

/// Draws the browser as a near full-screen popup over `area`.
pub fn draw(f: &mut Frame<'_>, variables: &[VariableView], b: &DeviceBrowser, area: Rect) {
    let rect = Rect::new(
        area.x + 1.min(area.width / 4),
        area.y + 1.min(area.height / 4),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let inner = popup(f, rect, "Device model: station-level variables");
    if inner.height < 3 {
        return;
    }
    let w = usize::from(inner.width);
    let rows = b.rows(variables);
    let filter_line = if b.filtering || !b.query.is_empty() {
        let (shown, col) = b.query.window(w.saturating_sub(3));
        if b.filtering {
            f.set_cursor_position(Position::new(
                inner.x + 2 + u16::try_from(col).unwrap_or(0),
                inner.y,
            ));
        }
        Line::from(vec![
            Span::styled("/ ", Style::new().fg(Color::Cyan)),
            Span::raw(shown),
            Span::styled(format!("   {} of {}", rows.len(), variables.len()), dim()),
        ])
    } else {
        Line::styled(
            truncate(
                "EVSE and connector variables: GetVariables from the CSMS",
                w,
            ),
            dim(),
        )
    };
    f.render_widget(Paragraph::new(filter_line), Rect { height: 1, ..inner });
    let list_area = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };
    if rows.is_empty() {
        let text = if variables.is_empty() {
            "no variables yet"
        } else {
            "no variable matches the filter"
        };
        f.render_widget(Paragraph::new(Line::styled(text, dim())), list_area);
        return;
    }
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|(_, text)| ListItem::new(truncate(text, w)))
        .collect();
    let mut state = ListState::default().with_selected(Some(b.selected.min(rows.len() - 1)));
    f.render_stateful_widget(
        List::new(items).highlight_style(SELECTED),
        list_area,
        &mut state,
    );
}

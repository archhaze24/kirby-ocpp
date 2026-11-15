//! The device-model browser: the station-level variables of `Snapshot2::variables` with a
//! filter. Slice S2 of M6 fills it (state, keys and drawing); the model only opens it and
//! reacts to its [`BrowserEvent`]s.

use crossterm::event::{KeyCode, KeyEvent};
use kirby_ocpp_core::station2::VariableView;

use crate::text::TextInput;

/// What a key press in the browser asks of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserEvent {
    None,
    /// Leave the browser.
    Close,
    /// Open the form that sets the value of `Snapshot2::variables[index]` (a `ReadWrite` row).
    Edit(usize),
    /// Show the detail of `Snapshot2::variables[index]` (a `ReadOnly` or `WriteOnly` row).
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

impl DeviceBrowser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a key press; `variables` are the rows of the current snapshot.
    pub fn handle_key(&mut self, key: KeyEvent, variables: &[VariableView]) -> BrowserEvent {
        let _ = variables;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => BrowserEvent::Close,
            _ => BrowserEvent::None,
        }
    }
}

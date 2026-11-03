//! The data browser: configuration keys, the local authorization list and the installed
//! charging profiles, with a fuzzy filter. The rows come from the station snapshot; edits are
//! forms and commands (`Submitted::ConfigValue`, `LocalEntry`, `Profile`), never direct writes.

use kirby_ocpp_core::station::Snapshot;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::text::{TextInput, sanitize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Config,
    LocalList,
    Profiles,
    Certificates,
}

impl Page {
    pub const ALL: [Self; 4] = [
        Self::Config,
        Self::LocalList,
        Self::Profiles,
        Self::Certificates,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Config => "Config keys",
            Self::LocalList => "Local auth list",
            Self::Profiles => "Charging profiles",
            Self::Certificates => "Certificates",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    fn shifted(self, d: isize) -> Self {
        let n = Self::ALL.len();
        Self::ALL[(self.index() + n).wrapping_add_signed(d) % n]
    }
}

/// One visible row: the index into the snapshot list it stands for, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub index: usize,
    pub text: String,
    /// Shown dimmed (read-only config keys).
    pub dim: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    pub page: Page,
    pub filter: TextInput,
    /// Typed characters go to the filter.
    pub filtering: bool,
    /// Selected row per page (an index into the *filtered* rows).
    pub sel: [usize; 4],
}

impl Browser {
    pub fn new(page: Page) -> Self {
        Self {
            page,
            filter: TextInput::default(),
            filtering: false,
            sel: [0; 4],
        }
    }

    pub fn switch_page(&mut self, d: isize) {
        self.page = self.page.shifted(d);
        self.filter = TextInput::default();
        self.filtering = false;
    }

    /// The rows of the current page that match the filter, best match first.
    pub fn rows(&self, snap: &Snapshot) -> Vec<Row> {
        let all = all_rows(self.page, snap);
        let query = self.filter.text().trim();
        if query.is_empty() {
            return all;
        }
        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, Row)> = all
            .into_iter()
            .filter_map(|row| {
                pattern
                    .score(Utf32Str::new(&row.text, &mut buf), &mut matcher)
                    .map(|s| (s, row))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.index.cmp(&b.1.index)));
        scored.into_iter().map(|(_, row)| row).collect()
    }

    /// The selected row (clamped to the visible rows).
    pub fn selected_row(&self, snap: &Snapshot) -> Option<Row> {
        let rows = self.rows(snap);
        let i = self.sel[self.page.index()].min(rows.len().saturating_sub(1));
        rows.into_iter().nth(i)
    }

    pub fn move_by(&mut self, d: isize, snap: &Snapshot) {
        let n = self.rows(snap).len();
        let sel = &mut self.sel[self.page.index()];
        *sel = if n == 0 {
            0
        } else {
            (*sel).min(n - 1).saturating_add_signed(d).min(n - 1)
        };
    }

    pub fn select_edge(&mut self, last: bool, snap: &Snapshot) {
        let n = self.rows(snap).len();
        self.sel[self.page.index()] = if last { n.saturating_sub(1) } else { 0 };
    }

    /// The filter changed: go back to the best match.
    pub fn reset_selection(&mut self) {
        self.sel[self.page.index()] = 0;
    }
}

fn all_rows(page: Page, snap: &Snapshot) -> Vec<Row> {
    match page {
        Page::Config => snap
            .config
            .iter()
            .enumerate()
            .map(|(index, item)| Row {
                index,
                text: format!(
                    "{}{} = {}",
                    sanitize(&item.key),
                    if item.readonly { " (ro)" } else { "" },
                    sanitize(&item.value)
                ),
                dim: item.readonly,
            })
            .collect(),
        Page::LocalList => snap
            .local_list
            .entries
            .iter()
            .enumerate()
            .map(|(index, e)| Row {
                index,
                text: format!(
                    "{:<20} {:<12} expires {}  parent {}",
                    sanitize(&e.id_tag),
                    e.status,
                    e.expiry_date.map_or_else(
                        || "-".to_owned(),
                        |d| d.format("%Y-%m-%d %H:%M").to_string()
                    ),
                    e.parent_id_tag
                        .as_deref()
                        .map_or_else(|| "-".to_owned(), sanitize),
                ),
                dim: false,
            })
            .collect(),
        Page::Certificates => snap
            .certificates
            .iter()
            .enumerate()
            .map(|(index, c)| Row {
                index,
                text: format!(
                    "{:<28} {}  expires {}",
                    sanitize(&c.kind),
                    sanitize(&c.subject),
                    c.not_after.format("%Y-%m-%d")
                ),
                dim: false,
            })
            .collect(),
        Page::Profiles => snap
            .charging_profiles
            .iter()
            .enumerate()
            .map(|(index, p)| Row {
                index,
                text: format!(
                    "#{} conn {} {} {} stack {}  {}",
                    p.id, p.connector, p.purpose, p.kind, p.stack_level, p.schedule
                ),
                dim: false,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use kirby_ocpp_core::StationConfig;

    use super::*;
    use crate::tests_support::snapshot_with_data;

    #[test]
    fn rows_follow_the_page_and_the_fuzzy_filter() {
        let snap = snapshot_with_data();
        let mut b = Browser::new(Page::Config);
        assert_eq!(b.rows(&snap).len(), snap.config.len());
        b.filter = TextInput::new("hrtbt");
        let rows = b.rows(&snap);
        assert!(rows[0].text.starts_with("HeartbeatInterval"), "{rows:?}");
        assert!(rows.len() < snap.config.len());
        b.filter = TextInput::new("zzzzqq");
        assert_eq!(b.rows(&snap).len(), 0);
        assert!(b.selected_row(&snap).is_none());
    }

    #[test]
    fn selection_is_clamped_and_filter_resets_on_page_switch() {
        let snap = snapshot_with_data();
        let mut b = Browser::new(Page::LocalList);
        b.move_by(10, &snap);
        assert_eq!(b.selected_row(&snap).unwrap().index, 1);
        b.move_by(-10, &snap);
        assert_eq!(b.selected_row(&snap).unwrap().index, 0);
        b.filter = TextInput::new("x");
        b.switch_page(1);
        assert_eq!(b.page, Page::Profiles);
        assert!(b.filter.is_empty());
        b.switch_page(-1);
        b.switch_page(-1);
        assert_eq!(b.page, Page::Config);
        let empty = Snapshot::initial(&StationConfig::new("ws://h/p"));
        b.move_by(1, &empty);
        assert!(b.selected_row(&empty).is_none());
    }
}

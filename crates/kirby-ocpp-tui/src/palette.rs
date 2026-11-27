//! Command palette: fuzzy search over every action.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::actions::ActionDef;
use crate::text::TextInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub query: TextInput,
    /// Indices into the action catalogue, best match first.
    pub results: Vec<usize>,
    pub selected: usize,
}

/// What the palette matches on for one action: the tab title, the label and the description.
/// Any catalogue (1.6 [`ActionDef`], the 2.x one) is turned into rows of this shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteRow<'a> {
    pub label: &'a str,
    pub desc: &'a str,
    pub tab: &'a str,
    /// Position of the task group in the unfiltered palette (smaller first, ties keep the
    /// catalogue order): connection, plug, authorize and transactions, messages, faults,
    /// scenarios, configuration and the rest.
    pub group: u8,
}

impl<'a> From<&'a ActionDef> for PaletteRow<'a> {
    fn from(a: &'a ActionDef) -> Self {
        Self {
            label: &a.label,
            desc: &a.desc,
            tab: a.tab.title(),
            group: crate::actions::palette_group(a.id),
        }
    }
}

impl Palette {
    pub fn new(catalog: &[ActionDef]) -> Self {
        let rows: Vec<PaletteRow<'_>> = catalog.iter().map(PaletteRow::from).collect();
        Self::from_rows(&rows)
    }

    /// A palette over any catalogue; `results` index into `rows`.
    pub fn from_rows(rows: &[PaletteRow<'_>]) -> Self {
        let mut p = Self {
            query: TextInput::default(),
            results: Vec::new(),
            selected: 0,
        };
        p.refresh_rows(rows);
        p
    }

    pub fn refresh(&mut self, catalog: &[ActionDef]) {
        let rows: Vec<PaletteRow<'_>> = catalog.iter().map(PaletteRow::from).collect();
        self.refresh_rows(&rows);
    }

    /// Re-ranks `rows` for the current query and selects the best match.
    pub fn refresh_rows(&mut self, rows: &[PaletteRow<'_>]) {
        self.results = rank_rows(rows, self.query.text());
        self.selected = 0;
    }

    pub fn move_by(&mut self, d: isize) {
        if !self.results.is_empty() {
            let n = self.results.len();
            self.selected = (self.selected + n).wrapping_add_signed(d) % n;
        }
    }

    pub fn chosen(&self) -> Option<usize> {
        self.results.get(self.selected).copied()
    }
}

/// Text the matcher sees for an action: tab, label and description.
pub fn haystack(a: &ActionDef) -> String {
    haystack_row(&PaletteRow::from(a))
}

/// [`haystack`] of a row: `"{tab} {label} {desc}"`.
pub fn haystack_row(r: &PaletteRow<'_>) -> String {
    format!("{} {} {}", r.tab, r.label, r.desc)
}

pub fn rank(catalog: &[ActionDef], query: &str) -> Vec<usize> {
    let rows: Vec<PaletteRow<'_>> = catalog.iter().map(PaletteRow::from).collect();
    rank_rows(&rows, query)
}

/// [`rank`] over rows.
pub fn rank_rows(rows: &[PaletteRow<'_>], query: &str) -> Vec<usize> {
    let mut ranked = rank_by(rows, query, haystack_row);
    if query.trim().is_empty() {
        // Without a query the list is ordered by task; a query ranks by match quality.
        ranked.sort_by_key(|&i| rows[i].group);
    }
    ranked
}

/// Fuzzy ranking over any catalogue: indices of the items matching `query`, best first
/// (all of them, in order, for an empty query). `haystack` gives the text an item is matched on.
pub fn rank_by<T>(items: &[T], query: &str, haystack: impl Fn(&T) -> String) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..items.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let hay = haystack(a);
            pattern
                .score(Utf32Str::new(&hay, &mut buf), &mut matcher)
                .map(|s| (s, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use kirby_ocpp_core::scenario::builtin;

    use super::*;
    use crate::actions::{ActionId, catalog};

    #[test]
    fn empty_query_lists_everything_in_order() {
        let cat = catalog(&builtin::all());
        assert_eq!(rank(&cat, "").len(), cat.len());
    }

    #[test]
    fn fuzzy_query_finds_actions_across_tabs() {
        let cat = catalog(&builtin::all());
        let top = rank(&cat, "authz")[0];
        assert_eq!(cat[top].id, ActionId::Authorize);
        let hits = rank(&cat, "firmware");
        assert!(hits.iter().any(|i| cat[*i].id == ActionId::FwStatus));
        assert_eq!(rank(&cat, "zzzzqq").len(), 0);
    }

    #[test]
    fn rows_rank_like_the_catalogue_they_come_from() {
        let cat = catalog(&builtin::all());
        let rows: Vec<PaletteRow<'_>> = cat.iter().map(PaletteRow::from).collect();
        for query in ["", "authz", "firmware", "zzzzqq"] {
            assert_eq!(rank_rows(&rows, query), rank(&cat, query), "{query}");
        }
        assert_eq!(Palette::from_rows(&rows), Palette::new(&cat));
    }

    #[test]
    fn selection_wraps() {
        let cat = catalog(&builtin::all());
        let mut p = Palette::new(&cat);
        p.move_by(-1);
        assert_eq!(p.selected, cat.len() - 1);
    }
}

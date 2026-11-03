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

impl Palette {
    pub fn new(catalog: &[ActionDef]) -> Self {
        let mut p = Self {
            query: TextInput::default(),
            results: Vec::new(),
            selected: 0,
        };
        p.refresh(catalog);
        p
    }

    pub fn refresh(&mut self, catalog: &[ActionDef]) {
        self.results = rank(catalog, self.query.text());
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
    format!("{} {} {}", a.tab.title(), a.label, a.desc)
}

pub fn rank(catalog: &[ActionDef], query: &str) -> Vec<usize> {
    rank_by(catalog, query, haystack)
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
    fn selection_wraps() {
        let cat = catalog(&builtin::all());
        let mut p = Palette::new(&cat);
        p.move_by(-1);
        assert_eq!(p.selected, cat.len() - 1);
    }
}

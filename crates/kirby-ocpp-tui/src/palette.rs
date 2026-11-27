//! Command palette: search over every action.

use crate::actions::ActionDef;
use crate::text::TextInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub query: TextInput,
    /// Indices into the action catalogue, best match first.
    pub results: Vec<usize>,
    pub selected: usize,
    /// Title of the tab that was open when the palette opened: without a query its actions
    /// come first.
    pub current_tab: String,
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
    /// A palette over the 1.6 catalogue, opened on the tab titled `current_tab`.
    pub fn new(catalog: &[ActionDef], current_tab: &str) -> Self {
        let rows: Vec<PaletteRow<'_>> = catalog.iter().map(PaletteRow::from).collect();
        Self::from_rows(&rows, current_tab)
    }

    /// A palette over any catalogue; `results` index into `rows`.
    pub fn from_rows(rows: &[PaletteRow<'_>], current_tab: &str) -> Self {
        let mut p = Self {
            query: TextInput::default(),
            results: Vec::new(),
            selected: 0,
            current_tab: current_tab.to_owned(),
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
        self.results = rank_rows(rows, self.query.text(), &self.current_tab);
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

/// Ranks the 1.6 catalogue for `query`; `current_tab` is the title of the open tab.
pub fn rank(catalog: &[ActionDef], query: &str, current_tab: &str) -> Vec<usize> {
    let rows: Vec<PaletteRow<'_>> = catalog.iter().map(PaletteRow::from).collect();
    rank_rows(&rows, query, current_tab)
}

/// [`rank`] over rows. Without a query the actions of `current_tab` come first, then the
/// rest, each part in task order; a query ranks by match quality.
pub fn rank_rows(rows: &[PaletteRow<'_>], query: &str, current_tab: &str) -> Vec<usize> {
    let mut ranked = rank_by(rows, query, haystack_row);
    if query.trim().is_empty() {
        ranked.sort_by_key(|&i| (rows[i].tab != current_tab, rows[i].group));
    }
    ranked
}

/// Ranking over any catalogue: indices of the items matching `query`, best first (all of them,
/// in order, for an empty query). `haystack` gives the text an item is matched on.
///
/// Every word of the query has to match, and a match has to be a real one, not letters
/// scattered over the text. A word matches when, from best to weakest, it:
///
/// 1. starts a word of the text (`notif` in `BootNotification`, `heart` in `Heartbeat`),
/// 2. occurs inside the text as it is,
/// 3. is a subsequence of one word of the text that starts with the same letter (`authz`
///    in `Authorize`; at least 3 letters),
/// 4. is made of the initials of consecutive words (`gv` for `Get variables`).
///
/// A match near the start of the text ranks above one further on.
pub fn rank_by<T>(items: &[T], query: &str, haystack: impl Fn(&T) -> String) -> Vec<usize> {
    let words: Vec<Vec<char>> = query
        .split_whitespace()
        .map(|w| w.chars().map(lower).collect())
        .collect();
    if words.is_empty() {
        return (0..items.len()).collect();
    }
    let mut scored: Vec<(u32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| score(&haystack(item), &words).map(|s| (s, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// One lowercase char per char, so positions stay comparable with the original text.
fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// The text as the matcher reads it.
struct Text {
    /// Lowercased characters.
    chars: Vec<char>,
    /// Where a word starts: after a non-alphanumeric char, or at a capital after a lowercase
    /// letter (`BootNotification` has two).
    starts: Vec<usize>,
    /// The runs of alphanumeric chars, as `start..end`.
    tokens: Vec<(usize, usize)>,
}

impl Text {
    fn new(text: &str) -> Self {
        let original: Vec<char> = text.chars().collect();
        let chars: Vec<char> = original.iter().map(|c| lower(*c)).collect();
        let mut starts = Vec::new();
        let mut tokens: Vec<(usize, usize)> = Vec::new();
        for (i, c) in original.iter().enumerate() {
            if !c.is_alphanumeric() {
                continue;
            }
            let prev = i.checked_sub(1).and_then(|p| original.get(p));
            let boundary = prev.is_none_or(|p| !p.is_alphanumeric());
            if boundary || (c.is_uppercase() && prev.is_some_and(|p| p.is_lowercase())) {
                starts.push(i);
            }
            match tokens.last_mut() {
                Some(last) if !boundary => last.1 = i + 1,
                _ => tokens.push((i, i + 1)),
            }
        }
        Self {
            chars,
            starts,
            tokens,
        }
    }
}

fn score(haystack: &str, words: &[Vec<char>]) -> Option<u32> {
    let text = Text::new(haystack);
    let mut total = 0;
    for w in words {
        total += word_score(&text, w)?;
    }
    Some(total)
}

/// A match at `pos` scores more the nearer the start it is.
fn near(base: u32, pos: usize) -> u32 {
    base + 90u32.saturating_sub(u32::try_from(pos).unwrap_or(90))
}

fn word_score(text: &Text, w: &[char]) -> Option<u32> {
    if let Some(&s) = text
        .starts
        .iter()
        .find(|&&s| text.chars.get(s..).is_some_and(|rest| rest.starts_with(w)))
    {
        return Some(near(1000, s));
    }
    if let Some(pos) = text.chars.windows(w.len()).position(|win| win == w) {
        return Some(near(600, pos));
    }
    if w.len() >= 3
        && let Some(&(s, _)) = text.tokens.iter().find(|&&(s, e)| {
            text.chars.get(s) == w.first()
                && text
                    .chars
                    .get(s..e)
                    .is_some_and(|token| is_subsequence(w, token))
        })
    {
        return Some(near(300, s));
    }
    if w.len() >= 2 {
        let initials: Vec<char> = text
            .starts
            .iter()
            .filter_map(|&s| text.chars.get(s).copied())
            .collect();
        if is_subsequence(w, &initials) {
            let first = text
                .starts
                .iter()
                .find(|&&s| text.chars.get(s) == w.first())
                .copied()
                .unwrap_or(0);
            return Some(near(200, first));
        }
    }
    None
}

/// Whether `needle` is contained in `hay` in order (not necessarily next to each other).
fn is_subsequence(needle: &[char], hay: &[char]) -> bool {
    let mut it = hay.iter();
    needle.iter().all(|n| it.any(|h| h == n))
}

#[cfg(test)]
mod tests {
    use kirby_ocpp_core::scenario::builtin;

    use super::*;
    use crate::actions::{ActionId, Tab, catalog};

    fn titles(cat: &[ActionDef], hits: &[usize]) -> Vec<String> {
        hits.iter().map(|&i| cat[i].label.clone()).collect()
    }

    #[test]
    fn empty_query_lists_everything_in_order() {
        let cat = catalog(&builtin::all());
        assert_eq!(rank(&cat, "", "Station").len(), cat.len());
    }

    #[test]
    fn fuzzy_query_finds_actions_across_tabs() {
        let cat = catalog(&builtin::all());
        let top = rank(&cat, "authz", "Station")[0];
        assert_eq!(cat[top].id, ActionId::Authorize);
        let hits = rank(&cat, "firmware", "Station");
        assert!(hits.iter().any(|i| cat[*i].id == ActionId::FwStatus));
        assert_eq!(rank(&cat, "zzzzqq", "Station").len(), 0);
    }

    #[test]
    fn rows_rank_like_the_catalogue_they_come_from() {
        let cat = catalog(&builtin::all());
        let rows: Vec<PaletteRow<'_>> = cat.iter().map(PaletteRow::from).collect();
        for query in ["", "authz", "firmware", "zzzzqq"] {
            assert_eq!(
                rank_rows(&rows, query, "Station"),
                rank(&cat, query, "Station"),
                "{query}"
            );
        }
        assert_eq!(
            Palette::from_rows(&rows, "Station"),
            Palette::new(&cat, "Station")
        );
    }

    #[test]
    fn selection_wraps() {
        let cat = catalog(&builtin::all());
        let mut p = Palette::new(&cat, "Station");
        p.move_by(-1);
        assert_eq!(p.selected, cat.len() - 1);
    }

    #[test]
    fn without_a_query_the_current_tab_comes_first_then_the_rest_by_task() {
        let cat = catalog(&builtin::all());
        for tab in [Tab::Connector, Tab::Transaction, Tab::Data] {
            let order = rank(&cat, "", tab.title());
            let own = cat.iter().filter(|a| a.tab == tab).count();
            assert!(
                order[..own].iter().all(|&i| cat[i].tab == tab),
                "{tab:?}: {:?}",
                titles(&cat, &order[..own + 2])
            );
            assert!(order[own..].iter().all(|&i| cat[i].tab != tab));
        }
        // The Transaction tab's own actions keep the task order of the whole palette.
        let order = rank(&cat, "", "Transaction");
        let pos = |id: ActionId| order.iter().position(|&i| cat[i].id == id).unwrap();
        assert!(pos(ActionId::Connect) > pos(ActionId::MeterCustom));
        assert!(pos(ActionId::Connect) < pos(ActionId::PlugToggle));
        assert!(pos(ActionId::PlugToggle) < pos(ActionId::FaultToggle));
    }

    #[test]
    fn a_palette_ranks_for_its_tab_after_every_refresh() {
        let cat = catalog(&builtin::all());
        let mut p = Palette::new(&cat, "Data");
        assert_eq!(cat[p.results[0]].tab, Tab::Data);
        p.query.insert_str("hea");
        p.refresh(&cat);
        p.query.backspace();
        p.query.backspace();
        p.query.backspace();
        p.refresh(&cat);
        assert_eq!(cat[p.results[0]].tab, Tab::Data, "an empty query again");
    }

    #[test]
    fn weak_scattered_matches_are_dropped() {
        let hay = |items: &[&str], q: &str| rank_by(items, q, |s| (*s).to_owned());
        let items = [
            "Get variables: Read device model variables of the station.",
            "Change transaction tariff: Set the tariff of a running transaction.",
            "Get base report: Ask for the report on the station.",
        ];
        assert_eq!(hay(&items, "tariff"), [1]);
        assert_eq!(hay(&items, "variables"), [0]);
    }

    #[test]
    fn prefix_and_contiguous_matches_beat_scattered_ones() {
        let hay = |items: &[&str], q: &str| rank_by(items, q, |s| (*s).to_owned());
        let items = [
            "Send MeterValues",
            "Reserve now with a remote token",
            "Remote start: start a transaction",
        ];
        assert_eq!(hay(&items, "remote")[0], 2, "word start near the front");
        assert_eq!(hay(&items, "values"), [0], "a word start inside CamelCase");
        assert_eq!(
            hay(&items, "rtm"),
            Vec::<usize>::new(),
            "no word has r..t..m"
        );
        // Letters, initials and several words.
        assert_eq!(hay(&items, "mvals"), [0]);
        assert_eq!(hay(&items, "rn"), [1]);
        assert_eq!(hay(&items, "remote start"), [2]);
    }

    #[test]
    fn every_word_of_the_query_has_to_match() {
        let cat = catalog(&builtin::all());
        assert_eq!(rank(&cat, "boot zzzzqq", "Station"), Vec::<usize>::new());
        let hits = rank(&cat, "send heartbeat", "Station");
        assert_eq!(cat[hits[0]].id, ActionId::Heartbeat);
    }
}

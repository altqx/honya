//! A query-and-filter list: the command bar, the Reader's jump list, the
//! Refine screen's slash and mention popups.
//!
//! These are four near-identical things drawn four ways today. Unifying them
//! also gives the aggressive key remap its safety net: anything that can be
//! done has a name here, so a binding a user cannot remember is still one
//! search away.
//!
//! Matching is subsequence-based rather than substring, so `ocv` finds "open
//! **c**hapter in **v**olume". Scoring prefers matches that start a word and
//! matches that run consecutively, because those are the ones a person meant.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::list::{self, ListState};
use super::zones::{ZoneId, ZoneKind};
use crate::ui::glyphs;
use crate::ui::input::caret_halves;
use crate::ui::text::truncate_cols;

/// Score awarded for a match that begins a word.
const WORD_START_BONUS: i32 = 12;
/// Score awarded per character of the current consecutive run.
///
/// Deliberately multiplied by the run length rather than flat. A flat bonus
/// loses to word-start bonuses in the pathological case where every character
/// begins a word — matching "open" against "o p e n" collects four word-start
/// bonuses and beats the exact run in "open chapter", which is plainly wrong.
/// A run of four is much stronger evidence than four isolated hits, so it
/// should score much higher, not merely a little.
const CONSECUTIVE_BONUS: i32 = 8;
/// Penalty per character skipped between matches.
const GAP_PENALTY: i32 = 1;
/// Bonus for matching the very first character.
const LEADING_BONUS: i32 = 6;

/// One candidate in a picker.
#[derive(Debug, Clone)]
pub struct Item {
    /// What is matched against and shown.
    pub label: String,
    /// Dimmed text after the label — a group name, a chapter number.
    pub detail: Option<String>,
    /// The binding this item also has, shown right-aligned.
    pub accel: Option<String>,
}

impl Item {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            accel: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn accel(mut self, accel: impl Into<String>) -> Self {
        self.accel = Some(accel.into());
        self
    }
}

/// How well `query` matches `candidate`, and where it matched.
///
/// `None` when the query is not a subsequence of the candidate. An empty query
/// matches everything with a score of zero, so an unfiltered picker keeps its
/// caller's ordering.
pub fn score(query: &str, candidate: &str) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let hay: Vec<char> = candidate.chars().collect();
    let needle: Vec<char> = query.chars().collect();

    let mut positions = Vec::with_capacity(needle.len());
    let mut total = 0i32;
    let mut hay_i = 0usize;
    let mut last_match: Option<usize> = None;
    let mut streak = 0i32;

    for &want in &needle {
        let want_lower = want.to_lowercase().next().unwrap_or(want);
        let found = (hay_i..hay.len()).find(|&i| {
            hay[i]
                .to_lowercase()
                .next()
                .unwrap_or(hay[i])
                == want_lower
        })?;

        let mut s = 1;
        if found == 0 {
            s += LEADING_BONUS;
        }
        if found > 0 && is_boundary(hay[found - 1]) {
            s += WORD_START_BONUS;
        }
        match last_match {
            Some(prev) if found == prev + 1 => {
                streak += 1;
                s += CONSECUTIVE_BONUS * streak;
            }
            Some(prev) => {
                streak = 0;
                s -= GAP_PENALTY * (found - prev - 1).min(16) as i32;
            }
            None => {}
        }
        total += s;
        positions.push(found);
        last_match = Some(found);
        hay_i = found + 1;
    }
    // A shorter candidate matching the same query is the better hit.
    total -= (hay.len() / 8) as i32;
    Some((total, positions))
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '_' | '/' | '.' | ':' | '·' | '(' | '[')
}

/// Filter and rank `items` against `query`, best first.
///
/// Returns indices into `items`, so the caller keeps ownership and the picker
/// never copies the whole catalogue on every keystroke.
pub fn filter(query: &str, items: &[Item]) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            // The detail is searchable too, at a discount, so "vol 3" finds a
            // chapter whose detail names the volume.
            let direct = score(query, &item.label);
            let via_detail = item
                .detail
                .as_deref()
                .and_then(|d| score(query, d))
                .map(|(s, _)| s - 20);
            let best = match (direct, via_detail) {
                (Some((a, _)), Some(b)) => a.max(b),
                (Some((a, _)), None) => a,
                (None, Some(b)) => b,
                (None, None) => return None,
            };
            Some((best, i))
        })
        .collect();
    // Stable by original order within equal scores, so an unfiltered picker
    // shows the caller's ordering rather than an arbitrary one.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// The picker's own state.
#[derive(Debug, Default, Clone)]
pub struct PickerState {
    pub query: String,
    pub cursor: usize,
    pub list: ListState,
}

impl PickerState {
    pub fn new() -> Self {
        let mut s = Self::default();
        s.list.select(Some(0));
        s
    }

    /// Reset the selection to the best match. Called after the query changes,
    /// since keeping an index into a list that has just been re-ranked would
    /// leave the cursor on an unrelated item.
    pub fn on_query_changed(&mut self) {
        self.list.select(Some(0));
    }
}

/// Render the query line and the filtered list.
///
/// Returns the filtered indices, so the caller can map the selection back to
/// its own catalogue.
pub fn render(
    ui: &mut Ui,
    area: Rect,
    state: &mut PickerState,
    items: &[Item],
    placeholder: &str,
) -> Vec<usize> {
    let matches = filter(&state.query, items);
    if area.width == 0 || area.height == 0 {
        return matches;
    }

    // Query line, a rule, then the list.
    let query_rect = Rect {
        height: 1,
        ..area
    };
    render_query(ui, query_rect, state, placeholder, matches.len());

    if area.height < 3 {
        return matches;
    }
    let rule_rect = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    super::card::rule(ui, rule_rect);

    let list_rect = Rect {
        y: area.y + 2,
        height: area.height - 2,
        ..area
    };
    if matches.is_empty() {
        ui.text(
            Rect {
                x: list_rect.x + 1,
                height: 1,
                ..list_rect
            },
            "no matches",
            Style::default().fg(ui.theme.ink_faint).bg(ui.surface()),
        );
        return matches;
    }

    // Lifted out of the closure: it only needs colours, and borrowing `ui`
    // inside would conflict with the mutable borrow `list::render` takes.
    let styles = RowStyles::of(ui);
    let query = state.query.clone();
    list::render(
        ui,
        list_rect,
        &mut state.list,
        matches.len(),
        list::Opts {
            rail: true,
            scrollbar: true,
            kind: ZoneKind::Row,
            id_base: 0,
        },
        |i| {
            let item = &items[matches[i]];
            list::Row::new(item_line(styles, item, &query, list_rect.width))
        },
    );
    matches
}

fn render_query(
    ui: &mut Ui,
    rect: Rect,
    state: &PickerState,
    placeholder: &str,
    count: usize,
) {
    let bg = ui.surface();
    ui.fill(rect, Style::default().bg(bg));
    let tally = format!(" {count} ");
    let tally_cols = tally.chars().count() as u16;
    let field_w = rect.width.saturating_sub(tally_cols + 2);

    let prompt = Style::default()
        .fg(ui.theme.accent)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let text = Style::default().fg(ui.theme.ink).bg(bg);

    let mut spans = vec![Span::styled(
        format!("{} ", glyphs::CHEVRON_RIGHT.as_str()),
        prompt,
    )];
    if state.query.is_empty() {
        spans.push(Span::styled(
            glyphs::ACCENT_RAIL.as_str().to_string(),
            Style::default().fg(ui.theme.stream_cursor).bg(bg),
        ));
        spans.push(Span::styled(
            truncate_cols(placeholder, field_w.saturating_sub(3) as usize),
            Style::default().fg(ui.theme.ink_faint).bg(bg),
        ));
    } else {
        let (before, after) =
            caret_halves(&state.query, state.cursor, field_w.saturating_sub(2) as usize);
        spans.push(Span::styled(before, text));
        spans.push(Span::styled(
            glyphs::ACCENT_RAIL.as_str().to_string(),
            Style::default().fg(ui.theme.stream_cursor).bg(bg),
        ));
        spans.push(Span::styled(after, text));
    }
    ui.line(rect, Line::from(spans), Style::default().bg(bg));

    // Match count, right-aligned.
    if rect.width > tally_cols {
        ui.text(
            Rect {
                x: rect.x + rect.width - tally_cols,
                width: tally_cols,
                height: 1,
                ..rect
            },
            tally,
            Style::default().fg(ui.theme.ink_faint).bg(bg),
        );
    }
}

/// The three styles a picker row needs, copied out of the theme so a row can be
/// built without holding a borrow on the render context.
#[derive(Debug, Clone, Copy)]
struct RowStyles {
    hit: Style,
    plain: Style,
    dim: Style,
}

impl RowStyles {
    fn of(ui: &Ui) -> Self {
        Self {
            hit: Style::default()
                .fg(ui.theme.accent)
                .add_modifier(Modifier::BOLD),
            plain: Style::default().fg(ui.theme.ink),
            dim: Style::default().fg(ui.theme.ink_faint),
        }
    }
}

/// One row: label with matched characters lifted, then detail, then accel.
fn item_line(styles: RowStyles, item: &Item, query: &str, width: u16) -> Line<'static> {
    let RowStyles { hit, plain, dim } = styles;

    let positions = score(query, &item.label)
        .map(|(_, p)| p)
        .unwrap_or_default();

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, ch) in item.label.chars().enumerate() {
        let style = if positions.contains(&i) { hit } else { plain };
        spans.push(Span::styled(ch.to_string(), style));
    }
    if let Some(d) = &item.detail {
        spans.push(Span::styled(format!("  {d}"), dim));
    }
    if let Some(a) = &item.accel {
        let used: usize = spans
            .iter()
            .map(|s| crate::ui::text::col_width(s.content.as_ref()))
            .sum();
        let want = a.chars().count() + 2;
        if used + want < width as usize {
            let gap = width as usize - used - want;
            spans.push(Span::styled(" ".repeat(gap), dim));
            spans.push(Span::styled(a.clone(), dim));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item> {
        vec![
            Item::new("Open chapter").detail("reader").accel("enter"),
            Item::new("Open project").detail("shelf"),
            Item::new("Close volume"),
            Item::new("Export volume").detail("project").accel("x"),
            Item::new("Pause run").detail("translate"),
        ]
    }

    #[test]
    fn an_empty_query_keeps_every_item_in_the_callers_order() {
        let its = items();
        let got = filter("", &its);
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn matching_is_subsequence_not_substring() {
        // "ocv" should find "Open Chapter" style targets, which no substring
        // search would.
        assert!(score("opch", "Open chapter").is_some());
        assert!(score("ozz", "Open chapter").is_none());
    }

    #[test]
    fn matching_ignores_case_in_both_directions() {
        assert!(score("OPEN", "open chapter").is_some());
        assert!(score("open", "OPEN CHAPTER").is_some());
    }

    #[test]
    fn a_word_start_match_outranks_one_buried_mid_word() {
        // "ev" begins two words in "Export volume" but is buried in the other.
        let starts = score("ev", "Export volume").unwrap().0;
        let buried = score("ev", "achieve vast").unwrap().0;
        assert!(
            starts > buried,
            "word-start {starts} should beat buried {buried}"
        );
    }

    #[test]
    fn consecutive_matches_outrank_scattered_ones() {
        // The pathological case: every character of "o p e n" begins a word, so
        // a flat consecutive bonus would lose to four word-start bonuses.
        let run = score("open", "open chapter").unwrap().0;
        let scattered = score("open", "o p e n").unwrap().0;
        assert!(run > scattered, "run {run} should beat scattered {scattered}");

        // And the ordinary case.
        let direct = score("vol", "volume").unwrap().0;
        let spread = score("vol", "very odd label").unwrap().0;
        assert!(direct > spread, "direct {direct} should beat spread {spread}");
    }

    #[test]
    fn a_longer_run_scores_disproportionately_better() {
        // Two characters of run is better than one; four is much better than
        // two. A flat bonus would make these differences merely linear.
        let two = score("op", "open").unwrap().0;
        let three = score("ope", "open").unwrap().0;
        let four = score("open", "open").unwrap().0;
        assert!(three - two > two, "run growth should accelerate");
        assert!(four - three > three - two, "and keep accelerating");
    }

    #[test]
    fn the_shorter_of_two_equal_matches_wins() {
        let short = score("vol", "volume").unwrap().0;
        let long = score("vol", "volume of a very long chapter title here").unwrap().0;
        assert!(short > long, "short {short} should beat long {long}");
    }

    #[test]
    fn filtering_ranks_the_obvious_answer_first() {
        let its = items();
        let got = filter("export", &its);
        assert_eq!(its[got[0]].label, "Export volume");
    }

    #[test]
    fn a_detail_is_searchable_but_at_a_discount() {
        let its = items();
        // "translate" only appears in a detail.
        let got = filter("translate", &its);
        assert!(!got.is_empty(), "detail should be searchable");
        assert_eq!(its[got[0]].label, "Pause run");

        // But a label match beats a detail match for the same query.
        let by_label = filter("open", &its);
        assert!(its[by_label[0]].label.starts_with("Open"));
    }

    #[test]
    fn a_query_matching_nothing_returns_nothing() {
        let its = items();
        assert!(filter("zzzzz", &its).is_empty());
    }

    #[test]
    fn match_positions_point_at_real_characters() {
        let (_, positions) = score("oc", "Open chapter").unwrap();
        let chars: Vec<char> = "Open chapter".chars().collect();
        assert_eq!(positions.len(), 2);
        for (p, want) in positions.iter().zip(['o', 'c']) {
            assert_eq!(
                chars[*p].to_ascii_lowercase(),
                want,
                "position {p} does not hold {want}"
            );
        }
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "positions must be strictly increasing"
        );
    }

    #[test]
    fn scoring_handles_multibyte_text_without_slicing_mid_character() {
        // honya's own content is Japanese and Thai; a byte-indexed matcher
        // would panic here.
        assert!(score("ほん", "ほんや 本屋").is_some());
        assert!(score("本", "ほんや 本屋").is_some());
        let (_, p) = score("本屋", "ほんや 本屋").unwrap();
        assert_eq!(p.len(), 2);
        assert!(score("กู", "คำว่า กู").is_some());
    }

    #[test]
    fn changing_the_query_resets_the_selection_to_the_best_match() {
        let mut st = PickerState::new();
        st.list.select(Some(4));
        st.query = "ex".into();
        st.on_query_changed();
        assert_eq!(
            st.list.selected(),
            Some(0),
            "a re-ranked list must not keep a stale index"
        );
    }
}

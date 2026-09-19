//! A segmented control: the tab bar, the Lexicon's sections, Settings'
//! categories.
//!
//! Three places in the app draw a row of mutually exclusive choices, each in its
//! own idiom — coloured text, bracketed labels, a divider-separated run. One
//! component means they agree, and means each segment registers its own
//! rectangle instead of the bar handing back a parallel array of rects for
//! somebody else to hit-test.
//!
//! The active segment is a filled pill rather than differently coloured text.
//! Colour alone has to survive whatever the palette does to it; a fill does not.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::style;
use super::zones::{ZoneId, ZoneKind};
use crate::ui::text::{col_width, truncate_cols};

/// One choice in the control.
#[derive(Debug, Clone)]
pub struct Segment {
    pub label: String,
    /// A secondary mark shown before the label when there is room — honya's CJK
    /// tab glyphs live here, so they enrich the bar without being the only way
    /// to tell one tab from another.
    pub mark: Option<String>,
    /// Shown after the label, e.g. an item count.
    pub badge: Option<String>,
    pub disabled: bool,
}

impl Segment {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            mark: None,
            badge: None,
            disabled: false,
        }
    }

    pub fn mark(mut self, mark: impl Into<String>) -> Self {
        self.mark = Some(mark.into());
        self
    }

    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    pub fn disabled(mut self, yes: bool) -> Self {
        self.disabled = yes;
        self
    }

    /// Columns wanted at a given level of detail.
    fn width(&self, marks: bool, badges: bool) -> u16 {
        let mut w = col_width(&self.label) + 2;
        if marks && let Some(m) = &self.mark {
            w += col_width(m) + 1;
        }
        if badges && let Some(b) = &self.badge {
            w += col_width(b) + 1;
        }
        w as u16
    }
}

/// How much of each segment is drawn. The bar steps down through these until
/// the row fits, so a narrow terminal loses decoration before it loses labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    Full,
    NoBadges,
    LabelsOnly,
}

/// A row of mutually exclusive choices.
pub struct SegmentedControl<'a> {
    pub segments: &'a [Segment],
    pub active: usize,
    /// Zone kind segments register under, so two controls on one screen differ.
    pub kind: ZoneKind,
    pub id_base: u32,
    /// Columns between segments.
    pub gap: u16,
}

impl<'a> SegmentedControl<'a> {
    pub fn new(segments: &'a [Segment], active: usize) -> Self {
        Self {
            segments,
            active,
            kind: ZoneKind::Segment,
            id_base: 0,
            gap: 1,
        }
    }

    pub fn ids(mut self, kind: ZoneKind, base: u32) -> Self {
        self.kind = kind;
        self.id_base = base;
        self
    }

    fn id_for(&self, i: usize) -> ZoneId {
        ZoneId::new(self.kind, self.id_base + i as u32)
    }

    fn total(&self, detail: Detail) -> u16 {
        if self.segments.is_empty() {
            return 0;
        }
        let (marks, badges) = match detail {
            Detail::Full => (true, true),
            Detail::NoBadges => (true, false),
            Detail::LabelsOnly => (false, false),
        };
        self.segments
            .iter()
            .map(|s| s.width(marks, badges))
            .sum::<u16>()
            + self.gap * (self.segments.len() as u16 - 1)
    }

    /// The richest detail level that fits in `cols`.
    fn detail_for(&self, cols: u16) -> Detail {
        for d in [Detail::Full, Detail::NoBadges, Detail::LabelsOnly] {
            if self.total(d) <= cols {
                return d;
            }
        }
        Detail::LabelsOnly
    }

    pub fn render(&self, ui: &mut Ui, area: Rect) {
        if area.width == 0 || area.height == 0 || self.segments.is_empty() {
            return;
        }
        let row = Rect {
            height: 1,
            ..area
        };
        ui.fill(row, Style::default().bg(ui.theme.bg));

        let detail = self.detail_for(row.width);
        let (marks, badges) = match detail {
            Detail::Full => (true, true),
            Detail::NoBadges => (true, false),
            Detail::LabelsOnly => (false, false),
        };

        let right = row.x + row.width;
        let mut x = row.x;
        for (i, seg) in self.segments.iter().enumerate() {
            if x >= right {
                break;
            }
            let want = seg.width(marks, badges);
            let w = want.min(right - x);
            let rect = Rect {
                x,
                y: row.y,
                width: w,
                height: 1,
            };
            let active = i == self.active;
            let st = if seg.disabled {
                ui.hit_only(rect, self.id_for(i)).with_disabled(true)
            } else {
                ui.interactive(rect, self.id_for(i), active)
            };

            let base = if active && !seg.disabled {
                style::filled(ui.theme)
            } else if seg.disabled {
                Style::default()
                    .fg(ui.theme.ink_faint)
                    .bg(ui.theme.bg)
                    .add_modifier(Modifier::DIM)
            } else {
                style::row(st.with_hover(st.hovered), ui.theme)
            };

            let mut spans = vec![Span::styled(" ", base)];
            let mut used = 1u16;
            if marks
                && let Some(m) = &seg.mark
            {
                let mc = col_width(m) as u16 + 1;
                if used + mc < w {
                    spans.push(Span::styled(
                        format!("{m} "),
                        if active {
                            base
                        } else {
                            base.fg(ui.theme.ink_faint)
                        },
                    ));
                    used += mc;
                }
            }
            let label_budget = w.saturating_sub(used + 1) as usize;
            let label = truncate_cols(&seg.label, label_budget);
            used += col_width(&label) as u16;
            spans.push(Span::styled(label, base));

            if badges
                && let Some(b) = &seg.badge
            {
                let bc = col_width(b) as u16 + 1;
                if used + bc < w {
                    spans.push(Span::styled(
                        format!(" {b}"),
                        if active {
                            base
                        } else {
                            base.fg(ui.theme.ink_faint)
                        },
                    ));
                    used += bc;
                }
            }
            if used < w {
                spans.push(Span::styled(
                    " ".repeat((w - used) as usize),
                    base,
                ));
            }

            ui.line(rect, Line::from(spans), base);
            x = x.saturating_add(w + self.gap);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ThemeId;
    use crate::ui::kit::focus::{Focus, Hover};
    use crate::ui::kit::tokens::Metrics;
    use crate::ui::kit::zones::Zones;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn segs() -> Vec<Segment> {
        vec![
            Segment::new("Shelf").mark("書架"),
            Segment::new("Project").mark("棚").badge("12"),
            Segment::new("Translate").mark("訳"),
            Segment::new("Reader").mark("読"),
            Segment::new("Lexicon").mark("辞"),
            Segment::new("Refine").mark("推"),
        ]
    }

    fn paint(w: u16, active: usize, segments: &[Segment]) -> (String, Zones) {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let mut term = Terminal::new(TestBackend::new(w, 1)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: 1,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            SegmentedControl::new(segments, active).render(&mut ui, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = (0..w).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        (text, zones)
    }

    #[test]
    fn every_segment_registers_a_zone_that_hit_tests_back_to_it() {
        let segments = segs();
        let (_, zones) = paint(120, 0, &segments);
        for i in 0..segments.len() {
            let r = zones
                .rect_of(ZoneId::segment(i))
                .unwrap_or_else(|| panic!("segment {i} not registered"));
            let cx = r.x + r.width / 2;
            assert_eq!(zones.at(cx, 0), Some(ZoneId::segment(i)), "segment {i}");
        }
    }

    #[test]
    fn segments_never_overlap_or_leave_the_bar() {
        let segments = segs();
        for w in [40u16, 60, 80, 120, 200] {
            let (_, zones) = paint(w, 2, &segments);
            let mut rects: Vec<Rect> = (0..segments.len())
                .filter_map(|i| zones.rect_of(ZoneId::segment(i)))
                .collect();
            rects.sort_by_key(|r| r.x);
            for pair in rects.windows(2) {
                assert!(
                    pair[0].x + pair[0].width <= pair[1].x,
                    "at {w}: {:?} overlaps {:?}",
                    pair[0],
                    pair[1]
                );
            }
            for r in &rects {
                assert!(r.x + r.width <= w, "at {w}: {r:?} ran off the bar");
            }
        }
    }

    #[test]
    fn labels_survive_when_the_bar_is_too_narrow_for_decoration() {
        let segments = segs();
        // Wide: marks and the badge are all present. Asserted one character at
        // a time because a wide glyph occupies two cells and the reconstruction
        // below reads cells, so "書架" comes back with the continuation cell
        // between its halves.
        let (wide, _) = paint(140, 0, &segments);
        assert!(wide.contains('書'), "wide bar keeps its marks: {wide:?}");
        assert!(wide.contains('棚'), "wide bar keeps its marks: {wide:?}");
        assert!(wide.contains("12"), "wide bar keeps its badge: {wide:?}");

        // Narrow: decoration goes first, the names stay readable.
        let (narrow, _) = paint(58, 0, &segments);
        assert!(narrow.contains("Shelf"), "got {narrow:?}");
        assert!(narrow.contains("Refine"), "got {narrow:?}");
    }

    #[test]
    fn a_disabled_segment_is_clickable_but_unfocusable() {
        let segments = vec![
            Segment::new("Agents"),
            Segment::new("Account").disabled(true),
        ];
        let (_, zones) = paint(40, 0, &segments);
        assert!(zones.contains(ZoneId::segment(1)));
        assert!(
            !zones.tab_order().any(|z| z == ZoneId::segment(1)),
            "a disabled segment must not take focus"
        );
    }

    #[test]
    fn an_empty_control_draws_nothing() {
        let (_, zones) = paint(40, 0, &[]);
        assert!(zones.is_empty());
    }

    #[test]
    fn two_controls_on_one_screen_keep_separate_ids() {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let segments = vec![Segment::new("A"), Segment::new("B")];
        let mut term = Terminal::new(TestBackend::new(40, 2)).unwrap();
        term.draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 40,
                height: 2,
            };
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            SegmentedControl::new(&segments, 0).render(
                &mut ui,
                Rect {
                    height: 1,
                    ..area
                },
            );
            SegmentedControl::new(&segments, 1).ids(ZoneKind::Segment, 100).render(
                &mut ui,
                Rect {
                    y: 1,
                    height: 1,
                    ..area
                },
            );
        })
        .unwrap();
        assert_eq!(zones.at(1, 0), Some(ZoneId::segment(0)));
        assert_eq!(zones.at(1, 1), Some(ZoneId::segment(100)));
    }
}

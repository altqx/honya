//! A table with clickable, sortable headers and columns that give way in order.
//!
//! The Lexicon hand-builds two tables and re-derives its column widths with a
//! bespoke function per table. Here a column declares how it wants to be sized
//! and what priority it has, and the table resolves the layout — which means the
//! behaviour at 60 columns is a property of the declaration rather than of
//! whoever wrote that particular renderer.
//!
//! Sorting lives here too, because a header you can click is the obvious
//! affordance and the current tables have none.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::list::{self, ListState};
use super::zones::{ZoneId, ZoneKind};
use crate::ui::glyphs;
use crate::ui::text::{col_width, pad_to_cols, truncate_cols};

/// How a column claims width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// Exactly this many columns, never more or less.
    Fixed(u16),
    /// At least this many; shares the slack with other `Flex` columns by weight.
    Flex { min: u16, weight: u16 },
}

/// Which way a column's contents sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Right,
}

/// One column.
#[derive(Debug, Clone)]
pub struct Column {
    pub title: String,
    pub width: Width,
    pub align: Align,
    /// Whether clicking the header sorts by this column.
    pub sortable: bool,
    /// Columns with a lower priority are dropped first when space runs out.
    /// The highest-priority column is never dropped.
    pub priority: u8,
    /// Ink for this column's cells. A column that means something different
    /// from its neighbours — a translation beside its source — says so here
    /// rather than the caller styling every cell it builds. Background is left
    /// alone so the row's selection still shows through.
    pub tint: Option<Color>,
}

impl Column {
    pub fn new(title: impl Into<String>, width: Width) -> Self {
        Self {
            title: title.into(),
            width,
            align: Align::Left,
            sortable: true,
            priority: 100,
            tint: None,
        }
    }

    pub fn tint(mut self, color: Color) -> Self {
        self.tint = Some(color);
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    pub fn fixed_sort(mut self, sortable: bool) -> Self {
        self.sortable = sortable;
        self
    }

    /// Lower drops first. The default is 100; give the column that carries the
    /// row's identity a higher number.
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    fn min_width(&self) -> u16 {
        match self.width {
            Width::Fixed(w) => w,
            Width::Flex { min, .. } => min,
        }
    }
}

/// Which column a table is sorted by, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sort {
    pub column: usize,
    pub descending: bool,
}

impl Sort {
    /// Click a header: sorting by a new column starts ascending; clicking the
    /// current column flips direction. Anything else would make the second
    /// click on a header do nothing visible.
    pub fn toggled(self, column: usize) -> Self {
        if self.column == column {
            Self {
                column,
                descending: !self.descending,
            }
        } else {
            Self {
                column,
                descending: false,
            }
        }
    }
}

/// Columns between adjacent cells.
const CELL_GAP: u16 = 1;

/// Resolve which columns are shown and how wide each is.
///
/// Returns `(index, width)` pairs in display order. Columns are dropped by
/// ascending priority until the rest fit; the highest-priority column always
/// survives, however narrow the table gets.
pub fn layout(columns: &[Column], width: u16) -> Vec<(usize, u16)> {
    if columns.is_empty() || width == 0 {
        return Vec::new();
    }
    // Decide the surviving set first.
    let mut keep: Vec<usize> = (0..columns.len()).collect();
    loop {
        let gaps = CELL_GAP * keep.len().saturating_sub(1) as u16;
        let needed: u16 = keep.iter().map(|&i| columns[i].min_width()).sum::<u16>() + gaps;
        if needed <= width || keep.len() == 1 {
            break;
        }
        // Drop the lowest priority; ties break toward the right, so the
        // leftmost of two equals is the one that stays.
        let victim = keep
            .iter()
            .copied()
            .enumerate()
            .min_by_key(|&(pos, i)| (columns[i].priority, std::cmp::Reverse(pos)))
            .map(|(pos, _)| pos);
        match victim {
            Some(pos) => {
                keep.remove(pos);
            }
            None => break,
        }
    }

    let gaps = CELL_GAP * keep.len().saturating_sub(1) as u16;
    let mins: u16 = keep.iter().map(|&i| columns[i].min_width()).sum();
    let mut widths: Vec<u16> = keep.iter().map(|&i| columns[i].min_width()).collect();

    // Hand the slack to the flexible columns, by weight.
    let mut slack = width.saturating_sub(mins + gaps);
    let total_weight: u32 = keep
        .iter()
        .map(|&i| match columns[i].width {
            Width::Flex { weight, .. } => weight as u32,
            Width::Fixed(_) => 0,
        })
        .sum();
    if slack > 0 && total_weight > 0 {
        let start_slack = slack;
        for (n, &i) in keep.iter().enumerate() {
            if let Width::Flex { weight, .. } = columns[i].width {
                let share = (start_slack as u32 * weight as u32 / total_weight) as u16;
                let share = share.min(slack);
                widths[n] += share;
                slack -= share;
            }
        }
        // Rounding leftovers go to the first flexible column, so the table
        // always fills its width exactly.
        if slack > 0
            && let Some(n) = keep
                .iter()
                .position(|&i| matches!(columns[i].width, Width::Flex { .. }))
        {
            widths[n] += slack;
        }
    }

    keep.into_iter().zip(widths).collect()
}

/// One row's cells, in the table's column order. A cell for a dropped column is
/// simply not drawn, so callers build all of them.
pub type Cells = Vec<String>;

/// Render the header row and register each sortable header.
pub fn render_header(ui: &mut Ui, area: Rect, columns: &[Column], sort: Sort) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect {
        height: 1,
        ..area
    };
    ui.fill(row, Style::default().bg(ui.surface()));
    let cols = layout(columns, row.width);

    let mut x = row.x;
    for (i, w) in cols {
        let rect = Rect {
            x,
            y: row.y,
            width: w,
            height: 1,
        };
        let col = &columns[i];
        let st = if col.sortable {
            ui.interactive(rect, ZoneId::new(ZoneKind::ColumnHeader, i as u32), false)
        } else {
            ui.state_of(ZoneId::new(ZoneKind::ColumnHeader, i as u32), false)
        };

        let active = col.sortable && sort.column == i;
        let mut sty = Style::default()
            .fg(if active {
                ui.theme.accent
            } else {
                ui.theme.ink_faint
            })
            .bg(ui.surface())
            .add_modifier(Modifier::BOLD);
        if st.hovered && col.sortable {
            sty = sty.add_modifier(Modifier::UNDERLINED);
        }

        // The sort marker eats into the title rather than overflowing the cell.
        let marker = if active {
            if sort.descending {
                glyphs::CHEVRON_DOWN.as_str()
            } else {
                glyphs::CHEVRON_RIGHT.as_str()
            }
        } else {
            ""
        };
        let marker_cols = col_width(marker);
        let title = truncate_cols(&col.title, (w as usize).saturating_sub(marker_cols));
        let text = format!("{title}{marker}");
        let text = match col.align {
            Align::Left => pad_to_cols(&text, w as usize),
            Align::Right => {
                let pad = (w as usize).saturating_sub(col_width(&text));
                format!("{}{}", " ".repeat(pad), text)
            }
        };
        ui.line(rect, Line::from(Span::styled(text, sty)), sty);
        x = x.saturating_add(w + CELL_GAP);
    }
}

/// Render the body. `cells_at` builds one row's cells, called only for the
/// visible rows.
pub fn render_body<F>(
    ui: &mut Ui,
    area: Rect,
    state: &mut ListState,
    columns: &[Column],
    len: usize,
    mut cells_at: F,
) -> std::ops::Range<usize>
where
    F: FnMut(usize) -> Cells,
{
    if area.width == 0 || area.height == 0 {
        return 0..0;
    }
    let overflowing = len > area.height as usize;
    let body_w = if overflowing {
        area.width.saturating_sub(1)
    } else {
        area.width
    };
    let cols = layout(columns, body_w);
    let aligns: Vec<Align> = cols.iter().map(|&(i, _)| columns[i].align).collect();
    let tints: Vec<Option<Color>> = cols.iter().map(|&(i, _)| columns[i].tint).collect();

    list::render(
        ui,
        Rect {
            width: body_w,
            ..area
        },
        state,
        len,
        list::Opts {
            rail: false,
            scrollbar: false,
            kind: ZoneKind::Row,
            id_base: 0,
        },
        |i| {
            let cells = cells_at(i);
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(cols.len() * 2);
            for (n, &(ci, w)) in cols.iter().enumerate() {
                let raw = cells.get(ci).map(String::as_str).unwrap_or("");
                let text = truncate_cols(raw, w as usize);
                let text = match aligns[n] {
                    Align::Left => pad_to_cols(&text, w as usize),
                    Align::Right => {
                        let pad = (w as usize).saturating_sub(col_width(&text));
                        format!("{}{}", " ".repeat(pad), text)
                    }
                };
                spans.push(match tints[n] {
                    Some(fg) => Span::styled(text, Style::default().fg(fg)),
                    None => Span::raw(text),
                });
                if n + 1 < cols.len() {
                    spans.push(Span::raw(" ".repeat(CELL_GAP as usize)));
                }
            }
            list::Row::new(Line::from(spans))
        },
    );

    if overflowing {
        list::render_scrollbar(
            ui,
            Rect {
                x: area.x + area.width - 1,
                width: 1,
                ..area
            },
            len,
            state.offset(),
        );
    }
    state.window(area.height, len)
}

/// Which column a header click landed on.
pub fn header_column(id: ZoneId) -> Option<usize> {
    (id.kind == ZoneKind::ColumnHeader).then_some(id.index as usize)
}

/// Selection styling for a table row, matching the list's.
pub fn row_style(ui: &Ui, selected: bool, focused: bool) -> Style {
    ui.row_style(super::style::State::selected(selected).with_focus(focused))
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

    /// The Lexicon's character table, roughly.
    fn columns() -> Vec<Column> {
        vec![
            Column::new("JP", Width::Flex { min: 8, weight: 2 }).priority(200),
            Column::new("Reading", Width::Flex { min: 8, weight: 2 }).priority(60),
            Column::new("Translation", Width::Flex { min: 10, weight: 3 }).priority(150),
            Column::new("Role", Width::Fixed(10)).priority(40),
            Column::new("Seen", Width::Fixed(5)).align(Align::Right).priority(20),
        ]
    }

    #[test]
    fn a_layout_exactly_fills_the_width_it_is_given() {
        let cols = columns();
        for w in [20u16, 40, 60, 80, 120, 200] {
            let got = layout(&cols, w);
            if got.is_empty() {
                continue;
            }
            let used: u16 = got.iter().map(|&(_, cw)| cw).sum::<u16>()
                + CELL_GAP * (got.len() as u16 - 1);
            assert_eq!(used, w, "at {w} the layout used {used}");
        }
    }

    #[test]
    fn columns_are_dropped_by_priority_lowest_first() {
        let cols = columns();
        let wide: Vec<usize> = layout(&cols, 200).into_iter().map(|(i, _)| i).collect();
        assert_eq!(wide.len(), 5, "everything fits at 200");

        let narrow: Vec<usize> = layout(&cols, 30).into_iter().map(|(i, _)| i).collect();
        // "Seen" (20) and "Role" (40) are the lowest priorities and go first.
        assert!(!narrow.contains(&4), "Seen should have been dropped");
        assert!(!narrow.contains(&3), "Role should have been dropped");
        assert!(narrow.contains(&0), "JP is the identity column and must stay");
    }

    #[test]
    fn the_identity_column_survives_any_width() {
        let cols = columns();
        for w in [1u16, 2, 5, 8, 12, 20] {
            let got = layout(&cols, w);
            assert!(!got.is_empty(), "at {w} the table vanished entirely");
            assert_eq!(
                got[0].0, 0,
                "at {w} the highest-priority column was dropped"
            );
        }
    }

    #[test]
    fn a_fixed_column_never_grows() {
        let cols = columns();
        let got = layout(&cols, 200);
        let seen = got.iter().find(|&&(i, _)| i == 4).expect("Seen present");
        assert_eq!(seen.1, 5, "a fixed column must keep its width");
    }

    #[test]
    fn slack_is_shared_by_weight() {
        let cols = vec![
            Column::new("a", Width::Flex { min: 5, weight: 1 }),
            Column::new("b", Width::Flex { min: 5, weight: 3 }),
        ];
        let got = layout(&cols, 45);
        let (a, b) = (got[0].1, got[1].1);
        assert!(b > a, "the heavier column should take more: {a} vs {b}");
        assert_eq!(a + b + CELL_GAP, 45);
    }

    #[test]
    fn clicking_a_new_column_sorts_ascending_and_clicking_again_flips() {
        let s = Sort::default();
        let by_two = s.toggled(2);
        assert_eq!(by_two.column, 2);
        assert!(!by_two.descending, "a new column starts ascending");

        let flipped = by_two.toggled(2);
        assert!(flipped.descending, "clicking the same column flips it");

        let back = flipped.toggled(2);
        assert!(!back.descending, "and flips back");

        let elsewhere = flipped.toggled(0);
        assert_eq!(elsewhere.column, 0);
        assert!(
            !elsewhere.descending,
            "moving to another column starts fresh rather than inheriting"
        );
    }

    fn paint(w: u16, h: u16, sort: Sort) -> (Vec<String>, Zones) {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let cols = columns();
        let mut state = ListState::new();
        state.select(Some(0));
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            render_header(
                &mut ui,
                Rect {
                    height: 1,
                    ..area
                },
                &cols,
                sort,
            );
            render_body(
                &mut ui,
                Rect {
                    y: 1,
                    height: h - 1,
                    ..area
                },
                &mut state,
                &cols,
                30,
                |i| {
                    vec![
                        format!("名前{i}"),
                        format!("なまえ{i}"),
                        format!("Name {i}"),
                        "main".into(),
                        format!("{i}"),
                    ]
                },
            );
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (lines, zones)
    }

    #[test]
    fn sortable_headers_register_and_decode_back_to_their_column() {
        let (_, zones) = paint(100, 8, Sort::default());
        for i in 0..5 {
            let id = ZoneId::new(ZoneKind::ColumnHeader, i);
            let r = zones
                .rect_of(id)
                .unwrap_or_else(|| panic!("header {i} not registered"));
            assert_eq!(r.y, 0, "headers live on the first row");
            assert_eq!(header_column(id), Some(i as usize));
        }
        assert_eq!(header_column(ZoneId::row(2)), None);
    }

    #[test]
    fn nothing_is_drawn_outside_the_table_at_any_width() {
        for w in [24u16, 40, 64, 100] {
            let (lines, zones) = paint(w, 8, Sort::default());
            for l in &lines {
                assert_eq!(l.chars().count(), w as usize, "a row overflowed at {w}");
            }
            for (rect, id) in zones.all() {
                assert!(
                    rect.x + rect.width <= w && rect.y + rect.height <= 8,
                    "at {w}: {id:?} at {rect:?} escaped"
                );
            }
        }
    }

    #[test]
    fn body_rows_register_their_data_index() {
        let (_, zones) = paint(100, 6, Sort::default());
        // Five body rows below the header, all from a 30-row dataset.
        for i in 0..5usize {
            assert!(zones.contains(ZoneId::row(i)), "row {i} not registered");
        }
        assert_eq!(zones.at(4, 1), Some(ZoneId::row(0)));
    }

    #[test]
    fn an_empty_table_is_not_a_panic() {
        assert!(layout(&[], 80).is_empty());
        assert!(layout(&columns(), 0).is_empty());
    }
}

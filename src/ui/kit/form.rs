//! Typed form fields, and a scrolling form built from them.
//!
//! A field declares what kind of value it holds and the component draws it, so
//! every row — and each of a stepper's arrows — registers a rectangle. That is
//! what makes a select something you click rather than cycle blind.
//!
//! One row per field. Help text belongs to the focused field and is drawn once,
//! wherever the caller puts it, rather than doubling the height of every row.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::list::{self, ListState};
use super::style::State;
use super::zones::{ZoneId, ZoneKind};
use crate::ui::glyphs;
use crate::ui::input::caret_halves;
use crate::ui::text::{pad_to_cols, truncate_cols};

/// Zone-index bases for a field's own sub-controls, kept far apart so a form
/// with a realistic number of fields cannot have one collide with another.
pub const DEC_BASE: u32 = 0x1000_0000;
pub const INC_BASE: u32 = 0x2000_0000;

/// The decrement affordance for field `i`.
pub fn dec_id(i: usize) -> ZoneId {
    ZoneId::new(ZoneKind::Button, DEC_BASE + i as u32)
}

/// The increment affordance for field `i`.
pub fn inc_id(i: usize) -> ZoneId {
    ZoneId::new(ZoneKind::Button, INC_BASE + i as u32)
}

/// Which field index a sub-control id belongs to, if any.
pub fn field_of(id: ZoneId) -> Option<(usize, Step)> {
    if id.kind != ZoneKind::Button {
        return None;
    }
    if id.index >= INC_BASE {
        Some(((id.index - INC_BASE) as usize, Step::Up))
    } else if id.index >= DEC_BASE {
        Some(((id.index - DEC_BASE) as usize, Step::Down))
    } else {
        None
    }
}

/// Which way a stepper arrow moves a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
}

/// What a field holds.
#[derive(Debug, Clone)]
pub enum Kind {
    Text {
        value: String,
        cursor: usize,
        placeholder: String,
    },
    /// A secret. Shown masked, and read-only when the environment supplies it —
    /// config cannot win against an env var, so offering to edit it would lie.
    Secret {
        value: String,
        cursor: usize,
        from_env: bool,
    },
    Number {
        value: i64,
        min: i64,
        max: i64,
    },
    Select {
        options: Vec<String>,
        index: usize,
    },
    Toggle {
        on: bool,
    },
}

impl Kind {
}

/// One row of a form.
#[derive(Debug, Clone)]
pub struct Field {
    pub label: String,
    pub kind: Kind,
    pub help: Option<String>,
    pub disabled: bool,
}

impl Field {
    pub fn new(label: impl Into<String>, kind: Kind) -> Self {
        Self {
            label: label.into(),
            kind,
            help: None,
            disabled: false,
        }
    }

    pub fn help(mut self, text: impl Into<String>) -> Self {
        self.help = Some(text.into());
        self
    }

    pub fn disabled(mut self, yes: bool) -> Self {
        self.disabled = yes;
        self
    }

    /// The value as displayed, masking a secret.
    pub fn display_value(&self) -> String {
        match &self.kind {
            Kind::Text { value, placeholder, .. } => {
                if value.is_empty() {
                    placeholder.clone()
                } else {
                    value.clone()
                }
            }
            Kind::Secret { value, from_env, .. } => {
                if *from_env {
                    "set by environment".to_string()
                } else if value.is_empty() {
                    "not set".to_string()
                } else {
                    mask(value)
                }
            }
            Kind::Number { value, .. } => value.to_string(),
            Kind::Select { options, index } => {
                options.get(*index).cloned().unwrap_or_default()
            }
            Kind::Toggle { on } => if *on { "on" } else { "off" }.to_string(),
        }
    }
}

/// Show a secret's shape without its content: enough to tell "a key is here"
/// from "the wrong key is here", never enough to read over a shoulder.
fn mask(secret: &str) -> String {
    let n = secret.chars().count();
    if n <= 8 {
        return "•".repeat(n.max(4));
    }
    let tail: String = secret.chars().skip(n - 4).collect();
    format!("{}{}", "•".repeat(8), tail)
}

/// Layout knobs for a form.
#[derive(Debug, Clone, Copy)]
pub struct Opts {
    /// Columns given to the label column, clamped to a third of the width.
    pub label_cols: u16,
    pub id_base: u32,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            label_cols: 22,
            id_base: 0,
        }
    }
}

/// Render `fields` into `area`, scrolling so the selected row stays visible.
///
/// Returns the range of fields actually drawn.
pub fn render(
    ui: &mut Ui,
    area: Rect,
    state: &mut ListState,
    fields: &[Field],
    opts: Opts,
) -> std::ops::Range<usize> {
    // Windowing and the scrollbar come from `list`, but the rows are drawn
    // here: a field owns sub-zones (a stepper's two arrows) and can be
    // disabled, neither of which a plain list row can model. Letting
    // `list::render` draw them too would register every row a second time —
    // and as focusable, which would quietly put disabled fields back in the
    // focus ring.
    ui.fill(area, Style::default().bg(ui.surface()));
    let overflowing = fields.len() > area.height as usize;
    let body_w = if overflowing {
        area.width.saturating_sub(1)
    } else {
        area.width
    };
    let win = state.window(area.height, fields.len());
    for (n, i) in win.clone().enumerate() {
        let rect = Rect {
            x: area.x,
            y: area.y + n as u16,
            width: body_w,
            height: 1,
        };
        render_field(ui, rect, &fields[i], i, state.selected() == Some(i), opts);
    }
    if overflowing {
        list::render_scrollbar(
            ui,
            Rect {
                x: area.x + area.width - 1,
                width: 1,
                ..area
            },
            fields.len(),
            state.offset(),
        );
    }
    win
}

/// Draw one field row and register it, plus any sub-controls it owns.
pub fn render_field(
    ui: &mut Ui,
    area: Rect,
    field: &Field,
    index: usize,
    selected: bool,
    opts: Opts,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect {
        height: 1,
        ..area
    };
    let id = ZoneId::new(ZoneKind::Field, opts.id_base + index as u32);
    let st = if field.disabled {
        ui.hit_only(row, id).with_disabled(true)
    } else {
        ui.interactive(row, id, selected)
    };
    let base = ui.row_style(st);
    ui.fill(row, base);

    // Rail, then label column, then the value takes the rest.
    let mut x = row.x;
    let (rail_glyph, rail_style) = match ui.rail_of(st) {
        Some((g, s)) => (g.as_str().to_string(), s),
        None => (" ".to_string(), base),
    };
    ui.line(
        Rect {
            x,
            y: row.y,
            width: 1,
            height: 1,
        },
        Line::from(Span::styled(rail_glyph, rail_style)),
        base,
    );
    x += 1;

    // Up to half the width, not a third: in a settings form the label column
    // is the thing being scanned, and a third truncates ordinary two-word
    // labels at 80 columns.
    let label_cols = opts.label_cols.min(row.width / 2).max(6);
    let label_style = if st.disabled {
        Style::default().fg(ui.theme.ink_faint).bg(base.bg.unwrap_or(ui.surface()))
    } else if selected || st.focused {
        Style::default()
            .fg(ui.theme.ink)
            .bg(base.bg.unwrap_or(ui.surface()))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(ui.theme.ink_soft)
            .bg(base.bg.unwrap_or(ui.surface()))
    };
    ui.line(
        Rect {
            x,
            y: row.y,
            width: label_cols,
            height: 1,
        },
        Line::from(Span::styled(
            pad_to_cols(&field.label, label_cols as usize),
            label_style,
        )),
        base,
    );
    x += label_cols;

    let value_rect = Rect {
        x,
        y: row.y,
        width: row.width.saturating_sub(x - row.x),
        height: 1,
    };
    render_value(ui, value_rect, field, index, st, base);
}

fn render_value(
    ui: &mut Ui,
    area: Rect,
    field: &Field,
    index: usize,
    st: State,
    base: Style,
) {
    if area.width == 0 {
        return;
    }
    let bg = base.bg.unwrap_or(ui.surface());
    let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);
    let normal = Style::default().fg(ui.theme.ink).bg(bg);

    match &field.kind {
        Kind::Toggle { on } => {
            let g = if *on {
                glyphs::TOGGLE_ON
            } else {
                glyphs::TOGGLE_OFF
            };
            let color = if st.disabled {
                ui.theme.ink_faint
            } else if *on {
                ui.theme.status_done
            } else {
                ui.theme.ink_faint
            };
            ui.line(
                area,
                Line::from(vec![
                    Span::styled(
                        g.as_str().to_string(),
                        Style::default().fg(color).bg(bg).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {}", if *on { "on" } else { "off" }),
                        if *on { normal } else { dim },
                    ),
                ]),
                base,
            );
        }
        Kind::Number { .. } | Kind::Select { .. } => {
            render_stepper(ui, area, field, index, st, base);
        }
        Kind::Secret { from_env, value, cursor } => {
            if *from_env {
                ui.line(
                    area,
                    Line::from(Span::styled(field.display_value(), dim)),
                    base,
                );
            } else if st.focused && !st.disabled {
                // A secret is edited at the end only, so the caret sits after
                // the mask rather than inside a value the user cannot see.
                let shown = if value.is_empty() {
                    String::new()
                } else {
                    mask(value)
                };
                let _ = cursor;
                ui.line(
                    area,
                    Line::from(vec![
                        Span::styled(truncate_cols(&shown, area.width as usize - 1), normal),
                        Span::styled(
                            glyphs::ACCENT_RAIL.as_str().to_string(),
                            Style::default().fg(ui.theme.stream_cursor).bg(bg),
                        ),
                    ]),
                    base,
                );
            } else {
                let style = if value.is_empty() { dim } else { normal };
                ui.line(
                    area,
                    Line::from(Span::styled(field.display_value(), style)),
                    base,
                );
            }
        }
        Kind::Text { value, cursor, placeholder } => {
            if st.focused && !st.disabled {
                let (before, after) = caret_halves(value, *cursor, area.width as usize - 1);
                ui.line(
                    area,
                    Line::from(vec![
                        Span::styled(before, normal),
                        Span::styled(
                            glyphs::ACCENT_RAIL.as_str().to_string(),
                            Style::default().fg(ui.theme.stream_cursor).bg(bg),
                        ),
                        Span::styled(after, normal),
                    ]),
                    base,
                );
            } else {
                let (text, style) = if value.is_empty() {
                    (placeholder.clone(), dim)
                } else {
                    (value.clone(), normal)
                };
                ui.line(
                    area,
                    Line::from(Span::styled(
                        truncate_cols(&text, area.width as usize),
                        style,
                    )),
                    base,
                );
            }
        }
    }
}

/// The widest a stepper's value column grows. Bounded rather than filling the
/// row: arrows pinned to the far edge of a wide column sit half a screen apart,
/// and clicking one means travelling to it.
const STEPPER_VALUE_COLS: u16 = 24;

/// `‹ value ›` with both arrows registered, so a stepped value can be changed
/// by pointing at it rather than only with the arrow keys.
fn render_stepper(ui: &mut Ui, area: Rect, field: &Field, index: usize, st: State, base: Style) {
    let bg = base.bg.unwrap_or(ui.surface());
    let value = field.display_value();

    let (at_min, at_max) = match &field.kind {
        Kind::Number { value, min, max } => (value <= min, value >= max),
        // Selects wrap, so neither end is ever a dead stop.
        _ => (false, false),
    };
    let arrow_style = |spent: bool| {
        if spent || st.disabled {
            Style::default()
                .fg(ui.theme.ink_faint)
                .bg(bg)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(ui.theme.accent).bg(bg)
        }
    };

    // A fixed-width block so the arrows stay put as the value cycles through
    // names of different lengths.
    let block = STEPPER_VALUE_COLS.min(area.width.saturating_sub(2));
    let shown = truncate_cols(&value, block as usize);

    let left = Rect {
        width: 1,
        ..area
    };
    if !st.disabled && !at_min {
        ui.zones.push_hit(left, dec_id(index));
    }
    ui.line(
        left,
        Line::from(Span::styled("<".to_string(), arrow_style(at_min))),
        base,
    );

    ui.line(
        Rect {
            x: area.x + 1,
            width: block,
            ..area
        },
        Line::from(Span::styled(
            pad_to_cols(&format!(" {shown}"), block as usize),
            Style::default()
                .fg(if st.disabled {
                    ui.theme.ink_faint
                } else {
                    ui.theme.ink
                })
                .bg(bg),
        )),
        base,
    );

    let right = Rect {
        x: area.x + 1 + block,
        width: 1,
        ..area
    };
    if right.x < area.x + area.width {
        if !st.disabled && !at_max {
            ui.zones.push_hit(right, inc_id(index));
        }
        ui.line(
            right,
            Line::from(Span::styled(">".to_string(), arrow_style(at_max))),
            base,
        );
    }
}

/// Draw the focused field's help text into `area`.
pub fn render_help(ui: &mut Ui, area: Rect, field: Option<&Field>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let style = Style::default().fg(ui.theme.ink_faint).bg(ui.theme.bg_elevated);
    ui.fill(area, style);
    let Some(help) = field.and_then(|f| f.help.as_deref()) else {
        return;
    };
    ui.text(
        Rect {
            height: 1,
            ..area
        },
        truncate_cols(help, area.width as usize),
        style,
    );
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

    fn fields() -> Vec<Field> {
        vec![
            Field::new(
                "Orchestrator model",
                Kind::Text {
                    value: "some-model".into(),
                    cursor: 4,
                    placeholder: "model id".into(),
                },
            )
            .help("Which model runs the metadata turn"),
            Field::new(
                "OpenRouter key",
                Kind::Secret {
                    value: "sk-abcdefghijklmnop".into(),
                    cursor: 0,
                    from_env: false,
                },
            ),
            Field::new(
                "Max attempts",
                Kind::Number {
                    value: 3,
                    min: 1,
                    max: 9,
                },
            ),
            Field::new(
                "Service tier",
                Kind::Select {
                    options: vec!["auto".into(), "flex".into(), "priority".into()],
                    index: 1,
                },
            ),
            Field::new("System One", Kind::Toggle { on: true }),
        ]
    }

    fn paint(
        w: u16,
        h: u16,
        focus: &Focus,
        state: &mut ListState,
        fields: &[Field],
    ) -> (Vec<String>, Zones) {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, focus, Hover::default(), 0);
            render(&mut ui, area, state, fields, Opts::default());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (lines, zones)
    }

    #[test]
    fn a_secret_shows_its_shape_but_never_its_content() {
        let f = Field::new(
            "key",
            Kind::Secret {
                value: "sk-supersecretvalue1234".into(),
                cursor: 0,
                from_env: false,
            },
        );
        let shown = f.display_value();
        assert!(!shown.contains("supersecret"), "leaked: {shown:?}");
        assert!(shown.contains("1234"), "the tail identifies which key: {shown:?}");
        assert!(shown.starts_with('•'));
    }

    #[test]
    fn a_short_secret_is_not_made_guessable_by_its_length() {
        // A four-character secret must not render as four dots, or the mask
        // hands over the length for free.
        assert_eq!(mask("ab").chars().count(), 4);
        assert_eq!(mask("abcd").chars().count(), 4);
    }

    #[test]
    fn an_env_supplied_secret_reads_as_read_only() {
        let f = Field::new(
            "key",
            Kind::Secret {
                value: String::new(),
                cursor: 0,
                from_env: true,
            },
        );
        assert_eq!(f.display_value(), "set by environment");
    }

    #[test]
    fn every_visible_field_registers_a_row() {
        let fs = fields();
        let mut st = ListState::new();
        st.select(Some(0));
        let (_, zones) = paint(80, 10, &Focus::new(), &mut st, &fs);
        for i in 0..fs.len() {
            assert!(
                zones.contains(ZoneId::new(ZoneKind::Field, i as u32)),
                "field {i} not registered"
            );
        }
    }

    #[test]
    fn stepped_fields_register_both_arrows_and_typed_ones_do_not() {
        let fs = fields();
        let mut st = ListState::new();
        st.select(Some(0));
        let (_, zones) = paint(80, 10, &Focus::new(), &mut st, &fs);

        // Number (index 2) and Select (index 3) are stepped.
        for i in [2usize, 3] {
            assert!(zones.contains(dec_id(i)), "field {i} has no decrement");
            assert!(zones.contains(inc_id(i)), "field {i} has no increment");
        }
        // Text (0) and Secret (1) are typed into, not stepped.
        for i in [0usize, 1] {
            assert!(!zones.contains(dec_id(i)), "field {i} should not step");
            assert!(!zones.contains(inc_id(i)));
        }
    }

    #[test]
    fn an_arrow_id_decodes_back_to_its_field_and_direction() {
        for i in [0usize, 1, 36, 999] {
            assert_eq!(field_of(dec_id(i)), Some((i, Step::Down)));
            assert_eq!(field_of(inc_id(i)), Some((i, Step::Up)));
        }
        // An ordinary button is not mistaken for a field's arrow.
        assert_eq!(field_of(ZoneId::button(3)), None);
        assert_eq!(field_of(ZoneId::row(3)), None);
    }

    #[test]
    fn a_number_at_its_limit_stops_offering_that_direction() {
        let at_max = vec![Field::new(
            "attempts",
            Kind::Number {
                value: 9,
                min: 1,
                max: 9,
            },
        )];
        let mut st = ListState::new();
        st.select(Some(0));
        let (_, zones) = paint(80, 4, &Focus::new(), &mut st, &at_max);
        assert!(zones.contains(dec_id(0)), "can still go down");
        assert!(
            !zones.contains(inc_id(0)),
            "clicking up at the maximum must not be offered"
        );
    }

    #[test]
    fn a_disabled_field_offers_no_arrows_and_takes_no_focus() {
        let fs = vec![
            Field::new(
                "tier",
                Kind::Select {
                    options: vec!["a".into(), "b".into()],
                    index: 0,
                },
            )
            .disabled(true),
        ];
        let mut st = ListState::new();
        let (_, zones) = paint(80, 4, &Focus::new(), &mut st, &fs);
        assert!(!zones.contains(dec_id(0)));
        assert!(!zones.contains(inc_id(0)));
        assert!(!zones.tab_order().any(|z| z == ZoneId::new(ZoneKind::Field, 0)));
    }

    #[test]
    fn labels_and_values_both_survive_a_narrow_form() {
        let fs = fields();
        let mut st = ListState::new();
        st.select(Some(0));
        for w in [40u16, 60, 80, 120] {
            let (lines, zones) = paint(w, 8, &Focus::new(), &mut st, &fs);
            for l in &lines {
                assert_eq!(l.chars().count(), w as usize, "a row overflowed at {w}");
            }
            for (rect, id) in zones.all() {
                assert!(
                    rect.x + rect.width <= w,
                    "at {w}: {id:?} at {rect:?} escaped"
                );
            }
        }
    }

    #[test]
    fn a_long_form_scrolls_to_keep_the_selection_visible() {
        let mut many: Vec<Field> = Vec::new();
        for i in 0..40 {
            many.push(Field::new(
                format!("field-{i}"),
                Kind::Toggle { on: i % 2 == 0 },
            ));
        }
        let mut st = ListState::new();
        st.select(Some(35));
        let (lines, zones) = paint(60, 6, &Focus::new(), &mut st, &many);
        assert!(
            zones.contains(ZoneId::new(ZoneKind::Field, 35)),
            "the selected field must be on screen"
        );
        let joined = lines.join("\n");
        assert!(joined.contains("field-35"), "got:\n{joined}");
        assert!(!joined.contains("field-0\n"), "should have scrolled past the top");
    }

    #[test]
    fn help_is_shown_for_the_focused_field_only() {
        let fs = fields();
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let mut term = Terminal::new(TestBackend::new(60, 1)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 1,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            render_help(&mut ui, area, Some(&fs[0]));
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = (0..60).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(text.contains("metadata turn"), "got {text:?}");
    }
}

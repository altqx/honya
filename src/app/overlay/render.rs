//! How each overlay draws itself.
//!
//! A second `impl Overlay` block, split from the dispatch and key handling
//! so neither file has to be the whole surface at once. Everything here
//! goes through `kit::Modal`, which owns the frame, the backdrop, the close
//! affordance and the focus trap, and registers what it drew — so none of
//! these restate their geometry anywhere for a click handler to consult.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::settings_defs::{self, Group, SField};
use crate::model::{AppConfig, LogLevel};
use crate::theme::ALL_THEMES;
use crate::ui::input;
use crate::ui::kit::{ZoneId, ZoneKind};
use crate::ui::text::{col_width, pad_to_cols, thai_display_safe, truncate_cols};

use super::*;

impl Overlay {
    pub fn render(
        &mut self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        cfg: &AppConfig,
        log: &[(LogLevel, String)],
    ) {
        // The two pickers own a selection and a scroll offset, so they take a
        // mutable borrow the rest of the match cannot share.
        match self {
            Overlay::Palette(st) => return render_palette(ui, area, st),
            Overlay::ReaderJump(st) => return render_reader_jump(ui, area, st),
            _ => {}
        }
        self.render_static(ui, area, cfg, log);
    }

    fn render_static(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        cfg: &AppConfig,
        log: &[(LogLevel, String)],
    ) {
        // Overlays still on the old path draw straight to the frame. They are
        // converted a batch at a time; `is_kit_rendered` says which are done.
        match self {
            Overlay::None | Overlay::Palette(_) | Overlay::ReaderJump(_) => {}
            Overlay::Help(off) => self.render_help_kit(ui, area, *off),
            Overlay::About => self.render_about_kit(ui, area),
            Overlay::Log(off) => self.render_log_kit(ui, area, log, *off),
            Overlay::Modal(dlg) => self.render_modal_kit(ui, area, dlg),
            Overlay::Export(st) => self.render_export_kit(ui, area, st),
            Overlay::Theme(st) => self.render_theme_kit(ui, area, st),

            Overlay::Welcome(st) => self.render_welcome_kit(ui, area, st),
            Overlay::Import(st) => self.render_import_kit(ui, area, st),
            Overlay::ImageSource(st) => self.render_image_source_kit(ui, area, st),
            Overlay::Settings(st) => self.render_settings_kit(ui, area, cfg, st),
            Overlay::Synopsis(st) => self.render_synopsis_kit(ui, area, st),
            Overlay::ProjectTitle(st) => self.render_project_title_kit(ui, area, st),
            Overlay::Qa(st) => self.render_qa_kit(ui, area, st),
            Overlay::ReaderNote(st) => self.render_reader_note_kit(ui, area, st),
            Overlay::ReaderInspect(st) => self.render_reader_inspect_kit(ui, area, st),
            Overlay::ReaderEdit(st) => self.render_reader_edit_kit(ui, area, st),
            Overlay::ReaderSearch(st) => self.render_reader_search_kit(ui, area, st),
        }
    }

    // ---- kit-rendered overlays -------------------------------------------
    //
    // Each of these draws through `kit::Modal`, which owns the frame, the
    // backdrop, the close affordance and the focus trap. None of them restate
    // their geometry anywhere: the registry the modal writes to is what clicks
    // resolve against.

    /// Keybinding reference, grouped by where the bindings apply.
    fn render_help_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, off: u16) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("Help — keybindings")
            .sizing(Sizing::large())
            .subtitle("jk scroll · Esc close")
            .render(ui, area);

        let rows = help_rows();
        // The key column is as wide as the widest key plus a gutter, rather
        // than a guess: ": / Ctrl-P / Ctrl-K" overran a hardcoded 18 and ran
        // into its own description.
        let key_cols = rows
            .iter()
            .filter_map(|r| match r {
                HelpRow::Binding(k, _) => Some(col_width(k)),
                _ => None,
            })
            .max()
            .unwrap_or(16)
            + 4;
        let mut st = ListState::new();
        st.scroll_by(off as isize, frame.body.height, rows.len());
        let dim = Style::default().fg(ui.theme.ink_faint);
        let key = Style::default()
            .fg(ui.theme.ink_soft)
            .add_modifier(Modifier::BOLD);
        let head = Style::default()
            .fg(ui.theme.accent)
            .add_modifier(Modifier::BOLD);

        list::render(
            ui,
            frame.body,
            &mut st,
            rows.len(),
            list::Opts {
                rail: false,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| match &rows[i] {
                HelpRow::Section(title) => {
                    Row::header(Line::from(Span::styled(title.to_string(), head)))
                }
                HelpRow::Blank => Row::header(Line::raw("")),
                HelpRow::Binding(k, what) => Row::header(Line::from(vec![
                    Span::styled(pad_to_cols(k, key_cols), key),
                    Span::styled(what.to_string(), dim),
                ])),
            },
        );
    }

    /// About card. Animated off the frame ticker: the waxing-moon status
    /// metaphor cycles, the three agents pulse left to right, and a line of
    /// Japanese is typed out in Thai one grapheme at a time — typing by
    /// grapheme rather than by char so a Thai cluster is never split mid-mark.
    fn render_about_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::modal::{Modal, Sizing};
        use unicode_segmentation::UnicodeSegmentation;

        let frame = ui.frame_count;
        let title = thai_display_safe("About · เกี่ยวกับ");
        let f = Modal::new(&title)
            .sizing(Sizing::small())
            .subtitle(env!("CARGO_PKG_VERSION"))
            .render(ui, area);

        let bg = ui.theme.bg_elevated;
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);
        let soft = Style::default().fg(ui.theme.ink_soft).bg(bg);
        let accent = Style::default()
            .fg(ui.theme.accent)
            .bg(bg)
            .add_modifier(Modifier::BOLD);

        const PHASES: [crate::ui::glyphs::Glyph; 5] = [
            crate::ui::glyphs::MOON_NEW,
            crate::ui::glyphs::MOON_CRESCENT,
            crate::ui::glyphs::MOON_FIRST_QUARTER,
            crate::ui::glyphs::MOON_LAST_QUARTER,
            crate::ui::glyphs::MOON_FULL,
        ];
        let moon = PHASES[(frame / 4) as usize % PHASES.len()];

        let mut row = 0u16;
        let mut put = |ui: &mut crate::ui::kit::Ui, line: Line<'static>| {
            let r = row_at(f.body, row);
            if r.height > 0 {
                ui.line(r, line, Style::default().bg(bg));
            }
            row += 1;
        };

        put(
            ui,
            Line::from(vec![
                Span::styled(format!("{} ", moon.as_str()), accent),
                Span::styled("honya 本屋", accent),
            ]),
        );
        put(
            ui,
            Line::from(Span::styled(
                "Japanese → Thai / English light-novel translation.",
                soft,
            )),
        );
        put(ui, Line::raw(""));

        // The typing demo: hold the finished line a moment, then start over.
        let jp = "「月が綺麗ですね。」";
        let th_full = thai_display_safe("— พระจันทร์คืนนี้สวยเหลือเกินนะ");
        let graphemes: Vec<&str> = th_full.graphemes(true).collect();
        const HOLD: usize = 22;
        let pos = (frame as usize) % (graphemes.len() + HOLD);
        let shown = pos.min(graphemes.len());
        let typed: String = graphemes[..shown].concat();
        let caret_on = shown < graphemes.len() || frame % 10 < 5;

        put(ui, Line::from(Span::styled(jp, soft)));
        put(
            ui,
            Line::from(vec![
                Span::styled(typed, Style::default().fg(ui.theme.translated_text).bg(bg)),
                Span::styled(
                    if caret_on {
                        crate::ui::glyphs::ACCENT_RAIL.as_str()
                    } else {
                        " "
                    },
                    Style::default().fg(ui.theme.stream_cursor).bg(bg),
                ),
            ]),
        );
        put(ui, Line::raw(""));

        // The three agents, pulsing left to right.
        let active = ((frame / 6) % 3) as usize;
        let agents = [
            ("Orchestrator", ui.theme.accent),
            ("Translator", ui.theme.status_working),
            ("Reviewer", ui.theme.accent_soft),
        ];
        let spinner = crate::ui::glyphs::frame_of(&crate::ui::glyphs::SPINNER, frame);
        let mut pipeline: Vec<Span<'static>> = Vec::new();
        for (i, (name, color)) in agents.into_iter().enumerate() {
            if i > 0 {
                pipeline.push(Span::styled(
                    format!(" {} ", crate::ui::glyphs::RULE_H.as_str()),
                    dim,
                ));
            }
            let (mark, style) = if i == active {
                (
                    spinner.as_str().to_string(),
                    Style::default().fg(color).bg(bg).add_modifier(Modifier::BOLD),
                )
            } else {
                (crate::ui::glyphs::BADGE_ORCHESTRATOR.as_str().to_string(), dim)
            };
            pipeline.push(Span::styled(format!("{mark} {name}"), style));
        }
        put(ui, Line::from(pipeline));
        put(ui, Line::raw(""));

        let commit = option_env!("HONYA_BUILD_COMMIT").unwrap_or("dev");
        put(
            ui,
            Line::from(vec![
                Span::styled(format!("build {commit}   "), dim),
                Span::styled(env!("CARGO_PKG_HOMEPAGE"), dim),
            ]),
        );
    }

    /// The activity log, newest last, scrolled back by `off`.
    fn render_log_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        log: &[(LogLevel, String)],
        off: u16,
    ) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("Activity log")
            .sizing(Sizing::large())
            .subtitle(format!("{} entries", log.len()))
            .render(ui, area);

        // `off` counts backwards from the newest entry, which is where the log
        // sits when opened.
        let mut st = ListState::following();
        if off > 0 {
            st.set_follow(false);
            st.scroll_by(
                -(off as isize),
                frame.body.height,
                log.len(),
            );
        }
        let colors = [
            ui.theme.ink_faint,
            ui.theme.ink_soft,
            ui.theme.status_warn,
            ui.theme.status_failed,
        ];
        list::render(
            ui,
            frame.body,
            &mut st,
            log.len(),
            list::Opts {
                rail: false,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let (level, msg) = &log[i];
                let (glyph, color) = match level {
                    LogLevel::Trace => (crate::ui::glyphs::DOT, colors[0]),
                    LogLevel::Info => (crate::ui::glyphs::CHECK, colors[1]),
                    LogLevel::Warn => (crate::ui::glyphs::FLAG, colors[2]),
                    LogLevel::Error => (crate::ui::glyphs::CROSS, colors[3]),
                };
                Row::header(Line::from(vec![
                    Span::styled(
                        format!("{} ", glyph.as_str()),
                        Style::default().fg(color),
                    ),
                    Span::styled(
                        thai_display_safe(msg),
                        Style::default().fg(colors[1]),
                    ),
                ]))
            },
        );
    }

    /// A query line over a ranked list — the shape the command bar and the
    /// Reader's jump list both are.
    ///
    /// `rows` is already filtered and ranked; this only draws it. Matched
    /// Reader search: one query line, applied to both panes.
    fn render_reader_search_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderSearchState,
    ) {
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Search")
            .sizing(Sizing::small())
            .footer(1)
            .render(ui, area);
        crate::ui::kit::picker::render_query(
            ui,
            crate::ui::kit::ctx::row_at(frame.body, 0),
            &st.query,
            st.cursor,
            "Search both panes…",
            None,
        );
        modal::footer_hint(ui, frame.footer, "  ↵ search both panes · Esc cancel");
    }

    /// A proofreading note, anchored to one translated line.
    fn render_reader_note_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderNoteState,
    ) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::editor;
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Note")
            .sizing(Sizing::small())
            .subtitle(format!("ch.{:03} · line {}", st.chapter, st.line))
            .footer(1)
            .render(ui, area);

        let lines = editor::wrap(&st.text, frame.body.width);
        editor::render(
            ui,
            frame.body,
            &editor::View::new(&st.text, &lines).cursor(st.cursor),
            0,
        );
        modal::render_footer(
            ui,
            frame.footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(DIALOG_CANCEL), "Cancel").accel("esc"),
                Button::new(ZoneId::button(DIALOG_CONFIRM), "Save")
                    .accel("↵")
                    .primary()
                    .disabled(st.text.trim().is_empty()),
            ]),
        );
    }

    /// First-run menu: four ways in, each a real row.
    fn render_welcome_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &WelcomeState) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{Modal, Sizing};

        // Five preamble rows, four menu rows, and the frame's own padding.
        let frame = Modal::new("ようこそ · Welcome to honya 本屋")
            .sizing(
                Sizing::medium()
                    .fit_height(5 + WELCOME_ITEMS as u16 + 4)
                    // The longest preamble line, so it is not truncated.
                    .fit_width(64),
            )
            .render(ui, area);

        let bg = ui.theme.bg_elevated;
        let soft = Style::default().fg(ui.theme.ink_soft).bg(bg);
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);
        for (n, line) in [
            "AI-assisted Japanese → Thai / English light-novel translation.",
            "",
            "Import an EPUB, then a three-agent pipeline works through it:",
            "Orchestrator ◆ plans · Translator ▲ drafts · Reviewer ■ checks.",
            "",
        ]
        .into_iter()
        .enumerate()
        {
            ui.text(
                row_at(frame.body, n as u16),
                line,
                if line.starts_with("AI-") { soft } else { dim },
            );
        }

        let menu = welcome_items(st);
        let list_area = Rect {
            y: frame.body.y + 5,
            height: frame.body.height.saturating_sub(5),
            ..frame.body
        };
        let mut ls = ListState::new();
        ls.select(Some(st.sel));
        list::render(
            ui,
            list_area,
            &mut ls,
            menu.len(),
            list::Opts {
                rail: true,
                scrollbar: false,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let (label, note) = menu[i];
                Row::new(Line::from(vec![
                    Span::raw(label.to_string()),
                    Span::styled(format!("   {note}"), dim),
                ]))
            },
        );
    }

    /// Pick a source file to refresh a volume's images from.
    fn render_image_source_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ImageSourceState,
    ) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Update volume images")
            .sizing(Sizing::medium())
            .subtitle(format!("Vol.{:02}", st.vol))
            .footer(1)
            .render(ui, area);

        let dim = Style::default().fg(ui.theme.ink_faint);
        let mut ls = ListState::new();
        ls.select(Some(st.sel));
        list::render(
            ui,
            frame.body,
            &mut ls,
            st.files.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let (path, size) = &st.files[i];
                let name = thai_display_safe(
                    &path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                );
                Row::new(Line::from(vec![
                    Span::raw(name),
                    Span::styled(format!("   {}", human_size(*size)), dim),
                ]))
            },
        );
        modal::footer_hint(ui, frame.footer, "  ↵ use this file · Esc cancel");
    }

    /// The QA inbox: a run summary, then every finding grouped under the
    /// chapter it belongs to.
    fn render_qa_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &QaState) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("QA review")
            .sizing(Sizing::large())
            .subtitle(truncate_cols(&thai_display_safe(&st.title), 48))
            .footer(1)
            .render(ui, area);

        let bg = ui.theme.bg_elevated;
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);

        // Summary band: how the run went, before the list of what went wrong.
        let mut summary: Vec<Span<'static>> = Vec::new();
        for (glyph, count, color) in [
            (crate::ui::glyphs::MOON_FULL, st.report.done, ui.theme.status_done),
            (crate::ui::glyphs::FLAG, st.report.review, ui.theme.status_warn),
            (crate::ui::glyphs::CROSS, st.report.failed, ui.theme.status_failed),
        ] {
            summary.push(Span::styled(
                format!("{}{count}", glyph.as_str()),
                Style::default()
                    .fg(if count == 0 { ui.theme.ink_faint } else { color })
                    .bg(bg),
            ));
            summary.push(Span::styled("    ", dim));
        }
        if let Some(pct) = st.report.clean_pct() {
            summary.push(Span::styled(
                format!("{pct}% clean"),
                Style::default()
                    .fg(ui.theme.ink_soft)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        ui.line(row_at(frame.body, 0), Line::from(summary), Style::default().bg(bg));

        let list_area = Rect {
            y: frame.body.y + 2,
            height: frame.body.height.saturating_sub(2),
            ..frame.body
        };

        if st.report.issues.is_empty() {
            ui.text(
                row_at(list_area, 0),
                "Nothing flagged — every finished chapter passed review.",
                Style::default().fg(ui.theme.status_done).bg(bg),
            );
            modal::footer_hint(ui, frame.footer, "  Esc close");
            return;
        }

        let rows = qa_rows(&st.report);
        let selected_row = rows
            .iter()
            .position(|r| matches!(r, QaRow::Issue(i) if *i == st.sel));

        let head = Style::default()
            .fg(ui.theme.accent)
            .add_modifier(Modifier::BOLD);
        let plain = Style::default().fg(ui.theme.ink);
        // Per-chapter counts, keyed the same way the headings are grouped.
        let counts: Vec<usize> = rows
            .iter()
            .map(|r| match r {
                QaRow::Heading(_) => 0,
                QaRow::Issue(i) => st.report.count_for(st.report.issues[*i].chapter),
            })
            .collect();
        let visuals: Vec<(String, ratatui::style::Color, String)> = st
            .report
            .issues
            .iter()
            .map(|iss| {
                let (g, c, tag) = qa_visual(iss, ui.theme);
                (g.to_string(), c, tag)
            })
            .collect();
        // An empty reason still has to say something, or the row reads as a
        // finding with no content.
        let details: Vec<String> = st
            .report
            .issues
            .iter()
            .map(|iss| {
                if iss.detail.trim().is_empty() {
                    qa_default_detail(iss).to_string()
                } else {
                    thai_display_safe(&iss.detail)
                }
            })
            .collect();

        let mut ls = ListState::new();
        ls.select(selected_row);
        list::render(
            ui,
            list_area,
            &mut ls,
            rows.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| match &rows[i] {
                QaRow::Heading(t) => {
                    // The count belongs to the heading, so take it from the
                    // finding immediately below.
                    let n = counts.get(i + 1).copied().unwrap_or(0);
                    Row::header(Line::from(vec![
                        Span::styled(t.clone(), head),
                        Span::styled(
                            if n > 1 { format!("   {n} findings") } else { String::new() },
                            Style::default().fg(ui.theme.ink_faint),
                        ),
                    ]))
                }
                QaRow::Issue(n) => {
                    let (glyph, color, tag) = &visuals[*n];
                    Row::new(Line::from(vec![
                        Span::styled(format!("  {glyph} "), Style::default().fg(*color)),
                        Span::styled(
                            format!("{tag:<12}"),
                            Style::default().fg(ui.theme.ink_faint),
                        ),
                        Span::styled(details[*n].clone(), plain),
                    ]))
                }
            },
        );
        modal::footer_hint(
            ui,
            frame.footer,
            "  ↵ open in the Reader · jk move · Esc close",
        );
    }

    /// Read-only source ‖ translation ‖ reviewer note for one chunk.
    fn render_reader_inspect_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderInspectState,
    ) {
        use crate::ui::kit::card::Card;
        use crate::ui::kit::editor;
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Inspect chunk")
            .sizing(Sizing::large())
            .subtitle(format!("ch.{:03} · chunk {}", st.chapter, st.chunk + 1))
            .footer(1)
            .render(ui, area);

        // Source and translation share the width; the reviewer note, when there
        // is one, takes a band underneath both.
        let note_rows = if st.review.is_some() {
            (frame.body.height / 4).clamp(3, 8)
        } else {
            0
        };
        let panes_h = frame.body.height.saturating_sub(note_rows);
        let half = frame.body.width / 2;

        for (n, (title, text)) in [
            ("Source 日本語", &st.source_jp),
            ("Translation", &st.translated_text),
        ]
        .into_iter()
        .enumerate()
        {
            let pane = Rect {
                x: frame.body.x + n as u16 * half,
                y: frame.body.y,
                width: half,
                height: panes_h,
            };
            let body = Card::new(title).render(ui, pane);
            let wrapped = editor::wrap(text, body.width);
            editor::render(
                ui,
                body,
                &editor::View::new(text, &wrapped).scroll(st.scroll as usize),
                n as u32,
            );
        }

        if let Some(review) = &st.review {
            let band = Rect {
                y: frame.body.y + panes_h,
                height: note_rows,
                ..frame.body
            };
            let body = Card::new("Reviewer note")
                .accent(ui.theme.status_warn)
                .render(ui, band);
            let wrapped = editor::wrap(review, body.width);
            editor::render(ui, body, &editor::View::new(review, &wrapped), 2);
        }
        modal::footer_hint(ui, frame.footer, "  jk scroll · Esc close");
    }

    /// In-place editor for one chunk's translated prose.
    fn render_reader_edit_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderEditState,
    ) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::editor;
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Edit translation")
            .sizing(Sizing::large())
            .subtitle(format!("ch.{:03} · chunk {}", st.chapter, st.chunk + 1))
            .footer(1)
            .render(ui, area);

        let wrapped = editor::wrap(&st.text, frame.body.width);
        editor::render(
            ui,
            frame.body,
            &editor::View::new(&st.text, &wrapped).cursor(st.cursor),
            0,
        );
        modal::render_footer(
            ui,
            frame.footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(DIALOG_CANCEL), "Discard").accel("esc"),
                Button::new(ZoneId::button(DIALOG_CONFIRM), "Save")
                    .accel("^s")
                    .primary(),
            ]),
        );
    }

    /// The shared source/translation editor behind the synopsis and title
    /// overlays: raw text above, its translation below, with the agent's phase
    /// reported between them.
    fn render_syn_body(
        &self,
        ui: &mut crate::ui::kit::Ui,
        body: Rect,
        syn: &SynopsisState,
        raw_title: &str,
    ) {
        use crate::ui::kit::card::Card;
        use crate::ui::kit::editor;

        let half = body.height / 2;
        let raw_area = Rect {
            height: half,
            ..body
        };
        let out_area = Rect {
            y: body.y + half,
            height: body.height.saturating_sub(half),
            ..body
        };

        let raw_body = Card::new(raw_title).render(ui, raw_area);
        let raw_wrapped = editor::wrap(&syn.raw, raw_body.width);
        editor::render(
            ui,
            raw_body,
            &editor::View::new(&syn.raw, &raw_wrapped)
                .cursor(if syn.edit_translation { usize::MAX } else { syn.cursor }),
            0,
        );

        let (label, accent) = match syn.phase {
            SynPhase::Editing => ("Translation", ui.theme.rule),
            SynPhase::Translating => ("Translation · working…", ui.theme.status_working),
            SynPhase::Done => ("Translation", ui.theme.status_done),
            SynPhase::Failed => ("Translation · failed", ui.theme.status_failed),
        };
        let out_body = Card::new(label).accent(accent).render(ui, out_area);
        let shown = if matches!(syn.phase, SynPhase::Failed) && !syn.error.is_empty() {
            &syn.error
        } else {
            &syn.translated_text
        };
        let out_wrapped = editor::wrap(shown, out_body.width);
        editor::render(
            ui,
            out_body,
            &editor::View::new(shown, &out_wrapped).cursor(if syn.edit_translation {
                syn.translated_cursor
            } else {
                usize::MAX
            }),
            1,
        );
    }

    /// Volume synopsis editor.
    fn render_synopsis_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &SynopsisEditState,
    ) {
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("Synopsis")
            .sizing(Sizing::large())
            .subtitle(format!(
                "Vol.{:02} · {}",
                st.vol,
                truncate_cols(&thai_display_safe(&st.title), 30)
            ))
            .footer(1)
            .render(ui, area);
        self.render_syn_body(ui, frame.body, &st.syn, "Source 日本語");
        self.render_syn_footer(ui, frame.footer, &st.syn);
    }

    /// Project title editor — the same shape, one line each.
    fn render_project_title_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &TitleEditState,
    ) {
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("Project title")
            .sizing(Sizing::medium())
            .footer(1)
            .render(ui, area);
        self.render_syn_body(ui, frame.body, &st.syn, "Title 日本語");
        self.render_syn_footer(ui, frame.footer, &st.syn);
    }

    /// Buttons shared by both translate-and-accept editors.
    fn render_syn_footer(&self, ui: &mut crate::ui::kit::Ui, footer: Rect, syn: &SynopsisState) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::modal;

        let working = matches!(syn.phase, SynPhase::Translating);
        let label = if syn.attempt > 0 { "Reroll" } else { "Translate" };
        modal::render_footer(
            ui,
            footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(DIALOG_CANCEL), "Cancel").accel("esc"),
                Button::new(ZoneId::button(DIALOG_ALTERNATE), label)
                    .accel("tab")
                    .disabled(working || syn.raw.trim().is_empty()),
                Button::new(ZoneId::button(DIALOG_CONFIRM), "Save")
                    .accel("↵")
                    .primary()
                    .disabled(working),
            ]),
        );
    }

    /// Settings: a category rail beside a form generated from the registry.
    ///
    /// Nothing here knows what any individual setting *is* — the rows come from
    /// [`settings_defs::ORDER`] and their live values from `field_kind`, which
    /// is the whole point of declaring them once.
    fn render_settings_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        cfg: &AppConfig,
        st: &SettingsState,
    ) {
        use crate::ui::kit::form;
        use crate::ui::kit::list::ListState;
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Settings")
            .sizing(Sizing::large())
            .footer(2)
            .render(ui, area);

        // Rail on the left, form on the right. At narrow widths the rail
        // collapses to a single strip of initials rather than stealing the
        // columns the values need.
        let rail_w = if ui.metrics.is_narrow() { 3 } else { 16 };
        let rail = Rect {
            width: rail_w.min(frame.body.width / 3),
            ..frame.body
        };
        let form_area = Rect {
            x: frame.body.x + rail.width + 1,
            width: frame.body.width.saturating_sub(rail.width + 1),
            ..frame.body
        };
        self.render_settings_rail(ui, rail, st);

        if st.tab == SettingsTab::Account {
            self.render_settings_account(ui, form_area, st);
            modal::footer_hint(ui, frame.footer, "  tab switch section · Esc close");
            return;
        }

        let fields: Vec<form::Field> = st
            .tab
            .group()
            .fields()
            .into_iter()
            .filter_map(|i| settings_defs::at(i).map(|d| self.settings_field(cfg, st, d)))
            .collect();
        let group_start = st.tab.group().first_field().unwrap_or(0);
        let mut ls = ListState::new();
        ls.select(Some(st.field.saturating_sub(group_start) as usize));

        form::render(
            ui,
            form_area,
            &mut ls,
            &fields,
            form::Opts {
                label_cols: 26,
                id_base: group_start as u32,
            },
        );

        // The focused row's help, rather than a help line on every row.
        let focused = (st.field.saturating_sub(group_start)) as usize;
        form::render_help(
            ui,
            Rect {
                height: 1,
                ..frame.footer
            },
            fields.get(focused),
        );
        modal::footer_hint(
            ui,
            Rect {
                y: frame.footer.y + 1,
                height: 1,
                ..frame.footer
            },
            "  ↵ save · tab section · ←→ change · Esc close",
        );
    }

    /// The category rail. Each entry is a zone, so a section is one click away.
    fn render_settings_rail(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &SettingsState,
    ) {
        let narrow = area.width < 6;
        for (n, group) in Group::ALL.into_iter().enumerate() {
            let row = crate::ui::kit::ctx::row_at(area, n as u16);
            if row.height == 0 {
                break;
            }
            let active = st.tab.group() == group;
            let state = ui.interactive(row, ZoneId::segment(n), active);
            let base = ui.row_style(state);
            ui.fill(row, base);
            let (glyph, rail_style) = match ui.rail_of(state) {
                Some((g, s)) => (g.as_str().to_string(), s),
                None => (" ".to_string(), base),
            };
            let label = if narrow {
                group.title().chars().next().unwrap_or(' ').to_string()
            } else {
                group.title().to_string()
            };
            ui.line(
                row,
                Line::from(vec![
                    Span::styled(glyph, rail_style),
                    Span::styled(
                        format!(" {label}"),
                        if active {
                            base.add_modifier(Modifier::BOLD)
                        } else {
                            base
                        },
                    ),
                ]),
                base,
            );
        }
    }

    /// The Account section: the two sign-ins and the remote link. These are
    /// actions rather than settings, so they have no registry rows and carry
    /// their own controls.
    fn render_settings_account(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &SettingsState,
    ) {
        use crate::app::overlay::state::{ACCOUNT_CODEX, ACCOUNT_GITHUB, ACCOUNT_REMOTE};
        use crate::remote::protocol::RemoteState;
        use crate::ui::kit::button::Button;
        use crate::ui::kit::ctx::row_at;

        let bg = ui.theme.bg_elevated;
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);

        // label · status · the control that changes it, one row each.
        let rows: [(u16, &str, String, ratatui::style::Color, &str, &str); 3] = [
            (
                ACCOUNT_GITHUB,
                "GitHub",
                match &st.account_login {
                    Some(login) => format!("signed in as {login}"),
                    None => "not signed in".into(),
                },
                match st.account_login {
                    Some(_) => ui.theme.status_done,
                    None => ui.theme.ink_soft,
                },
                if st.account_login.is_some() { "sign out" } else { "sign in" },
                if st.account_login.is_some() { "^O" } else { "^A" },
            ),
            (
                ACCOUNT_CODEX,
                "Codex",
                match &st.codex_account {
                    // An older token carries no account id, so being signed in
                    // is all there is to report.
                    Some(id) if id.is_empty() => "signed in".into(),
                    // The account id is a UUID; enough of it to recognise is
                    // all that earns the room.
                    Some(id) => {
                        format!("signed in · {}", id.chars().take(8).collect::<String>())
                    }
                    None => "not signed in".into(),
                },
                match st.codex_account {
                    Some(_) => ui.theme.status_done,
                    None => ui.theme.ink_soft,
                },
                if st.codex_account.is_some() { "sign out" } else { "sign in" },
                "^X",
            ),
            (
                ACCOUNT_REMOTE,
                "Remote",
                match st.remote_state {
                    RemoteState::Connected => {
                        format!("connected · {} watching", st.remote_watchers)
                    }
                    RemoteState::Connecting => "connecting…".into(),
                    RemoteState::Pairing => "pairing…".into(),
                    RemoteState::Error => "error".into(),
                    RemoteState::Disconnected if st.remote_enabled => "enabled".into(),
                    RemoteState::Disconnected => "off".into(),
                },
                match st.remote_state {
                    RemoteState::Connected => ui.theme.status_done,
                    RemoteState::Connecting | RemoteState::Pairing => ui.theme.status_working,
                    RemoteState::Error => ui.theme.status_failed,
                    RemoteState::Disconnected => ui.theme.ink_soft,
                },
                if st.remote_enabled { "disable" } else { "enable" },
                "^R",
            ),
        ];

        for (n, (id, label, status, color, verb, accel)) in rows.into_iter().enumerate() {
            let row = row_at(area, n as u16 * 2);
            if row.height == 0 {
                continue;
            }
            ui.line(
                row,
                Line::from(vec![
                    Span::styled(format!("{label:<14}"), dim),
                    Span::styled(status, Style::default().fg(color).bg(bg)),
                ]),
                Style::default().bg(bg),
            );
            // The relay needs an account before it has anything to link.
            let blocked = id == ACCOUNT_REMOTE && st.account_login.is_none();
            let button = Button::new(crate::ui::kit::ZoneId::action(id), verb)
                .accel(accel)
                .disabled(blocked);
            let w = button.width();
            if row.width > w + 2 {
                button.render(
                    ui,
                    Rect {
                        x: row.x + row.width - w,
                        width: w,
                        ..row
                    },
                );
            }
        }

        if let Some(prompt) = &st.remote_auth_code {
            let row = row_at(area, 6);
            if row.height > 0 {
                ui.line(
                    row,
                    Line::from(vec![
                        Span::styled("Code          ", dim),
                        Span::styled(
                            prompt.code.clone(),
                            Style::default()
                                .fg(ui.theme.accent)
                                .bg(bg)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Style::default().bg(bg),
                );
            }
            let hint = row_at(area, 7);
            if hint.height > 0 {
                ui.line(
                    hint,
                    Line::from(Span::styled(
                        format!("  {}   Ctrl-B open · Ctrl-K copy", prompt.uri),
                        dim,
                    )),
                    Style::default().bg(bg),
                );
            }
        }
    }

    /// One registry row as a live form field.
    fn settings_field(
        &self,
        cfg: &AppConfig,
        st: &SettingsState,
        d: &settings_defs::Def,
    ) -> crate::ui::kit::form::Field {
        use crate::ui::kit::form::{Field, Kind};
        use settings_defs::Kind as DKind;

        let cursor = if settings_defs::index_of(d.field) == st.field {
            st.cursor
        } else {
            0
        };
        let kind = match d.kind {
            DKind::Toggle => Kind::Toggle {
                on: self.settings_toggle(st, d.field),
            },
            DKind::Select => Kind::Select {
                // The current value only: the list is cycled through the
                // arrows, which is how these have always been edited.
                options: vec![self.settings_select_label(cfg, st, d.field)],
                index: 0,
            },
            DKind::Secret => {
                let (value, from_env) = self.settings_secret(st, d.field);
                Kind::Secret {
                    value,
                    cursor,
                    from_env,
                }
            }
            DKind::Number { min, max } => Kind::Number {
                value: self
                    .settings_text(st, d.field)
                    .trim()
                    .parse::<i64>()
                    .unwrap_or(min),
                min,
                max,
            },
            DKind::Text => Kind::Text {
                value: self.settings_text(st, d.field),
                cursor,
                placeholder: "unset".into(),
            },
        };
        Field::new(d.label, kind)
            .help(d.help)
            .disabled(self.settings_disabled(st, d.field))
    }

    /// The live value of a toggle row.
    fn settings_toggle(&self, st: &SettingsState, f: SField) -> bool {
        match f {
            SField::ParallelLookahead => st.parallel_lookahead,
            SField::PrepassExtract => st.prepass_extract,
            SField::CoherenceCheck => st.coherence_check,
            SField::SystemOneEnabled => st.system_one.enabled,
            other => settings_defs::feature_of(other)
                .map(|feat| st.system_one.armed(feat))
                .unwrap_or(false),
        }
    }

    /// The label a cycle row currently shows.
    fn settings_select_label(
        &self,
        _cfg: &AppConfig,
        st: &SettingsState,
        f: SField,
    ) -> String {
        match f {
            SField::OrchProvider => st.models.orchestrator.provider.label().to_string(),
            SField::TransProvider => st.models.translator.provider.label().to_string(),
            SField::ReviewProvider => st.models.reviewer.provider.label().to_string(),
            SField::RefineProvider => st.models.refine.provider.label().to_string(),
            SField::OrchEffort => crate::model::Effort::label(st.models.orchestrator.effort).to_string(),
            SField::TransEffort => crate::model::Effort::label(st.models.translator.effort).to_string(),
            SField::ReviewEffort => crate::model::Effort::label(st.models.reviewer.effort).to_string(),
            SField::RefineEffort => crate::model::Effort::label(st.models.refine.effort).to_string(),
            SField::PreferredLanguageField => st.preferred_language.label().to_string(),
            SField::ServiceTierField => {
                crate::model::ServiceTier::label(st.service_tier).to_string()
            }
            SField::GateMode => st.system_one.review_gate.label().to_string(),
            SField::GateProvider => st.system_one.provider.label().to_string(),
            // "Terminal (adaptive) · adaptive" says it twice and overflows the
            // stepper's value column; the name alone already carries the tone.
            SField::Theme => {
                let (name, tone) = (st.theme.label(), st.theme.tone());
                if name.to_lowercase().contains(tone) {
                    name.to_string()
                } else {
                    format!("{name} · {tone}")
                }
            }
            SField::UpdateModeField => st.update_mode.label().to_string(),
            SField::ReleaseChannelField => st.release_channel.label().to_string(),
            _ => String::new(),
        }
    }

    /// A secret row's value, and whether the environment already supplies it.
    fn settings_secret(&self, st: &SettingsState, f: SField) -> (String, bool) {
        match f {
            SField::OpenRouterKey => (st.openrouter_key.clone(), st.api_key_env),
            SField::TokenrouterKey => (st.tokenrouter_key.clone(), st.tokenrouter_key_env),
            SField::GoogleKey => (st.google_key.clone(), st.google_key_env),
            SField::CloudflareToken => (
                st.cloudflare_api_token.clone(),
                st.cloudflare_api_token_env,
            ),
            SField::GateKey => (st.typesafe_key.clone(), st.typesafe_key_env),
            _ => (String::new(), false),
        }
    }

    /// A text or numeric row's current contents.
    fn settings_text(&self, st: &SettingsState, f: SField) -> String {
        match f {
            SField::OrchModel => st.models.orchestrator.model.clone(),
            SField::TransModel => st.models.translator.model.clone(),
            SField::ReviewModel => st.models.reviewer.model.clone(),
            SField::RefineModel => st.models.refine.model.clone(),
            SField::CloudflareAccount => st.cloudflare_account_id.clone(),
            SField::MaxAttempts => st.max_attempts.clone(),
            SField::ContinuitySentences => st.continuity_sentences.clone(),
            SField::LoopStall => st.loop_stall_secs.clone(),
            SField::Retranslates => st.max_chapter_retranslates.clone(),
            SField::ChunkTargetTokens => st.chunk_target_tokens.clone(),
            SField::ChunkHardCapTokens => st.chunk_hard_cap_tokens.clone(),
            SField::GateModel => st.system_one.model.clone(),
            SField::GateConfidence => st.system_one_confidence.clone(),
            _ => String::new(),
        }
    }

    /// Whether a row is currently inert.
    ///
    /// The System One block is the only case: with the master switch off, every
    /// judgement row below it does nothing, and saying so is better than
    /// letting someone set five toggles that have no effect.
    fn settings_disabled(&self, st: &SettingsState, f: SField) -> bool {
        if matches!(f, SField::SystemOneEnabled) {
            return false;
        }
        let in_block = settings_defs::feature_of(f).is_some()
            || matches!(
                f,
                SField::GateMode
                    | SField::GateProvider
                    | SField::GateModel
                    | SField::GateKey
                    | SField::GateConfidence
            );
        in_block && !st.system_one.enabled
    }

    /// The import wizard.
    ///
    /// The frame is a fixed height across every step on purpose: a modal that
    /// resized as you advanced would move the controls out from under the
    /// pointer. Completed steps on the rail are clickable, so going back is one
    /// click rather than the right number of Escapes.
    fn render_import_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &ImportState) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::modal::{Modal, Sizing};
        use crate::ui::kit::progress::{self, Step, StepState};

        let title = thai_display_safe(if st.lock_name {
            "Add volume · เพิ่มเล่ม"
        } else {
            "New project · นำเข้าไฟล์"
        });
        // A fixed height, because the frame must not resize as the user
        // advances — but sized to the tallest step rather than to the terminal,
        // which left most of the panel empty on every step.
        let frame = Modal::new(&title)
            .sizing(Sizing::medium().fit_width(62))
            .fixed_height(18)
            .footer(1)
            .render(ui, area);

        // The rail shows only the steps this flow actually visits.
        let visible = st.visible_steps();
        let steps: Vec<Step> = visible
            .iter()
            .map(|&s| {
                let state = if s < st.step {
                    StepState::Done
                } else if s == st.step {
                    StepState::Current
                } else if s.is_optional() {
                    StepState::Optional
                } else {
                    StepState::Ahead
                };
                Step::new(s.label(), state)
            })
            .collect();
        progress::stepper(ui, row_at(frame.body, 0), &steps);

        ui.line(
            row_at(frame.body, 1),
            import_context_line(st, ui.theme),
            Style::default().bg(ui.theme.bg_elevated),
        );

        let body = Rect {
            y: frame.body.y + 3,
            height: frame.body.height.saturating_sub(3),
            ..frame.body
        };
        self.render_import_body(ui, body, st);
        self.render_import_footer(ui, frame.footer, st);
    }

    fn render_import_body(
        &self,
        ui: &mut crate::ui::kit::Ui,
        body: Rect,
        st: &ImportState,
    ) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::progress;

        let bg = ui.theme.bg_elevated;
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);
        let ink = Style::default().fg(ui.theme.ink).bg(bg);

        match st.step {
            ImportStep::Pick => {
                if st.files.is_empty() {
                    ui.text(
                        row_at(body, 0),
                        "No importable files in this folder — press r to rescan.",
                        dim,
                    );
                    ui.text(
                        row_at(body, 2),
                        format!(
                            "Accepted: {}",
                            crate::document_import::supported_import_summary()
                        ),
                        dim,
                    );
                    return;
                }
                let mut ls = ListState::new();
                ls.select(Some(st.sel));
                list::render(
                    ui,
                    body,
                    &mut ls,
                    st.files.len(),
                    list::Opts {
                        rail: true,
                        scrollbar: true,
                        kind: ZoneKind::Row,
                        id_base: 0,
                    },
                    |i| {
                        let (path, size) = &st.files[i];
                        // A filename can be Thai, and every string reaching
                        // the screen has to be decomposed first or the cells
                        // drift.
                        let name = thai_display_safe(
                            &path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                        );
                        Row::new(Line::from(vec![
                            Span::raw(name),
                            Span::styled(format!("   {}", human_size(*size)), dim),
                        ]))
                    },
                );
            }
            ImportStep::Name => {
                ui.text(row_at(body, 0), "What is this project called?", dim);
                let (before, after) = input::caret_halves(
                    &st.name,
                    st.name_cursor,
                    body.width.saturating_sub(2) as usize,
                );
                ui.line(
                    row_at(body, 2),
                    Line::from(vec![
                        Span::styled(before, ink),
                        Span::styled(
                            crate::ui::glyphs::ACCENT_RAIL.as_str().to_string(),
                            Style::default().fg(ui.theme.stream_cursor).bg(bg),
                        ),
                        Span::styled(after, ink),
                    ]),
                    Style::default().bg(bg),
                );
                if let Some(note) = st.note {
                    ui.text(
                        row_at(body, 4),
                        note,
                        Style::default().fg(ui.theme.status_warn).bg(bg),
                    );
                }
            }
            ImportStep::Title => {
                self.render_syn_body(ui, body, &st.title_syn, "Title 日本語");
            }
            ImportStep::Volume => {
                ui.text(row_at(body, 0), "Which volume is this?", dim);
                ui.line(
                    row_at(body, 2),
                    Line::from(vec![
                        Span::styled("  ", ink),
                        Span::styled(
                            format!("Vol.{:02}", st.vol),
                            Style::default()
                                .fg(ui.theme.accent)
                                .bg(bg)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Style::default().bg(bg),
                );
                ui.text(row_at(body, 4), "↑↓ or type a number", dim);
                if let Some(target) = st.target_project()
                    && !target.volumes.is_empty()
                {
                    ui.text(
                        row_at(body, 6),
                        format!("Already has  {}", volume_chips(&target.volumes)),
                        dim,
                    );
                }
            }
            ImportStep::Synopsis => {
                self.render_syn_body(ui, body, &st.syn, "Synopsis 日本語");
            }
            ImportStep::Importing => {
                let (done, total, what) =
                    st.progress.clone().unwrap_or((0, 0, "starting".into()));
                ui.text(row_at(body, 0), format!("Importing · {what}"), ink);
                progress::bar_with_label(ui, row_at(body, 2), done, total);
                ui.text(
                    row_at(body, 4),
                    "Chapters are read in spine order and illustrations relocated.",
                    dim,
                );
            }
        }
    }

    /// Back / Skip / Next, as real buttons.
    fn render_import_footer(
        &self,
        ui: &mut crate::ui::kit::Ui,
        footer: Rect,
        st: &ImportState,
    ) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::modal;

        if st.step == ImportStep::Importing {
            modal::footer_hint(ui, footer, "  import continues in the background");
            return;
        }
        let mut buttons = vec![Button::new(ZoneId::button(DIALOG_CANCEL), "Back").accel("esc")];
        if matches!(st.step, ImportStep::Title | ImportStep::Synopsis) {
            buttons.push(
                Button::new(ZoneId::button(DIALOG_ALTERNATE), "Translate").accel("tab"),
            );
        }
        // One forward button, labelled for what it will actually do. A separate
        // Skip would be a second button doing the same thing as Next, since an
        // optional step advances either way.
        let empty_optional = match st.step {
            ImportStep::Title => st.title_syn.translated_text.trim().is_empty(),
            ImportStep::Synopsis => st.syn.raw.trim().is_empty(),
            _ => false,
        };
        let next_label = match st.step {
            ImportStep::Synopsis if !empty_optional => "Import",
            ImportStep::Synopsis => "Skip · import",
            s if s.is_optional() && empty_optional => "Skip",
            _ => "Next",
        };
        buttons.push(
            Button::new(ZoneId::button(DIALOG_CONFIRM), next_label)
                .accel("↵")
                .primary()
                .disabled(st.step == ImportStep::Pick && st.files.is_empty()),
        );
        modal::render_footer(ui, footer, ButtonRow::new(buttons));
    }

    /// A confirm dialog, with its choices as real buttons.
    fn render_modal_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, dlg: &Dialog) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new(&dlg.title)
            .sizing(Sizing::small())
            .footer(1)
            .closable(false)
            .render(ui, area);

        let wrapped = crate::ui::kit::editor::wrap(&dlg.body, frame.body.width);
        for (n, range) in wrapped.iter().enumerate() {
            if (n as u16) >= frame.body.height.saturating_sub(1) {
                break;
            }
            ui.text(
                crate::ui::kit::ctx::row_at(frame.body, n as u16),
                thai_display_safe(&dlg.body[range.clone()]),
                Style::default()
                    .fg(ui.theme.ink_soft)
                    .bg(ui.theme.bg_elevated),
            );
        }

        let mut buttons = vec![Button::new(ZoneId::button(DIALOG_CANCEL), "Cancel").accel("esc")];
        if let Some(alt) = &dlg.alternate {
            buttons.push(
                Button::new(ZoneId::button(DIALOG_ALTERNATE), alt.label.clone())
                    .accel(alt.key.to_string()),
            );
        }
        buttons.push(
            Button::new(ZoneId::button(DIALOG_CONFIRM), dlg.confirm_label.clone())
                .accel("↵")
                .primary(),
        );
        modal::render_footer(ui, frame.footer, ButtonRow::new(buttons));
    }

    /// Export picker: a format checklist, then progress, then results.
    fn render_export_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &ExportState) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::modal::{self, Modal, Sizing};
        use crate::ui::kit::{badge::Chip, progress};

        let frame = Modal::new("Export volume")
            .sizing(Sizing::medium())
            .subtitle(format!("Vol.{:02}", st.vol))
            .footer(1)
            .render(ui, area);

        if let Some((written, warnings)) = &st.done {
            let mut n = 0u16;
            ui.text(
                crate::ui::kit::ctx::row_at(frame.body, n),
                format!("Wrote {} file(s)", written.len()),
                Style::default()
                    .fg(ui.theme.status_done)
                    .bg(ui.theme.bg_elevated)
                    .add_modifier(Modifier::BOLD),
            );
            n += 1;
            for path in written.iter().take(frame.body.height.saturating_sub(2) as usize) {
                ui.text(
                    crate::ui::kit::ctx::row_at(frame.body, n),
                    truncate_cols(&path.display().to_string(), frame.body.width as usize),
                    Style::default()
                        .fg(ui.theme.ink_soft)
                        .bg(ui.theme.bg_elevated),
                );
                n += 1;
            }
            for w in warnings.iter().take(2) {
                ui.text(
                    crate::ui::kit::ctx::row_at(frame.body, n),
                    truncate_cols(w, frame.body.width as usize),
                    Style::default()
                        .fg(ui.theme.status_warn)
                        .bg(ui.theme.bg_elevated),
                );
                n += 1;
            }
            modal::render_footer(
                ui,
                frame.footer,
                ButtonRow::new(vec![
                    Button::new(ZoneId::button(DIALOG_CONFIRM), "Close")
                        .accel("↵")
                        .primary(),
                ]),
            );
            return;
        }

        if let Some((done, total, what)) = &st.progress {
            ui.text(
                crate::ui::kit::ctx::row_at(frame.body, 0),
                format!("Exporting {what}…"),
                Style::default()
                    .fg(ui.theme.ink_soft)
                    .bg(ui.theme.bg_elevated),
            );
            progress::bar_with_label(
                ui,
                crate::ui::kit::ctx::row_at(frame.body, 2),
                *done,
                *total,
            );
            modal::footer_hint(ui, frame.footer, "  export continues in the background");
            return;
        }

        for (i, fmt) in crate::export::ExportFormat::ALL.iter().enumerate() {
            let row = crate::ui::kit::ctx::row_at(frame.body, i as u16);
            if row.height == 0 {
                break;
            }
            Chip::new(ZoneId::row(i), fmt.label(), st.formats[i]).render(ui, row);
            let desc_x = row.x + 18;
            if desc_x < row.x + row.width {
                ui.text(
                    Rect {
                        x: desc_x,
                        width: row.width - 18,
                        ..row
                    },
                    export_desc(*fmt),
                    Style::default()
                        .fg(ui.theme.ink_faint)
                        .bg(ui.theme.bg_elevated),
                );
            }
        }

        let any = st.formats.iter().any(|b| *b);
        modal::render_footer(
            ui,
            frame.footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(DIALOG_CANCEL), "Cancel").accel("esc"),
                Button::new(ZoneId::button(DIALOG_CONFIRM), "Export")
                    .accel("↵")
                    .primary()
                    .disabled(!any),
            ]),
        );
    }

    /// Theme picker. The selection previews live, so the list doubles as the
    /// preview surface and needs no separate swatch.
    fn render_theme_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &ThemePickerState) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Theme")
            .sizing(Sizing::medium())
            .subtitle("↵ keep · Esc revert")
            .footer(1)
            .render(ui, area);

        let mut list_state = ListState::new();
        list_state.select(Some(st.sel));
        let dim = Style::default().fg(ui.theme.ink_faint);
        list::render(
            ui,
            frame.body,
            &mut list_state,
            ALL_THEMES.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let id = ALL_THEMES[i];
                Row::new(Line::from(vec![
                    // Padded by display column, not by character: "Washi 和紙" is
                    // eight chars but ten columns, so `{:<22}` left it two
                    // columns wider than its neighbours.
                    Span::raw(pad_to_cols(id.label(), 22)),
                    Span::styled(id.tone().to_string(), dim),
                ]))
            },
        );
        modal::footer_hint(
            ui,
            frame.footer,
            "  the selection previews live — Esc puts the old one back",
        );
    }
}

/// The command bar: every action that has a name, one search away.
fn render_palette(ui: &mut crate::ui::kit::Ui, area: Rect, st: &mut PaletteState) {
    use crate::ui::kit::modal::{Modal, Sizing};

    let items = st.picker_items();
    let matches = crate::ui::kit::picker::filter(&st.picker.query, &items);
    // Query line, rule, and one row per match — capped so a long list still
    // scrolls rather than growing past the terminal.
    let rows = (matches.len() as u16).clamp(1, 14) + 2 + 4;
    let frame = Modal::new("Command bar")
        .sizing(Sizing::medium().fit_height(rows))
        .subtitle(format!("{} of {}", matches.len(), st.items.len()))
        .render(ui, area);
    crate::ui::kit::picker::render(ui, frame.body, &mut st.picker, &items, "Type a command…");
}

/// The Reader's jump list: chapters, sections and bookmarks together.
fn render_reader_jump(ui: &mut crate::ui::kit::Ui, area: Rect, st: &mut ReaderJumpState) {
    use crate::ui::kit::modal::{Modal, Sizing};

    let items = st.picker_items();
    let matches = crate::ui::kit::picker::filter(&st.picker.query, &items);
    let rows = (matches.len() as u16).clamp(1, 16) + 2 + 4;
    let frame = Modal::new("Jump to")
        .sizing(Sizing::medium().fit_height(rows))
        .subtitle(truncate_cols(&thai_display_safe(&st.title), 40))
        .render(ui, area);
    crate::ui::kit::picker::render(
        ui,
        frame.body,
        &mut st.picker,
        &items,
        "Chapter · section · bookmark…",
    );
}

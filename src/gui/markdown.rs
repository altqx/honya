//! honya's markdown, drawn in the window.
//!
//! The GUI printed raw markdown into labels — `**bold**`, `[REVIEW NEEDED]`
//! banners, ruby parentheses and image links, all as literal text. The parser
//! and the styling rules already exist in `ui::markdown`; only the sink was
//! missing. This converts its output — ratatui `Line`s that already carry
//! colour and emphasis — into an egui `LayoutJob`, so both front ends render
//! the same document by the same rules and only the last step differs.

use std::hash::{Hash, Hasher};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, TextStyle, Ui};
use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::theme::Theme;

/// Rendered lines, kept until the text, the width or the theme changes.
///
/// Parsing is the expensive half, and a pane redraws on every animation tick.
#[derive(Default)]
pub struct MarkdownCache {
    key: Option<u64>,
    job: LayoutJob,
}

/// Wide enough that the parser does not hard-wrap: egui wraps by pixels, and
/// wrapping twice would leave prose ragged at a column boundary nothing here
/// can see.
const NO_WRAP: usize = 100_000;

fn key_for(md: &str, theme: &Theme, base: Color32, font: &FontId) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    md.len().hash(&mut h);
    md.as_bytes().iter().rev().take(64).for_each(|b| b.hash(&mut h));
    crate::ui::markdown::theme_fingerprint(theme).hash(&mut h);
    base.to_array().hash(&mut h);
    font.size.to_bits().hash(&mut h);
    h.finish()
}

impl MarkdownCache {
    fn job(&mut self, md: &str, theme: &Theme, base: Color32, font: FontId) -> &LayoutJob {
        let key = key_for(md, theme, base, &font);
        if self.key != Some(key) {
            // The parser wants a ratatui colour for its base ink; the palette
            // was built from the theme, so read the slot rather than convert
            // back and risk a different answer.
            let lines = crate::ui::markdown::render(md, theme.translated_text, theme, NO_WRAP);
            self.job = to_job(&lines, base, font);
            self.key = Some(key);
        }
        &self.job
    }
}

/// Draw `md` as honya renders it, wrapping to the available width.
pub fn show(ui: &mut Ui, cache: &mut MarkdownCache, md: &str, theme: &Theme, base: Color32) {
    show_with(
        ui,
        cache,
        md,
        theme,
        base,
        &Options {
            wrap: true,
            ..Default::default()
        },
    );
}

/// How a pane wants its document drawn.
#[derive(Default)]
pub struct Options<'a> {
    /// False lets long lines run off the edge instead of folding — what the
    /// Reader's wrap toggle asks for.
    pub wrap: bool,
    /// Surfaces to tint wherever they appear, for the glossary highlight.
    pub highlight: &'a [String],
    pub tint: Option<Color32>,
}

pub fn show_with(
    ui: &mut Ui,
    cache: &mut MarkdownCache,
    md: &str,
    theme: &Theme,
    base: Color32,
    opts: &Options<'_>,
) {
    let font = TextStyle::Body.resolve(ui.style());
    let mut job = cache.job(md, theme, base, font).clone();
    job.wrap.max_width = if opts.wrap {
        ui.available_width()
    } else {
        f32::INFINITY
    };
    if let Some(tint) = opts.tint
        && !opts.highlight.is_empty()
    {
        tint_terms(&mut job, opts.highlight, tint);
    }
    ui.label(job);
}

/// Recolour every occurrence of `terms` in an already-laid-out job.
///
/// Splitting sections rather than re-parsing: the document's own styling is
/// whatever the markdown said, and a highlight is a second opinion about the
/// same characters.
fn tint_terms(job: &mut LayoutJob, terms: &[String], tint: Color32) {
    let text = job.text.clone();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        if term.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(at) = text[from..].find(term.as_str()) {
            let start = from + at;
            cuts.push((start, start + term.len()));
            from = start + term.len();
        }
    }
    if cuts.is_empty() {
        return;
    }
    cuts.sort_unstable();
    let mut out = LayoutJob {
        text: String::new(),
        wrap: job.wrap.clone(),
        ..Default::default()
    };
    let mut cut = cuts.into_iter().peekable();
    for section in &job.sections {
        let (mut at, end) = (section.byte_range.start, section.byte_range.end);
        while at < end {
            // Skip any hit that ended before this section begins.
            while cut.peek().is_some_and(|(_, e)| *e <= at) {
                cut.next();
            }
            let next = cut.peek().copied();
            match next {
                Some((s, e)) if s <= at => {
                    let stop = e.min(end);
                    let mut fmt = section.format.clone();
                    fmt.color = tint;
                    out.append(&text[at..stop], 0.0, fmt);
                    at = stop;
                }
                Some((s, _)) if s < end => {
                    out.append(&text[at..s], 0.0, section.format.clone());
                    at = s;
                }
                _ => {
                    out.append(&text[at..end], 0.0, section.format.clone());
                    at = end;
                }
            }
        }
    }
    *job = out;
}

fn to_job(lines: &[Line<'static>], base: Color32, font: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            job.append("\n", 0.0, TextFormat { font_id: font.clone(), color: base, ..Default::default() });
        }
        for span in &line.spans {
            job.append(&span.content, 0.0, format_of(&span.style, base, &font));
        }
    }
    job
}

fn format_of(style: &ratatui::style::Style, base: Color32, font: &FontId) -> TextFormat {
    let m = style.add_modifier;
    TextFormat {
        font_id: font.clone(),
        color: style
            .fg
            .map(super::theme_map::color)
            .unwrap_or(base),
        background: style
            .bg
            .map(super::theme_map::color)
            .unwrap_or(Color32::TRANSPARENT),
        italics: m.contains(Modifier::ITALIC),
        underline: if m.contains(Modifier::UNDERLINED) {
            egui::Stroke::new(1.0_f32, base)
        } else {
            egui::Stroke::NONE
        },
        strikethrough: if m.contains(Modifier::CROSSED_OUT) {
            egui::Stroke::new(1.0_f32, base)
        } else {
            egui::Stroke::NONE
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::Span;

    fn theme() -> Theme {
        Theme::washi()
    }

    #[test]
    fn a_styled_span_keeps_its_colour_and_emphasis_through_the_adapter() {
        let base = Color32::from_rgb(1, 2, 3);
        let font = FontId::proportional(14.0);
        let red = ratatui::style::Color::Rgb(200, 30, 30);
        let lines = vec![Line::from(vec![
            Span::styled("plain", Style::default()),
            Span::styled(
                "shouted",
                Style::default().fg(red).add_modifier(Modifier::ITALIC),
            ),
        ])];
        let job = to_job(&lines, base, font);
        assert_eq!(job.text, "plainshouted");
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[0].format.color, base, "unstyled falls to base");
        assert!(!job.sections[0].format.italics);
        assert_eq!(
            job.sections[1].format.color,
            Color32::from_rgb(200, 30, 30)
        );
        assert!(job.sections[1].format.italics);
    }

    #[test]
    fn lines_are_joined_with_the_newline_that_separates_them() {
        let job = to_job(
            &[Line::raw("one"), Line::raw("two")],
            Color32::WHITE,
            FontId::proportional(14.0),
        );
        assert_eq!(job.text, "one\ntwo");
    }

    #[test]
    fn the_document_is_parsed_once_and_reused_until_something_changes() {
        let mut cache = MarkdownCache::default();
        let t = theme();
        let font = FontId::proportional(14.0);
        let md = "**bold** and *italic*";

        cache.job(md, &t, Color32::WHITE, font.clone());
        let first = cache.key;
        cache.job(md, &t, Color32::WHITE, font.clone());
        assert_eq!(cache.key, first, "the same document does not re-parse");

        cache.job("different", &t, Color32::WHITE, font.clone());
        assert_ne!(cache.key, first);

        // A theme change has to invalidate too: the colours are baked in.
        let before = cache.key;
        cache.job("different", &Theme::sumi(), Color32::WHITE, font);
        assert_ne!(cache.key, before);
    }

    #[test]
    fn markdown_emphasis_does_not_reach_the_reader_as_asterisks() {
        let mut cache = MarkdownCache::default();
        let job = cache.job(
            "this is **bold** text",
            &theme(),
            Color32::WHITE,
            FontId::proportional(14.0),
        );
        assert!(
            !job.text.contains("**"),
            "the markers are styling, not text: {:?}",
            job.text
        );
        assert!(job.text.contains("bold"));
    }

    #[test]
    fn a_highlighted_term_is_recoloured_without_losing_the_rest() {
        let base = Color32::WHITE;
        let tint = Color32::from_rgb(9, 9, 9);
        let font = FontId::proportional(14.0);
        let mut job = to_job(&[Line::raw("the 先輩 smiled at the 先輩")], base, font);
        let before = job.text.clone();

        tint_terms(&mut job, &["先輩".to_string()], tint);
        assert_eq!(job.text, before, "tinting must not change the text");

        let tinted: String = job
            .sections
            .iter()
            .filter(|s| s.format.color == tint)
            .map(|s| job.text[s.byte_range.clone()].to_string())
            .collect();
        assert_eq!(tinted, "先輩先輩", "both occurrences, and only those");
    }

    #[test]
    fn tinting_a_term_that_is_not_there_changes_nothing() {
        let mut job = to_job(
            &[Line::raw("nothing to see")],
            Color32::WHITE,
            FontId::proportional(14.0),
        );
        let before = job.sections.len();
        tint_terms(&mut job, &["先輩".to_string()], Color32::RED);
        assert_eq!(job.sections.len(), before);
    }

    #[test]
    fn an_empty_term_does_not_match_everywhere() {
        let mut job = to_job(
            &[Line::raw("abc")],
            Color32::WHITE,
            FontId::proportional(14.0),
        );
        tint_terms(&mut job, &[String::new()], Color32::RED);
        assert_eq!(job.text, "abc");
    }
}

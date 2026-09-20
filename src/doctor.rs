//! `honya doctor` — what this terminal can actually do, and what to do about it.
//!
//! The UI adapts to the terminal in several ways that are invisible when they
//! work and baffling when they do not: colours are quantized to the detected
//! depth, mouse reporting depends on the terminal passing events through, and
//! several glyphs are East Asian *Ambiguous*, meaning they occupy one column in
//! a Latin locale and two in a CJK one. When honya looks wrong, the cause is
//! almost always one of those, and none of them are things a user can be
//! expected to guess at.
//!
//! So each check reports what was found, whether it is a problem, and — when it
//! is — the specific thing to change. Exit status is zero unless a check
//! actually fails, so this is usable in a script.

use crate::theme::quantize::ColorDepth;
use crate::ui::glyphs;
use crate::ui::text::col_width;

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Working as intended.
    Good,
    /// Usable, but something is being given up.
    Degraded,
    /// Broken; the UI will misbehave.
    Bad,
}

impl Verdict {
    fn mark(self) -> &'static str {
        match self {
            Verdict::Good => "ok  ",
            Verdict::Degraded => "note",
            Verdict::Bad => "FAIL",
        }
    }
}

struct Check {
    name: &'static str,
    verdict: Verdict,
    found: String,
    /// What to do about it, when there is something to do.
    fix: Option<String>,
}

impl Check {
    fn good(name: &'static str, found: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Good,
            found: found.into(),
            fix: None,
        }
    }

    fn degraded(name: &'static str, found: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Degraded,
            found: found.into(),
            fix: Some(fix.into()),
        }
    }

    fn bad(name: &'static str, found: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Bad,
            found: found.into(),
            fix: Some(fix.into()),
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Which multiplexer, if any, is between honya and the terminal.
///
/// Worth naming because both re-encode mouse events into their own stream, so
/// a mouse problem inside one is usually the multiplexer's configuration
/// rather than the terminal's.
fn multiplexer() -> Option<&'static str> {
    if env("TMUX").is_some() {
        Some("tmux")
    } else if env("ZELLIJ").is_some() {
        Some("zellij")
    } else if env("STY").is_some() {
        Some("screen")
    } else {
        None
    }
}

fn check_color() -> Check {
    let depth = ColorDepth::detect();
    let signals = [
        env("NO_COLOR").map(|v| format!("NO_COLOR={v}")),
        env("HONYA_COLOR_DEPTH").map(|v| format!("HONYA_COLOR_DEPTH={v}")),
        env("COLORTERM").map(|v| format!("COLORTERM={v}")),
        env("TERM").map(|v| format!("TERM={v}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ");
    let found = format!("{depth:?}  ({signals})");

    match depth {
        ColorDepth::TrueColor => Check::good("color depth", found),
        ColorDepth::Indexed256 => Check::degraded(
            "color depth",
            found,
            "themes are mapped to the 256-colour cube. For exact colours, set \
             COLORTERM=truecolor if your terminal supports it.",
        ),
        ColorDepth::Ansi16 => Check::degraded(
            "color depth",
            found,
            "only 16 colours: palettes are matched by hue, so shades collapse \
             but success and failure stay distinguishable. Set TERM to a \
             256color variant if your terminal supports one.",
        ),
        ColorDepth::Mono => Check::degraded(
            "color depth",
            found,
            "no colour at all. State is carried by bold and reverse video \
             instead. Unset NO_COLOR to restore colour.",
        ),
    }
}

fn check_size() -> Check {
    match ratatui::crossterm::terminal::size() {
        Ok((w, h)) if w >= 80 && h >= 24 => {
            Check::good("terminal size", format!("{w}x{h}"))
        }
        Ok((w, h)) if w >= 60 => Check::degraded(
            "terminal size",
            format!("{w}x{h}"),
            "below 80x24 the layout drops to a single pane and compacts itself. \
             Usable, but side panels are hidden.",
        ),
        Ok((w, h)) => Check::bad(
            "terminal size",
            format!("{w}x{h}"),
            "narrower than 60 columns; content will be heavily truncated. \
             Widen the window.",
        ),
        Err(e) => Check::bad(
            "terminal size",
            format!("unreadable ({e})"),
            "honya needs a real TTY. Run it directly rather than through a pipe.",
        ),
    }
}

fn check_keyboard() -> Check {
    match ratatui::crossterm::terminal::supports_keyboard_enhancement() {
        Ok(true) => Check::good("keyboard protocol", "enhancement supported"),
        Ok(false) => Check::degraded(
            "keyboard protocol",
            "legacy encoding",
            "Ctrl-Tab and some modified keys cannot be told apart from their \
             unmodified forms. Everything has an alternative binding; press ? \
             for the list.",
        ),
        Err(e) => Check::degraded(
            "keyboard protocol",
            format!("could not query ({e})"),
            "assuming legacy encoding, which only costs a few modified keys.",
        ),
    }
}

fn check_mouse() -> Check {
    match multiplexer() {
        Some("tmux") => Check::degraded(
            "mouse",
            "reporting requested, via tmux",
            "if clicks do nothing, tmux is swallowing them: set `set -g mouse on` \
             in your tmux.conf.",
        ),
        Some(name) => Check::degraded(
            "mouse",
            format!("reporting requested, via {name}"),
            format!("{name} re-encodes mouse events; if clicks do nothing, enable \
                     mouse support in its configuration."),
        ),
        None => Check::good("mouse", "reporting requested directly"),
    }
}

/// Ambiguous-width glyphs are the one hazard specific to this app's audience.
fn check_glyph_width() -> Check {
    // If the measurement disagrees with what the glyph declares, the host's
    // width tables are treating these as wide — which is what a CJK locale does.
    let sample = glyphs::MOON_FULL;
    let measured = col_width(sample.as_str());
    if measured as u16 == sample.cols() {
        Check::good(
            "glyph width",
            format!("status glyphs measure {measured} column"),
        )
    } else {
        Check::bad(
            "glyph width",
            format!(
                "status glyphs measure {measured} columns but the layout assumes {}",
                sample.cols()
            ),
            "your locale treats East Asian Ambiguous characters as wide, which \
             shifts every column after them. Set HONYA_GLYPHS=ascii for an \
             unambiguously single-column glyph set.",
        )
    }
}

fn check_locale() -> Check {
    let lang = env("LC_ALL").or_else(|| env("LC_CTYPE")).or_else(|| env("LANG"));
    match lang {
        Some(l) if l.to_ascii_lowercase().contains("utf") => {
            Check::good("locale", l)
        }
        Some(l) => Check::degraded(
            "locale",
            l,
            "not a UTF-8 locale; Japanese and Thai text may not render. Set \
             LANG to a UTF-8 variant.",
        ),
        None => Check::degraded(
            "locale",
            "unset",
            "no locale set; UTF-8 is assumed. Set LANG if text renders as boxes.",
        ),
    }
}

/// Run every check and print the report. Returns a non-zero-worthy error only
/// when something actually fails.
pub fn run() -> anyhow::Result<()> {
    let checks = vec![
        check_size(),
        check_color(),
        check_glyph_width(),
        check_locale(),
        check_mouse(),
        check_keyboard(),
    ];

    println!("honya {}", crate::update::version_string());
    if let Some(mux) = multiplexer() {
        println!("running inside {mux}");
    }
    println!();

    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut failed = 0usize;
    let mut notes = Vec::new();
    for c in &checks {
        println!("  {}  {:<width$}  {}", c.verdict.mark(), c.name, c.found);
        if c.verdict == Verdict::Bad {
            failed += 1;
        }
        if let Some(fix) = &c.fix {
            notes.push((c.name, fix.clone()));
        }
    }

    if !notes.is_empty() {
        println!();
        for (name, fix) in notes {
            println!("  {name}:");
            for line in wrap_words(&fix, 72) {
                println!("    {line}");
            }
        }
    }

    println!();
    if failed == 0 {
        println!("No problems found.");
        Ok(())
    } else {
        anyhow::bail!("{failed} check(s) failed")
    }
}

/// Wrap `text` to `width` columns at word boundaries.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && col_width(&line) + 1 + col_width(word) > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_comes_with_something_to_do_about_it() {
        // A report that says something is wrong without saying what to change
        // is not a diagnosis.
        for c in [
            check_size(),
            check_color(),
            check_glyph_width(),
            check_locale(),
            check_mouse(),
            check_keyboard(),
        ] {
            if c.verdict != Verdict::Good {
                assert!(
                    c.fix.as_ref().is_some_and(|f| f.len() > 20),
                    "{} reported {:?} with no useful fix",
                    c.name,
                    c.verdict
                );
            }
        }
    }

    #[test]
    fn checks_never_panic_and_always_report_something() {
        for c in [
            check_size(),
            check_color(),
            check_glyph_width(),
            check_locale(),
            check_mouse(),
            check_keyboard(),
        ] {
            assert!(!c.name.is_empty());
            assert!(!c.found.is_empty(), "{} found nothing to report", c.name);
        }
    }

    #[test]
    fn wrapping_keeps_every_word_and_respects_the_width() {
        let text = "your locale treats East Asian Ambiguous characters as wide, \
                    which shifts every column after them";
        let lines = wrap_words(text, 30);
        assert!(!lines.is_empty());
        for l in &lines {
            assert!(col_width(l) <= 30, "line too wide: {l:?}");
        }
        let round_trip: Vec<&str> = lines.iter().flat_map(|l| l.split(' ')).collect();
        let original: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(round_trip, original, "wrapping lost or reordered a word");
    }

    #[test]
    fn a_word_longer_than_the_width_still_gets_a_line() {
        let lines = wrap_words("short suuuuuuuuuuuuuuuuuuuperlong", 8);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("suuu"));
    }

    #[test]
    fn empty_text_wraps_to_nothing() {
        assert!(wrap_words("", 40).is_empty());
        assert!(wrap_words("   ", 40).is_empty());
    }
}

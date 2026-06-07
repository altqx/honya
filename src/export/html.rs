//! Render the parsed block IR to XHTML for the EPUB export. The output is the inner
//! `<body>` markup; `epub.rs` wraps it in the XHTML document shell. Image `src`s are
//! prefixed with `img_prefix` (e.g. `images/`) and only emitted for files in the
//! `embedded` set — a referenced-but-missing image degrades to its alt caption so the
//! EPUB stays valid (every `<img>` resolves to a manifested file).

use std::collections::HashSet;

use super::blocks::{Block, Inline};

/// Escape text for XML element content.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape text for an XML attribute value (adds quote escaping).
pub fn esc_attr(s: &str) -> String {
    esc(s).replace('"', "&quot;")
}

/// Render block IR to an XHTML body fragment.
pub fn blocks_to_xhtml(blocks: &[Block], img_prefix: &str, embedded: &HashSet<String>) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            Block::Heading { level, spans } => {
                let lvl = (*level).clamp(1, 6);
                out.push_str(&format!(
                    "<h{lvl}>{}</h{lvl}>\n",
                    inlines_to_xhtml(spans, img_prefix, embedded)
                ));
            }
            Block::Para(spans) => {
                out.push_str(&format!(
                    "<p>{}</p>\n",
                    inlines_to_xhtml(spans, img_prefix, embedded)
                ));
            }
            Block::Image { alt, file } => {
                out.push_str(&format!(
                    "<p class=\"illus\">{}</p>\n",
                    image_or_caption(alt, file, img_prefix, embedded)
                ));
            }
            Block::Rule => out.push_str("<hr/>\n"),
        }
    }
    out
}

fn inlines_to_xhtml(spans: &[Inline], img_prefix: &str, embedded: &HashSet<String>) -> String {
    let mut out = String::new();
    for span in spans {
        match span {
            Inline::Text(t) => out.push_str(&esc(t)),
            Inline::Bold(inner) => out.push_str(&format!(
                "<strong>{}</strong>",
                inlines_to_xhtml(inner, img_prefix, embedded)
            )),
            Inline::Italic(inner) => out.push_str(&format!(
                "<em>{}</em>",
                inlines_to_xhtml(inner, img_prefix, embedded)
            )),
            Inline::Image { alt, file } => {
                out.push_str(&image_or_caption(alt, file, img_prefix, embedded))
            }
        }
    }
    out
}

/// `<img>` when the file is embedded, else the alt text as a plain caption.
fn image_or_caption(alt: &str, file: &str, img_prefix: &str, embedded: &HashSet<String>) -> String {
    if embedded.contains(file) {
        format!(
            "<img src=\"{}{}\" alt=\"{}\"/>",
            img_prefix,
            esc_attr(file),
            esc_attr(alt)
        )
    } else if alt.is_empty() {
        String::new()
    } else {
        esc(alt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::blocks::parse_blocks;

    fn embedded(files: &[&str]) -> HashSet<String> {
        files.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn renders_subset_and_escapes() {
        let md = "# A & B\n\nfoo **bold** <x> *it*\n\n---";
        let html = blocks_to_xhtml(&parse_blocks(md), "images/", &embedded(&[]));
        assert!(html.contains("<h1>A &amp; B</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("&lt;x&gt;"));
        assert!(html.contains("<em>it</em>"));
        assert!(html.contains("<hr/>"));
    }

    #[test]
    fn image_emitted_only_when_embedded() {
        let md = "![cap](../../images/p.png)";
        let blocks = parse_blocks(md);
        let yes = blocks_to_xhtml(&blocks, "images/", &embedded(&["p.png"]));
        assert!(yes.contains("<img src=\"images/p.png\" alt=\"cap\"/>"));
        let no = blocks_to_xhtml(&blocks, "images/", &embedded(&[]));
        assert!(!no.contains("<img"));
        assert!(no.contains("cap")); // degrades to caption
    }
}

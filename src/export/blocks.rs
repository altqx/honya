//! A tiny block/inline parser for the *known* honya Markdown subset — exactly what
//! `cleanse` and the translator emit: ATX headings (`# `), blank-line-separated
//! paragraphs, thematic breaks (`---`), image links (`![alt](url)`), and `**bold**`
//! / `*italic*` emphasis. This is deliberately NOT a general CommonMark parser: the
//! producer and consumer are both in-tree, so the grammar is small and closed.
//!
//! Image URLs are reduced to their basename (the file under `images/`), since every
//! deliverable format re-homes images under its own directory. `referenced_images`
//! harvests those basenames; `rewrite_image_links` rewrites the raw Markdown's image
//! URLs to a new prefix for the merged-Markdown export (which never parses blocks).

use std::sync::OnceLock;

use regex::Regex;

/// One block-level element of a chapter body.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// ATX heading; `level` is 1..=6.
    Heading { level: u8, spans: Vec<Inline> },
    /// A normal paragraph of inline runs.
    Para(Vec<Inline>),
    /// A standalone illustration (a block that is just one image link).
    Image { alt: String, file: String },
    /// A thematic break / scene divider (`---`).
    Rule,
}

/// One inline run within a block.
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Image { alt: String, file: String },
}

/// Image-link regex: `![alt](url)`. `alt` may be empty; `url` is anything but `)`.
fn image_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"!\[([^\]]*)\]\(([^)]*)\)").expect("valid image regex"))
}

/// The file basename of an image URL (`../../images/m_001.png` → `m_001.png`).
pub fn image_basename(url: &str) -> String {
    let trimmed = url.trim();
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .to_string()
}

/// Every image basename referenced in `md`, in first-seen order, de-duplicated.
pub fn referenced_images(md: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for cap in image_re().captures_iter(md) {
        let file = image_basename(&cap[2]);
        if !file.is_empty() && !seen.contains(&file) {
            seen.push(file);
        }
    }
    seen
}

/// Rewrite every image URL in `md` to `{prefix}{basename}` (used by the merged
/// Markdown export, which copies images into a sibling `images/` directory).
pub fn rewrite_image_links(md: &str, prefix: &str) -> String {
    image_re()
        .replace_all(md, |cap: &regex::Captures<'_>| {
            format!("![{}]({}{})", &cap[1], prefix, image_basename(&cap[2]))
        })
        .into_owned()
}

/// If `md` opens with a top-level `# ` heading, return its text and the body with
/// that line removed; else `(None, md)`. Lets export use the translated (Thai)
/// chapter title and avoid rendering it twice.
pub fn split_leading_title(md: &str) -> (Option<String>, String) {
    let mut lines = md.lines();
    // Skip leading blank lines to find the first content line.
    let mut prefix_blanks = 0usize;
    let first = loop {
        match lines.next() {
            Some(l) if l.trim().is_empty() => prefix_blanks += 1,
            other => break other,
        }
    };
    if let Some(line) = first
        && let Some(rest) = line.trim_start().strip_prefix("# ")
    {
        let title = rest.trim().to_string();
        let body: String = md
            .lines()
            .skip(prefix_blanks + 1)
            .collect::<Vec<_>>()
            .join("\n");
        return (Some(title), body.trim_start_matches('\n').to_string());
    }
    (None, md.to_string())
}

/// Parse the known Markdown subset into block elements.
pub fn parse_blocks(md: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    // Split on blank-line boundaries (cleanse guarantees `\n\n` between blocks).
    for raw in md.split("\n\n") {
        let chunk = raw.trim_matches('\n');
        let trimmed = chunk.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_rule(trimmed) {
            blocks.push(Block::Rule);
            continue;
        }
        if let Some(level) = heading_level(trimmed) {
            let text = trimmed[level as usize..].trim_start();
            blocks.push(Block::Heading {
                level,
                spans: parse_inline(text),
            });
            continue;
        }
        // A block that is exactly one image link → standalone illustration.
        if let Some(img) = sole_image(trimmed) {
            blocks.push(img);
            continue;
        }
        // Otherwise a paragraph: soft-wrap newlines collapse to spaces.
        let joined = chunk.lines().map(str::trim).collect::<Vec<_>>().join(" ");
        blocks.push(Block::Para(parse_inline(&joined)));
    }
    blocks
}

/// A thematic break: a line of 3+ `-` (the cleanse divider) or `*`/`_` variants.
fn is_rule(line: &str) -> bool {
    let s: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && (s.bytes().all(|b| b == b'-') || s.bytes().all(|b| b == b'*'))
}

/// ATX heading level (count of leading `#`, 1..=6) when followed by a space.
fn heading_level(line: &str) -> Option<u8> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some(hashes as u8)
    } else {
        None
    }
}

/// If `chunk` is exactly one image link (nothing else), return it as a Block::Image.
fn sole_image(chunk: &str) -> Option<Block> {
    let cap = image_re().captures(chunk)?;
    if cap.get(0)?.as_str().trim() == chunk.trim() {
        Some(Block::Image {
            alt: cap[1].to_string(),
            file: image_basename(&cap[2]),
        })
    } else {
        None
    }
}

/// Parse inline emphasis + images out of plain text. Greedy but closed: `**` before
/// `*`, images via `![..](..)`. Unmatched markers are emitted as literal text.
pub fn parse_inline(s: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let flush = |buf: &mut String, out: &mut Vec<Inline>| {
        if !buf.is_empty() {
            out.push(Inline::Text(std::mem::take(buf)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        // image: ![alt](url)
        if c == '!'
            && chars.get(i + 1) == Some(&'[')
            && let Some((alt, url, end)) = scan_image(&chars, i)
        {
            flush(&mut buf, &mut out);
            out.push(Inline::Image {
                alt,
                file: image_basename(&url),
            });
            i = end;
            continue;
        }
        // bold: **...**
        if c == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some((inner, end)) = scan_delim(&chars, i, "**")
        {
            flush(&mut buf, &mut out);
            out.push(Inline::Bold(parse_inline(&inner)));
            i = end;
            continue;
        }
        // italic: *...*
        if c == '*'
            && let Some((inner, end)) = scan_delim(&chars, i, "*")
        {
            flush(&mut buf, &mut out);
            out.push(Inline::Italic(parse_inline(&inner)));
            i = end;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out);
    out
}

/// Scan `![alt](url)` starting at `start` (`!`). Returns (alt, url, index-after).
fn scan_image(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    // alt: from start+2 up to ']'
    let mut i = start + 2;
    let alt_start = i;
    while i < chars.len() && chars[i] != ']' {
        i += 1;
    }
    if i >= chars.len() || chars.get(i + 1) != Some(&'(') {
        return None;
    }
    let alt: String = chars[alt_start..i].iter().collect();
    i += 2; // skip "]("
    let url_start = i;
    while i < chars.len() && chars[i] != ')' {
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    let url: String = chars[url_start..i].iter().collect();
    Some((alt, url, i + 1))
}

/// Scan a `delim`-fenced span starting at `start`. Returns (inner, index-after).
fn scan_delim(chars: &[char], start: usize, delim: &str) -> Option<(String, usize)> {
    let dn = delim.chars().count();
    let inner_start = start + dn;
    let mut i = inner_start;
    while i < chars.len() {
        if matches_at(chars, i, delim) {
            let inner: String = chars[inner_start..i].iter().collect();
            if inner.is_empty() {
                return None;
            }
            return Some((inner, i + dn));
        }
        i += 1;
    }
    None
}

fn matches_at(chars: &[char], at: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(k, pc)| chars.get(at + k) == Some(&pc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referenced_images_dedup_and_basename() {
        let md =
            "![a](../../images/x.png)\n\nสวัสดี ![b](../../images/y.jpg) ![c](../../images/x.png)";
        assert_eq!(referenced_images(md), vec!["x.png", "y.jpg"]);
    }

    #[test]
    fn rewrite_links_to_new_prefix() {
        let md = "![ภาพ](../../images/m_001.png)";
        assert_eq!(
            rewrite_image_links(md, "images/"),
            "![ภาพ](images/m_001.png)"
        );
    }

    #[test]
    fn split_title_strips_leading_heading() {
        let (title, body) = split_leading_title("# บทที่ ๑\n\nเนื้อเรื่อง");
        assert_eq!(title.as_deref(), Some("บทที่ ๑"));
        assert_eq!(body, "เนื้อเรื่อง");
        let (none, body2) = split_leading_title("เนื้อเรื่องล้วน");
        assert!(none.is_none());
        assert_eq!(body2, "เนื้อเรื่องล้วน");
    }

    #[test]
    fn blocks_cover_the_subset() {
        let md = "# หัวเรื่อง\n\nย่อหน้า **หนา** และ *เอียง*\n\n---\n\n![ภาพ](../../images/p.png)";
        let blocks = parse_blocks(md);
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(blocks[1], Block::Para(_)));
        assert_eq!(blocks[2], Block::Rule);
        assert_eq!(
            blocks[3],
            Block::Image {
                alt: "ภาพ".to_string(),
                file: "p.png".to_string()
            }
        );
    }

    #[test]
    fn inline_emphasis_and_image() {
        let runs = parse_inline("ก **ข** ค *ง* ![z](../img/z.png)");
        assert_eq!(runs[0], Inline::Text("ก ".to_string()));
        assert_eq!(runs[1], Inline::Bold(vec![Inline::Text("ข".to_string())]));
        assert!(matches!(runs.last(), Some(Inline::Image { file, .. }) if file == "z.png"));
    }

    #[test]
    fn unmatched_emphasis_is_literal() {
        let runs = parse_inline("ราคา 5 * 3 ดาว");
        assert_eq!(runs, vec![Inline::Text("ราคา 5 * 3 ดาว".to_string())]);
    }
}

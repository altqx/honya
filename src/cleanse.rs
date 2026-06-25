//! XHTML/HTML -> clean Markdown for honya's raw chapter files. Self-contained.
//!
//! Load-bearing cleanse rules: `<br>`->`---` thematic break (an empty `<p><br></p>`
//! scene-break spacer, or a stacked run of them, becomes a single divider — never a
//! literal token); b/strong/span.b->`**..**`;
//! i/em/span.em->`*..*`; 「」->“”, 『』->‘’; ruby->`Base (Furigana)`;
//! `<img>`->`![ภาพประกอบ](../../images/FILE.png)`; other tags stripped; block elements
//! separated by a blank line; 3+ newlines collapsed to 2.
//!
//! Gaiji (外字): an inline `<img>`/SVG `<image>` that is a single external glyph
//! (class/id/filename marked `gaiji`, or a lone non-ASCII `alt`) is emitted as
//! prose inline — NOT as an illustration link — so the sentence stays whole.
//! Its `alt`/`title` is usually the glyph itself ("!!!", "あ゛", "40°") → emitted
//! verbatim; some publishers instead store the punctuation's *name* ("かんたんふ"
//! = 感嘆符) → mapped to the real mark ("!!!"); a label-less gaiji is dropped.
//! Source text is also scrubbed of Private-Use-Area codepoints
//! (`sanitize_external_chars`) — they collide with the IMG sentinels below and
//! don't render anyway.
//!
//! Ordering matters: emphasis/ruby/img happen during the DOM walk; quotes + whitespace
//! run in a textual post-pass guarded by IMG_OPEN/IMG_CLOSE sentinels so they never
//! touch image alt/URL. Entities are NOT re-decoded — html5ever already did.

use std::collections::HashMap;
use std::sync::OnceLock;

use ego_tree::NodeRef;
use regex::Regex;
use scraper::Html;
use scraper::node::Node;

/// The fixed chapter-relative prefix for relocated images (hard spec).
const IMAGE_PREFIX: &str = "../../images/";
/// Default image alt text (Thai: "illustration").
const IMAGE_ALT: &str = "ภาพประกอบ";

// Sentinels keep the textual post-pass from changing image alt text or URLs.
// Both live in the Private Use Area where gaiji codepoints also live, so source
// text is scrubbed of PUA (`sanitize_external_chars`) before it reaches `out`.
const IMG_OPEN: char = '\u{E000}';
const IMG_CLOSE: char = '\u{E001}';

/// Block-level HTML elements: each forces a paragraph break in the output.
fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "aside"
            | "nav"
            | "blockquote"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "tr"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "figure"
            | "figcaption"
            | "hr"
            | "pre"
    )
}

/// Convert an XHTML/HTML fragment into clean Markdown per the honya cleanse rules.
pub fn xhtml_to_markdown(html: &str, image_map: &HashMap<String, String>) -> String {
    let doc = Html::parse_fragment(html);

    let mut out = String::with_capacity(html.len());
    walk(doc.tree.root(), image_map, &mut out);

    post_process(&out)
}

pub fn leading_image_title(html: &str) -> Option<String> {
    let doc = Html::parse_fragment(html);
    match first_visible_content(doc.tree.root()) {
        VisibleContent::Image(label) => label,
        VisibleContent::Text | VisibleContent::None => None,
    }
}

/// Apply honya's quote, external-glyph, and whitespace cleanup to Markdown that
/// came from another converter such as MarkItDown.
pub fn clean_markdown(markdown: &str) -> String {
    post_process(&sanitize_external_chars(markdown))
}

enum VisibleContent {
    None,
    Text,
    Image(Option<String>),
}

fn first_visible_content(node: NodeRef<'_, Node>) -> VisibleContent {
    match node.value() {
        Node::Text(text) => {
            if text.trim().is_empty() {
                VisibleContent::None
            } else {
                VisibleContent::Text
            }
        }
        Node::Element(el) => match el.name() {
            "head" | "script" | "style" | "title" | "br" => VisibleContent::None,
            "img" | "image" => VisibleContent::Image(image_title(el)),
            _ => first_visible_child(node),
        },
        _ => first_visible_child(node),
    }
}

fn first_visible_child(node: NodeRef<'_, Node>) -> VisibleContent {
    for child in node.children() {
        match first_visible_content(child) {
            VisibleContent::None => {}
            found => return found,
        }
    }
    VisibleContent::None
}

fn image_title(el: &scraper::node::Element) -> Option<String> {
    let label = el
        .attr("alt")
        .or_else(|| el.attr("title"))
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let title = sanitize_external_chars(label)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!title.is_empty()).then_some(title)
}

/// Recursively render a node and its subtree into `out`.
fn walk(node: NodeRef<'_, Node>, image_map: &HashMap<String, String>, out: &mut String) {
    match node.value() {
        Node::Text(text) => {
            push_sanitized(out, text);
        }
        Node::Element(el) => {
            let tag = el.name();
            match tag {
                // Script/style text must never leak into the markdown.
                "head" | "script" | "style" | "title" => {}
                "br" => {
                    // Scene-break divider. Light novels use empty <p><br></p> paragraphs
                    // (often stacked) as vertical scene gaps; render each as a Markdown
                    // thematic break `---`, emitted block-level so it always parses as a
                    // rule. A run of them collapses to a single rule in post_process. This
                    // leaves no inline token for the LLM translator to over-replicate.
                    ensure_block_break(out);
                    out.push_str("---");
                    ensure_block_break(out);
                }
                "img" => {
                    let src = el.attr("src").unwrap_or("");
                    emit_image_or_gaiji(el, src, image_map, out);
                }
                // SVG href may be namespaced, so match "href" or ":href".
                "image" => {
                    let src = el
                        .attrs()
                        .find(|(k, _)| *k == "href" || k.ends_with(":href"))
                        .map(|(_, v)| v)
                        .unwrap_or("");
                    emit_image_or_gaiji(el, src, image_map, out);
                }
                "b" | "strong" => {
                    out.push_str("**");
                    walk_children(node, image_map, out);
                    out.push_str("**");
                }
                "i" | "em" => {
                    out.push('*');
                    walk_children(node, image_map, out);
                    out.push('*');
                }
                "span" => match span_emphasis(el) {
                    SpanEmphasis::Bold => {
                        out.push_str("**");
                        walk_children(node, image_map, out);
                        out.push_str("**");
                    }
                    SpanEmphasis::Italic => {
                        out.push('*');
                        walk_children(node, image_map, out);
                        out.push('*');
                    }
                    SpanEmphasis::None => walk_children(node, image_map, out),
                },
                "ruby" => {
                    render_ruby(node, image_map, out);
                }
                // Loose rt/rp outside a <ruby>: ignore quietly.
                "rt" | "rp" => {}
                _ => {
                    // Transparent; block elements still separate paragraphs.
                    if is_block(tag) {
                        ensure_block_break(out);
                        walk_children(node, image_map, out);
                        ensure_block_break(out);
                    } else {
                        walk_children(node, image_map, out);
                    }
                }
            }
        }
        _ => walk_children(node, image_map, out),
    }
}

fn walk_children(node: NodeRef<'_, Node>, image_map: &HashMap<String, String>, out: &mut String) {
    for child in node.children() {
        walk(child, image_map, out);
    }
}

/// Render `<ruby>`: base text followed by " (furigana)". `<rb>` is transparent,
/// `<rp>` (parenthesis fallbacks) are dropped, `<rt>` is the annotation.
fn render_ruby(node: NodeRef<'_, Node>, image_map: &HashMap<String, String>, out: &mut String) {
    let mut base = String::new();
    let mut furigana = String::new();

    collect_ruby(node, image_map, &mut base, &mut furigana);

    let base = base.trim();
    let furigana = furigana.trim();
    out.push_str(base);
    if !furigana.is_empty() {
        out.push_str(" (");
        out.push_str(furigana);
        out.push(')');
    }
}

/// Split a ruby subtree's content into base text and furigana (rt) text.
fn collect_ruby(
    node: NodeRef<'_, Node>,
    image_map: &HashMap<String, String>,
    base: &mut String,
    furigana: &mut String,
) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => push_sanitized(base, t),
            Node::Element(el) => match el.name() {
                "rt" => {
                    let mut sub = String::new();
                    walk_children(child, image_map, &mut sub);
                    furigana.push_str(&sub);
                }
                // rp: parenthesis fallback for non-ruby readers, dropped entirely.
                "rp" => {}
                // rb: transparent base container.
                "rb" => {
                    collect_ruby(child, image_map, base, furigana);
                }
                _ => {
                    walk(child, image_map, base);
                }
            },
            _ => {}
        }
    }
}

enum SpanEmphasis {
    Bold,
    Italic,
    None,
}

/// Classify a `<span>` by its `class` tokens: "b" => bold, "em" => italic.
fn span_emphasis(el: &scraper::node::Element) -> SpanEmphasis {
    if let Some(class) = el.attr("class") {
        for tok in class.split_whitespace() {
            match tok {
                "b" => return SpanEmphasis::Bold,
                "em" => return SpanEmphasis::Italic,
                _ => {}
            }
        }
    }
    SpanEmphasis::None
}

/// Ensure the output ends with exactly a paragraph break (two newlines), without
/// piling up blank lines (the final collapse pass will tidy anything remaining).
fn ensure_block_break(out: &mut String) {
    if out.is_empty() {
        return;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// Emit an `<img>` / SVG `<image>`: an inline gaiji becomes its substitute text
/// (see `gaiji_replacement`) as prose so the sentence stays intact; anything
/// else is a relocated illustration link wrapped in IMG sentinels.
fn emit_image_or_gaiji(
    el: &scraper::node::Element,
    src: &str,
    image_map: &HashMap<String, String>,
    out: &mut String,
) {
    if let Some(text) = gaiji_replacement(el, src) {
        out.push_str(&text);
        return;
    }
    let file = resolve_image(src, image_map);
    out.push(IMG_OPEN);
    out.push_str("![");
    out.push_str(IMAGE_ALT);
    out.push_str("](");
    out.push_str(IMAGE_PREFIX);
    out.push_str(&file);
    out.push(')');
    out.push(IMG_CLOSE);
}

/// The inline substitute for a gaiji glyph (see the module doc for the policy):
/// `None` = ordinary illustration (stays an image link), `Some("")` = drop it.
fn gaiji_replacement(el: &scraper::node::Element, src: &str) -> Option<String> {
    let alt = el.attr("alt").map(str::trim).filter(|s| !s.is_empty());
    if !is_gaiji_img(el, src, alt) {
        return None;
    }
    let label = alt.or_else(|| el.attr("title").map(str::trim).filter(|s| !s.is_empty()));
    let Some(label) = label else {
        return Some(String::new());
    };
    // A punctuation NAME ("かんたんふ") must become the mark, never the literal
    // word — else the translator renders "exclamation mark" as prose.
    if let Some(glyph) = punctuation_name_to_glyph(label) {
        return Some(glyph.to_string());
    }
    // Otherwise the label IS the glyph; drop any raw PUA it carries.
    Some(sanitize_external_chars(label))
}

/// Map a punctuation-mark *name* that some publishers put in a gaiji `alt`
/// (e.g. "かんたんふ" = 感嘆符) to the actual mark. Safe because it is only
/// reached for detected gaiji, so the label is a mark name, not prose. The
/// exclamation glyph these books ship is a triple "!!!". Extend as needed.
fn punctuation_name_to_glyph(label: &str) -> Option<&'static str> {
    match label {
        "かんたんふ" | "かんたんぷ" | "感嘆符" | "感歎符" => Some("!!!"),
        "ぎもんふ" | "ぎもんぷ" | "疑問符" => Some("?"),
        "かんたんふぎもんふ" | "感嘆符疑問符" => Some("!?"),
        _ => None,
    }
}

/// A gaiji image carries an explicit `gaiji` marker on its class/id/filename, or
/// an `alt` that is a single non-ASCII glyph (the character itself, not a caption).
fn is_gaiji_img(el: &scraper::node::Element, src: &str, alt: Option<&str>) -> bool {
    fn has_gaiji_token(s: &str) -> bool {
        s.to_ascii_lowercase().contains("gaiji")
    }
    if el
        .attr("class")
        .is_some_and(|c| c.split_whitespace().any(has_gaiji_token))
    {
        return true;
    }
    if el.attr("id").is_some_and(has_gaiji_token) {
        return true;
    }
    if has_gaiji_token(basename(src)) {
        return true;
    }
    if let Some(a) = alt {
        let mut chars = a.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return !c.is_ascii();
        }
    }
    false
}

/// Drop every Private-Use-Area codepoint from source text. The PUA holds
/// non-standard gaiji *and* our IMG sentinels (U+E000/E001), so a raw PUA char
/// in prose would both fail to render and corrupt the sentinel-based post-pass.
fn sanitize_external_chars(s: &str) -> String {
    if !s.chars().any(is_private_use) {
        return s.to_string();
    }
    s.chars().filter(|&c| !is_private_use(c)).collect()
}

fn is_private_use(c: char) -> bool {
    matches!(
        c as u32,
        0xE000..=0xF8FF            // BMP PUA (includes the IMG_OPEN/IMG_CLOSE sentinels)
        | 0xF_0000..=0xF_FFFD     // Plane 15 (PUA-A)
        | 0x10_0000..=0x10_FFFD   // Plane 16 (PUA-B)
    )
}

/// Append `text` to `out`, scrubbing any external-character (PUA) codepoints.
fn push_sanitized(out: &mut String, text: &str) {
    out.push_str(&sanitize_external_chars(text));
}

/// Resolve an `<img src>` to a relocated basename. Lookup order:
///   1. exact `src` key in image_map,
///   2. basename of `src` as a key in image_map,
///   3. raw basename of `src` (when no map entry exists).
fn resolve_image(src: &str, image_map: &HashMap<String, String>) -> String {
    if let Some(mapped) = image_map.get(src) {
        return mapped.clone();
    }
    let base = basename(src);
    if let Some(mapped) = image_map.get(base) {
        return mapped.clone();
    }
    base.to_string()
}

/// Last path segment, splitting on both '/' and '\\' and dropping any fragment/query.
fn basename(src: &str) -> &str {
    let no_frag = src.split('#').next().unwrap_or(src);
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    no_query.rsplit(['/', '\\']).next().unwrap_or(no_query)
}

fn ws_collapse_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Three or more newlines (optionally with interleaved spaces/tabs) -> two.
    RE.get_or_init(|| Regex::new(r"\n{3,}").unwrap())
}

fn blank_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // A line consisting only of spaces/tabs (between newlines) -> empty line.
    RE.get_or_init(|| Regex::new(r"(?m)^[ \t]+$").unwrap())
}

fn trailing_ws_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Strip trailing whitespace, including stray CR from non-conforming input.
    RE.get_or_init(|| Regex::new(r"(?m)[ \t\r]+$").unwrap())
}

fn run_spaces_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Runs of 2+ ASCII spaces/tabs (mid-line) -> single space.
    RE.get_or_init(|| Regex::new(r"[ \t]{2,}").unwrap())
}

fn rule_run_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Two or more `---` thematic breaks separated only by a blank line -> one.
    RE.get_or_init(|| Regex::new(r"---(?:\n\n---)+").unwrap())
}

/// Apply quote conversion + whitespace normalization. Image markdown lives
/// inside IMG_OPEN..IMG_CLOSE sentinels: we split on those, normalize ONLY the
/// non-image segments, then reassemble (sentinels removed).
fn post_process(input: &str) -> String {
    let mut result = String::with_capacity(input.len());

    let mut rest = input;
    while let Some(open_idx) = rest.find(IMG_OPEN) {
        let (before, after_open) = rest.split_at(open_idx);
        result.push_str(&normalize_text(before));

        let after_open = &after_open[IMG_OPEN.len_utf8()..];
        match after_open.find(IMG_CLOSE) {
            Some(close_idx) => {
                let (img_md, after_close) = after_open.split_at(close_idx);
                result.push_str(img_md); // verbatim
                rest = &after_close[IMG_CLOSE.len_utf8()..];
            }
            None => {
                // Unterminated sentinel (shouldn't happen): emit rest as text.
                result.push_str(&normalize_text(after_open));
                rest = "";
            }
        }
    }
    result.push_str(&normalize_text(rest));

    // Strip blank-only lines BEFORE collapsing newline runs, then cap block breaks at 2.
    let mut s = result;
    s = blank_line_re().replace_all(&s, "").into_owned();
    s = trailing_ws_re().replace_all(&s, "").into_owned();
    s = ws_collapse_re().replace_all(&s, "\n\n").into_owned();
    // Collapse a run of consecutive thematic breaks (stacked <br> spacers) into one.
    s = rule_run_re().replace_all(&s, "---").into_owned();

    s.trim_matches('\n').to_string()
}

/// Normalize ONE non-image text segment: convert CJK corner quotes to curly
/// quotes and squeeze runs of intra-line ASCII spaces. (Newline collapsing is
/// done once globally in `post_process`, after sentinels are gone.)
fn normalize_text(seg: &str) -> String {
    if seg.is_empty() {
        return String::new();
    }
    let mut s = String::with_capacity(seg.len());
    for ch in seg.chars() {
        match ch {
            '「' => s.push('\u{201C}'), // “
            '」' => s.push('\u{201D}'), // ”
            '『' => s.push('\u{2018}'), // ‘
            '』' => s.push('\u{2019}'), // ’
            other => s.push(other),
        }
    }
    run_spaces_re().replace_all(&s, " ").into_owned()
}

/// A chapter is "image-only" if, after removing image markdown links, no
/// meaningful residual text remains (using the default threshold of 0 chars).
pub fn is_image_only(markdown: &str) -> bool {
    is_image_only_with_threshold(markdown, 0)
}

fn img_md_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Matches a markdown image: ![alt](url)
    RE.get_or_init(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap())
}

/// `is_image_only` with a configurable residual-character budget. The chapter
/// must contain at least one image, and after stripping all image links the
/// remaining non-whitespace character count must be <= `max_residual_chars`.
pub fn is_image_only_with_threshold(markdown: &str, max_residual_chars: usize) -> bool {
    let has_image = img_md_re().is_match(markdown);
    if !has_image {
        return false;
    }
    let residual = img_md_re().replace_all(markdown, "");
    let residual_chars = residual.chars().filter(|c| !c.is_whitespace()).count();
    residual_chars <= max_residual_chars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(html: &str) -> String {
        xhtml_to_markdown(html, &HashMap::new())
    }

    #[test]
    fn br_becomes_thematic_break() {
        assert_eq!(md("a<br/>b"), "a\n\n---\n\nb");
        assert_eq!(md("a<br>b"), "a\n\n---\n\nb");
    }

    #[test]
    fn br_paragraph_spacer_becomes_one_rule() {
        // `<p><br/></p>` scene-break spacers become a single `---` divider, never a
        // literal `&nbsp;` token (which the LLM translator otherwise replicates all
        // over the output).
        let out = md("<p>a</p><p><br/></p><p>b</p>");
        assert_eq!(out, "a\n\n---\n\nb");
        assert!(!out.contains("nbsp"));
        // Several stacked spacers collapse to ONE divider, not a stack of rules.
        assert_eq!(
            md("<p>a</p><p><br/></p><p><br/></p><p>b</p>"),
            "a\n\n---\n\nb"
        );
        assert_eq!(
            md("<p>a</p><p><br/></p><p><br/></p><p><br/></p><p>b</p>"),
            "a\n\n---\n\nb"
        );
    }

    #[test]
    fn bold_variants() {
        assert_eq!(md("<b>x</b>"), "**x**");
        assert_eq!(md("<strong>x</strong>"), "**x**");
        assert_eq!(md(r#"<span class="b">x</span>"#), "**x**");
    }

    #[test]
    fn italic_variants() {
        assert_eq!(md("<i>x</i>"), "*x*");
        assert_eq!(md("<em>x</em>"), "*x*");
        assert_eq!(md(r#"<span class="em">x</span>"#), "*x*");
    }

    #[test]
    fn plain_span_is_transparent() {
        assert_eq!(md(r#"<span class="other">x</span>"#), "x");
        assert_eq!(md("<span>x</span>"), "x");
    }

    #[test]
    fn corner_quotes_to_curly() {
        assert_eq!(md("「やあ」"), "\u{201C}やあ\u{201D}");
    }

    #[test]
    fn double_corner_quotes_to_single_curly() {
        assert_eq!(md("『心の声』"), "\u{2018}心の声\u{2019}");
    }

    #[test]
    fn ruby_base_furigana() {
        assert_eq!(md("<ruby>漢<rt>かん</rt></ruby>"), "漢 (かん)");
    }

    #[test]
    fn ruby_with_rp_and_rb() {
        // rb transparent, rp stripped.
        let html = "<ruby><rb>漢字</rb><rp>(</rp><rt>かんじ</rt><rp>)</rp></ruby>";
        assert_eq!(md(html), "漢字 (かんじ)");
    }

    #[test]
    fn img_uses_fixed_prefix_and_alt() {
        assert_eq!(
            md(r#"<img src="i001.png"/>"#),
            "![ภาพประกอบ](../../images/i001.png)"
        );
    }

    #[test]
    fn img_resolves_via_map_src_key() {
        let mut map = HashMap::new();
        map.insert("../Images/a.png".to_string(), "a_1.png".to_string());
        let out = xhtml_to_markdown(r#"<img src="../Images/a.png"/>"#, &map);
        assert_eq!(out, "![ภาพประกอบ](../../images/a_1.png)");
    }

    #[test]
    fn svg_image_is_rendered() {
        let map = HashMap::new();
        let out = xhtml_to_markdown(
            r#"<svg xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="cover.png"/></svg>"#,
            &map,
        );
        assert_eq!(out, "![ภาพประกอบ](../../images/cover.png)");
        assert!(is_image_only(&out));
    }

    #[test]
    fn img_resolves_via_basename_key() {
        let mut map = HashMap::new();
        map.insert("cover.png".to_string(), "cover_2.png".to_string());
        let out = xhtml_to_markdown(r#"<img src="OEBPS/Images/cover.png"/>"#, &map);
        assert_eq!(out, "![ภาพประกอบ](../../images/cover_2.png)");
    }

    #[test]
    fn img_alt_and_url_survive_quote_pass() {
        // A corner quote adjacent to an image must NOT bleed into the image md.
        let out = md(r#"「<img src="x.png"/>」"#);
        assert_eq!(out, "\u{201C}![ภาพประกอบ](../../images/x.png)\u{201D}");
    }

    #[test]
    fn other_tags_stripped_text_kept() {
        assert_eq!(md(r#"<a href="x">link</a>"#), "link");
    }

    #[test]
    fn block_elements_separate_paragraphs() {
        assert_eq!(md("<p>one</p><p>two</p>"), "one\n\ntwo");
    }

    #[test]
    fn collapses_excess_blank_lines() {
        // div nesting could produce extra breaks; cap at two newlines.
        let out = md("<div>a</div><div></div><div></div><div>b</div>");
        assert_eq!(out, "a\n\nb");
    }

    #[test]
    fn does_not_redecode_entities() {
        // html5ever already decodes &amp; -> &; we must not double-process.
        assert_eq!(md("Tom &amp; Jerry"), "Tom & Jerry");
    }

    #[test]
    fn is_image_only_true_for_pure_image() {
        let m = "![ภาพประกอบ](../../images/i.png)";
        assert!(is_image_only(m));
    }

    #[test]
    fn is_image_only_false_with_prose() {
        let m = "![ภาพประกอบ](../../images/i.png)\n\nここに本文がある。";
        assert!(!is_image_only(m));
    }

    #[test]
    fn is_image_only_false_without_image() {
        assert!(!is_image_only("ただのテキスト"));
    }

    #[test]
    fn is_image_only_threshold_allows_small_residual() {
        let m = "![ภาพประกอบ](../../images/i.png)\n*"; // 1 stray char
        assert!(!is_image_only(m)); // strict: 0 budget
        assert!(is_image_only_with_threshold(m, 2)); // tolerant
    }

    #[test]
    fn nested_emphasis() {
        assert_eq!(md("<b>a<i>b</i>c</b>"), "**a*b*c**");
    }

    #[test]
    fn intra_line_space_runs_squeezed() {
        assert_eq!(md("a    b"), "a b");
    }

    #[test]
    fn mixed_paragraph_with_ruby_and_quote() {
        let html = "<p>「<ruby>君<rt>きみ</rt></ruby>」</p>";
        assert_eq!(md(html), "\u{201C}君 (きみ)\u{201D}");
    }

    #[test]
    fn gaiji_punctuation_name_alt_maps_to_mark() {
        // Real publisher convention: alt is the punctuation NAME (感嘆符 =
        // "かんたんふ"), not the glyph. It must become "!!!", never the word.
        assert_eq!(
            md(r#"飲んでないよ<img class="gaiji" alt="かんたんふ"/>"#),
            "飲んでないよ!!!"
        );
        assert_eq!(md(r#"なに<img class="gaiji" alt="感嘆符"/>"#), "なに!!!");
    }

    #[test]
    fn gaiji_literal_glyph_alt_is_emitted_verbatim() {
        // Sibling volumes store the glyph itself in alt — emit it as-is.
        assert_eq!(md(r#"だめ<img class="gaiji" alt="!!!"/>"#), "だめ!!!");
        assert_eq!(md(r#"<img class="gaiji" alt="あ゛"/>"#), "あ゛");
        assert_eq!(md(r#"<img class="gaiji" alt="40°"/>"#), "40°");
    }

    #[test]
    fn gaiji_without_label_is_dropped() {
        // No alt/title and no name to map → emit nothing (no tofu marker), so the
        // surrounding text simply joins up.
        assert_eq!(
            md(r#"名は<img class="gaiji" src="g.png"/>った"#),
            "名はった"
        );
        assert_eq!(md(r#"名は<img id="gaiji_3" src="x.png"/>った"#), "名はった");
    }

    #[test]
    fn gaiji_detected_by_src_basename() {
        assert_eq!(
            md(r#"名は<img src="../image/gaiji-001.png" alt="彁"/>った"#),
            "名は彁った"
        );
    }

    #[test]
    fn single_glyph_alt_is_treated_as_gaiji() {
        // No gaiji marker, but a lone non-ASCII alt IS the character.
        assert_eq!(md(r#"名は<img src="x.png" alt="彁"/>った"#), "名は彁った");
    }

    #[test]
    fn gaiji_with_pua_alt_is_dropped() {
        // alt holds a raw PUA codepoint — unrenderable and hint-less, so drop it.
        assert_eq!(
            md("名は<img class=\"gaiji\" alt=\"\u{E5C0}\"/>った"),
            "名はった"
        );
    }

    #[test]
    fn ordinary_illustration_still_renders_as_image_link() {
        // Regression: a normal alt (caption, multi-char) is NOT mistaken for gaiji.
        assert_eq!(
            md(r#"<img src="i001.png" alt="挿絵1"/>"#),
            "![ภาพประกอบ](../../images/i001.png)"
        );
    }

    #[test]
    fn gaiji_inside_quotes_keeps_sentence_intact() {
        assert_eq!(
            md(r#"「飲めるか<img class="gaiji" alt="かんたんふ"/>」"#),
            "\u{201C}飲めるか!!!\u{201D}"
        );
    }

    #[test]
    fn pua_in_source_text_is_dropped_not_corrupting_sentinels() {
        // A raw PUA char in prose — including the exact IMG sentinel codepoints
        // U+E000/U+E001 — is removed, so it can't derail the sentinel post-pass.
        assert_eq!(md("名\u{E000}は\u{E001}る"), "名はる");
        assert_eq!(md("名\u{F8FF}る"), "名る");
    }

    #[test]
    fn pua_in_text_does_not_eat_adjacent_image() {
        // A stray PUA char immediately before a real image must not swallow the
        // following image markdown (the bug a literal IMG_OPEN would have caused).
        // (alt is a multi-char caption, so the image stays an illustration link.)
        assert_eq!(
            md("あ\u{E000}<img src=\"i.png\" alt=\"挿絵の説明\"/>"),
            "あ![ภาพประกอบ](../../images/i.png)"
        );
    }
}

//! XHTML/HTML -> clean Markdown for honya's raw chapter files. Self-contained.
//!
//! Load-bearing cleanse rules: `<br>`->`---` thematic break (an empty `<p><br></p>`
//! scene-break spacer, or a stacked run of them, becomes a single divider — never a
//! literal token); b/strong/span.b->`**..**`;
//! i/em/span.em->`*..*`; 「」->“”, 『』->‘’; ruby->`Base (Furigana)`;
//! `<img>`->`![ภาพประกอบ](../../images/FILE.png)`; other tags stripped; block elements
//! separated by a blank line; 3+ newlines collapsed to 2.
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

/// Recursively render a node and its subtree into `out`.
fn walk(node: NodeRef<'_, Node>, image_map: &HashMap<String, String>, out: &mut String) {
    match node.value() {
        Node::Text(text) => {
            out.push_str(text);
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
                // SVG href may be namespaced, so match "href" or ":href".
                "image" => {
                    let src = el
                        .attrs()
                        .find(|(k, _)| *k == "href" || k.ends_with(":href"))
                        .map(|(_, v)| v)
                        .unwrap_or("");
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
            Node::Text(t) => base.push_str(t),
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
}

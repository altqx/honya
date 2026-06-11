//! File import helpers for MarkItDown-style document conversion.
//!
//! EPUB keeps honya's spine/media pipeline. Other supported formats use the same
//! pure-Rust conversion approach as MarkItDown's Rust implementation, limited to
//! the file families honya exposes in the shelf picker.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use htmd::HtmlToMarkdown;
use scraper::{Html, Selector};
use zip::ZipArchive;

use crate::cleanse;

/// Import formats intentionally exposed in honya's shelf picker.
pub const SUPPORTED_IMPORT_EXTS: &[&str] = &[
    "epub", "pdf", "docx", "html", "htm", "xhtml", "txt", "text", "md", "markdown", "json",
    "jsonl", "csv", "xml",
];

#[derive(Debug, Clone)]
pub struct MarkdownChapter {
    pub body: String,
    pub image_only: bool,
}

#[derive(Debug, Clone)]
pub struct ConvertedMarkdown {
    pub chapters: Vec<MarkdownChapter>,
    pub degraded: bool,
}

struct RawConversion {
    markdown: String,
    title: Option<String>,
    degraded: bool,
}

pub fn is_supported_import_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_supported_import_ext)
        .unwrap_or(false)
}

pub fn is_supported_import_ext(ext: &str) -> bool {
    let ext = ext.trim().trim_start_matches('.');
    SUPPORTED_IMPORT_EXTS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(ext))
}

pub fn is_epub_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("epub"))
        .unwrap_or(false)
}

pub fn supported_import_summary() -> &'static str {
    "EPUB, PDF, Word (.docx), HTML, Markdown/Text, CSV, JSON, XML"
}

/// Convert a non-EPUB source through honya's MarkItDown-style converter, then
/// apply honya's Markdown cleanup and split obvious `#`-headed sections.
pub fn convert_markitdown_path(
    path: &Path,
    fallback_title: &str,
    on_progress: impl Fn(&str),
) -> anyhow::Result<ConvertedMarkdown> {
    if is_epub_path(path) {
        bail!("EPUB uses honya's spine-aware importer");
    }
    if !is_supported_import_path(path) {
        bail!("unsupported import format: {}", display_ext(path));
    }

    on_progress("detecting format");
    let raw = convert_raw(path, &on_progress)
        .with_context(|| format!("could not convert {} to Markdown", path.display()))?;
    let title = raw
        .title
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(prettify_stem));
    let title_for_chapter = title.as_deref().unwrap_or(fallback_title);
    let markdown = cleanse::clean_markdown(&raw.markdown);
    if markdown.trim().is_empty() {
        bail!("converter produced no Markdown content");
    }

    Ok(ConvertedMarkdown {
        chapters: split_markdown_chapters(&markdown, title_for_chapter),
        degraded: raw.degraded,
    })
}

fn convert_raw(path: &Path, on_progress: &impl Fn(&str)) -> anyhow::Result<RawConversion> {
    let ext = normalized_ext(path);
    match ext.as_deref() {
        Some("pdf") => convert_pdf(path, on_progress),
        Some("docx") => convert_docx(path, on_progress),
        Some("html" | "htm" | "xhtml") => convert_html(path, on_progress),
        Some("csv") => convert_csv(path, on_progress),
        Some("xml") => convert_xml(path, on_progress),
        Some("txt" | "text" | "md" | "markdown" | "json" | "jsonl") => {
            convert_plain_text(path, on_progress)
        }
        _ => bail!("unsupported import format: {}", display_ext(path)),
    }
}

fn convert_pdf(path: &Path, on_progress: &impl Fn(&str)) -> anyhow::Result<RawConversion> {
    on_progress("extracting PDF text");
    let data = std::fs::read(path)?;
    let extracted = catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(&data)
    }));
    let text = match extracted {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => bail!("failed to extract text from PDF: {e}"),
        Err(_) => bail!("pdf-extract panicked while parsing the document"),
    };

    if text.trim().is_empty() {
        return Ok(RawConversion {
            markdown:
                "<!-- This PDF appears to be scanned or image-only; no text layer was found. -->"
                    .to_string(),
            title: None,
            degraded: true,
        });
    }

    Ok(RawConversion {
        markdown: normalize_newlines(&text),
        title: None,
        degraded: false,
    })
}

fn convert_docx(path: &Path, on_progress: &impl Fn(&str)) -> anyhow::Result<RawConversion> {
    on_progress("reading Word document");
    let file = std::fs::File::open(path)?;
    let mut zip = ZipArchive::new(file)?;
    let document = read_zip_string(&mut zip, "word/document.xml")
        .context("missing word/document.xml in DOCX")?;
    let styles = read_zip_string(&mut zip, "word/styles.xml").unwrap_or_default();
    let style_names = parse_docx_style_names(&styles);
    let markdown = render_docx_document(&document, &style_names)?;
    let title = markdown
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim_start_matches('#').trim().to_string());

    Ok(RawConversion {
        markdown,
        title,
        degraded: document.contains("<w:commentReference") || document.contains("<m:oMath"),
    })
}

fn convert_html(path: &Path, on_progress: &impl Fn(&str)) -> anyhow::Result<RawConversion> {
    on_progress("converting HTML");
    let bytes = std::fs::read(path)?;
    let html = decode_bytes(&bytes, None);
    let title = extract_html_title(&html);
    let markdown = if needs_honya_html_cleanse(&html) {
        cleanse::xhtml_to_markdown(&html, &HashMap::new())
    } else {
        HtmlToMarkdown::builder()
            .skip_tags(vec!["script", "style"])
            .build()
            .convert(&html)
            .unwrap_or_default()
    };

    Ok(RawConversion {
        markdown,
        title,
        degraded: false,
    })
}

fn convert_csv(path: &Path, on_progress: &impl Fn(&str)) -> anyhow::Result<RawConversion> {
    on_progress("converting CSV");
    let bytes = std::fs::read(path)?;
    let text = decode_bytes(&bytes, None);
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows: Vec<Vec<String>> = Vec::new();
    rows.push(reader.headers()?.iter().map(str::to_string).collect());
    for record in reader.records() {
        rows.push(record?.iter().map(str::to_string).collect());
    }

    Ok(RawConversion {
        markdown: rows_to_markdown_table(&rows),
        title: None,
        degraded: false,
    })
}

fn convert_xml(path: &Path, on_progress: &impl Fn(&str)) -> anyhow::Result<RawConversion> {
    on_progress("converting XML");
    let bytes = std::fs::read(path)?;
    let text = decode_bytes(&bytes, None);
    let doc = roxmltree::Document::parse(&text)?;
    let root = doc.root_element();
    let mut markdown = String::new();
    render_xml_node(root, 0, &mut markdown);
    Ok(RawConversion {
        markdown,
        title: Some(root.tag_name().name().to_string()),
        degraded: false,
    })
}

fn convert_plain_text(path: &Path, on_progress: &impl Fn(&str)) -> anyhow::Result<RawConversion> {
    on_progress("reading text");
    let bytes = std::fs::read(path)?;
    Ok(RawConversion {
        markdown: decode_bytes(&bytes, None),
        title: None,
        degraded: false,
    })
}

pub fn split_markdown_chapters(markdown: &str, fallback_title: &str) -> Vec<MarkdownChapter> {
    let mut md = markdown.trim().to_string();
    if !first_content_line_is_h1(&md) {
        let title = fallback_title.trim();
        if !title.is_empty() {
            md = format!("# {title}\n\n{md}");
        }
    }

    let lines: Vec<&str> = md.lines().collect();
    let h1_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| is_h1(line).then_some(idx))
        .collect();

    if h1_indices.len() <= 1 {
        return vec![chapter(md)];
    }

    let mut out = Vec::with_capacity(h1_indices.len());
    let prefix = lines[..h1_indices[0]].join("\n").trim().to_string();
    for (pos, start) in h1_indices.iter().copied().enumerate() {
        let end = h1_indices.get(pos + 1).copied().unwrap_or(lines.len());
        let mut body = lines[start..end].join("\n").trim().to_string();
        if pos == 0 && !prefix.is_empty() {
            body = format!("{prefix}\n\n{body}");
        }
        if !body.trim().is_empty() {
            out.push(chapter(body));
        }
    }

    if out.is_empty() {
        vec![chapter(md)]
    } else {
        out
    }
}

fn chapter(body: String) -> MarkdownChapter {
    let body = cleanse::clean_markdown(&body);
    let image_only = cleanse::is_image_only(&body);
    MarkdownChapter { body, image_only }
}

fn read_zip_string<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Option<String> {
    let mut file = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(decode_bytes(&buf, Some("utf-8")))
}

fn parse_docx_style_names(xml: &str) -> HashMap<String, String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for style in doc
        .descendants()
        .filter(|n| n.is_element() && local(*n) == "style")
    {
        let Some(id) = attr_local(style, "styleId") else {
            continue;
        };
        let Some(name_node) = style
            .children()
            .find(|n| n.is_element() && local(*n) == "name")
        else {
            continue;
        };
        if let Some(name) = attr_local(name_node, "val") {
            out.insert(id.to_string(), name.to_ascii_lowercase());
        }
    }
    out
}

fn render_docx_document(
    xml: &str,
    style_names: &HashMap<String, String>,
) -> anyhow::Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    let body = doc
        .descendants()
        .find(|n| n.is_element() && local(*n) == "body")
        .context("DOCX body not found")?;
    let mut blocks = Vec::new();
    for child in body.children().filter(|n| n.is_element()) {
        match local(child) {
            "p" => {
                let para = render_docx_paragraph(child, style_names);
                if !para.trim().is_empty() {
                    blocks.push(para);
                }
            }
            "tbl" => {
                let table = render_docx_table(child, style_names);
                if !table.trim().is_empty() {
                    blocks.push(table);
                }
            }
            _ => {}
        }
    }
    Ok(blocks.join("\n\n"))
}

fn render_docx_table(
    table: roxmltree::Node<'_, '_>,
    style_names: &HashMap<String, String>,
) -> String {
    let mut rows = Vec::new();
    for tr in table
        .children()
        .filter(|n| n.is_element() && local(*n) == "tr")
    {
        let mut row = Vec::new();
        for tc in tr
            .children()
            .filter(|n| n.is_element() && local(*n) == "tc")
        {
            let cell = tc
                .children()
                .filter(|n| n.is_element() && local(*n) == "p")
                .map(|p| render_docx_paragraph(p, style_names))
                .filter(|p| !p.trim().is_empty())
                .collect::<Vec<_>>()
                .join("<br>");
            row.push(cell);
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }
    rows_to_markdown_table(&rows)
}

fn render_docx_paragraph(
    paragraph: roxmltree::Node<'_, '_>,
    style_names: &HashMap<String, String>,
) -> String {
    let mut out = String::new();
    for child in paragraph.children().filter(|n| n.is_element()) {
        match local(child) {
            "r" => out.push_str(&render_docx_run(child)),
            "hyperlink" => {
                for run in child
                    .children()
                    .filter(|n| n.is_element() && local(*n) == "r")
                {
                    out.push_str(&render_docx_run(run));
                }
            }
            _ => {}
        }
    }

    let text = out.trim();
    if text.is_empty() {
        return String::new();
    }
    match docx_heading_prefix(paragraph, style_names) {
        Some(prefix) => format!("{prefix}{text}"),
        None => text.to_string(),
    }
}

fn render_docx_run(run: roxmltree::Node<'_, '_>) -> String {
    let bold = run
        .children()
        .find(|n| n.is_element() && local(*n) == "rPr")
        .is_some_and(|rpr| rpr.children().any(|n| n.is_element() && local(n) == "b"));
    let italic = run
        .children()
        .find(|n| n.is_element() && local(*n) == "rPr")
        .is_some_and(|rpr| rpr.children().any(|n| n.is_element() && local(n) == "i"));
    let mut text = String::new();
    for child in run.children().filter(|n| n.is_element()) {
        match local(child) {
            "t" => text.push_str(child.text().unwrap_or_default()),
            "tab" => text.push(' '),
            "br" | "cr" => text.push('\n'),
            _ => {}
        }
    }
    if text.is_empty() {
        return text;
    }
    match (bold, italic) {
        (true, true) => format!("***{text}***"),
        (true, false) => format!("**{text}**"),
        (false, true) => format!("*{text}*"),
        (false, false) => text,
    }
}

fn docx_heading_prefix(
    paragraph: roxmltree::Node<'_, '_>,
    style_names: &HashMap<String, String>,
) -> Option<&'static str> {
    let style_id = paragraph
        .descendants()
        .find(|n| n.is_element() && local(*n) == "pStyle")
        .and_then(|n| attr_local(n, "val"))?;
    let style = style_names
        .get(style_id)
        .map(String::as_str)
        .unwrap_or(style_id)
        .to_ascii_lowercase();
    if style == "title" {
        return Some("# ");
    }
    if let Some(level) = heading_level(&style) {
        return Some(match level.min(6) {
            1 => "# ",
            2 => "## ",
            3 => "### ",
            4 => "#### ",
            5 => "##### ",
            _ => "###### ",
        });
    }
    None
}

fn heading_level(style: &str) -> Option<usize> {
    let digits: String = style.chars().filter(|c| c.is_ascii_digit()).collect();
    if (style.contains("heading") || style.starts_with('h')) && !digits.is_empty() {
        digits.parse().ok()
    } else {
        None
    }
}

fn render_xml_node(node: roxmltree::Node<'_, '_>, depth: usize, out: &mut String) {
    if !node.is_element() {
        return;
    }
    let indent = "  ".repeat(depth);
    let children: Vec<_> = node.children().filter(|n| n.is_element()).collect();
    if children.is_empty() {
        let text = node.text().unwrap_or_default().trim();
        if text.is_empty() {
            out.push_str(&format!("{indent}- {}\n", node.tag_name().name()));
        } else {
            out.push_str(&format!("{indent}- {}: {}\n", node.tag_name().name(), text));
        }
        return;
    }
    out.push_str(&format!("{indent}- {}\n", node.tag_name().name()));
    for child in children {
        render_xml_node(child, depth + 1, out);
    }
}

fn extract_html_title(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("title").ok()?;
    let title = doc.select(&selector).next()?.text().collect::<String>();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_string())
}

fn needs_honya_html_cleanse(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("<ruby") || lower.contains("gaiji")
}

fn decode_bytes(data: &[u8], charset: Option<&str>) -> String {
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(data) {
        let (text, _, _) = enc.decode(data);
        return text.into_owned();
    }
    if let Some(label) = charset
        && let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes())
    {
        let (text, _, _) = enc.decode(data);
        return text.into_owned();
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    det.feed(&data[..data.len().min(64 * 1024)], data.len() <= 64 * 1024);
    let enc = det.guess(None, chardetng::Utf8Detection::Allow);
    let (text, _, _) = enc.decode(data);
    text.into_owned()
}

fn rows_to_markdown_table(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    render_table_row(&rows[0], width, &mut out);
    out.push('|');
    for _ in 0..width {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in &rows[1..] {
        render_table_row(row, width, &mut out);
    }
    out
}

fn render_table_row(row: &[String], width: usize, out: &mut String) {
    out.push('|');
    for i in 0..width {
        let cell = row.get(i).map(|c| escape_table_cell(c)).unwrap_or_default();
        out.push(' ');
        out.push_str(&cell);
        out.push_str(" |");
    }
    out.push('\n');
}

fn escape_table_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\r', " ")
        .replace('\n', "<br>")
        .trim()
        .to_string()
}

fn normalize_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newline_run = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push('\n');
            }
        } else if ch != '\r' {
            newline_run = 0;
            out.push(ch);
        }
    }
    out
}

fn first_content_line_is_h1(markdown: &str) -> bool {
    markdown
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(is_h1)
        .unwrap_or(false)
}

fn is_h1(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("# ") && !trimmed.starts_with("## ")
}

fn local<'a, 'input>(node: roxmltree::Node<'a, 'input>) -> &'a str {
    node.tag_name().name()
}

fn attr_local<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attr| attr.name() == name)
        .map(|attr| attr.value())
}

fn normalized_ext(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
}

fn display_ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_else(|| "(none)".to_string())
}

fn prettify_stem(stem: &str) -> String {
    let replaced: String = stem
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect();
    let words: Vec<&str> = replaced.split_whitespace().collect();
    let trimmed: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| {
            let lw = w.to_ascii_lowercase();
            !(lw.starts_with('v') && lw[1..].chars().all(|c| c.is_ascii_digit()) && lw.len() > 1)
                && lw != "vol"
        })
        .collect();
    let base = if trimmed.is_empty() { words } else { trimmed };
    base.join(" ").trim().to_string()
}

pub fn importable_files(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();

    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() || !is_supported_import_path(path) {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push((path.to_path_buf(), size));
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extensions_are_bounded_to_requested_formats() {
        assert!(is_supported_import_ext("epub"));
        assert!(is_supported_import_ext("PDF"));
        assert!(is_supported_import_ext("docx"));
        assert!(is_supported_import_ext("csv"));
        assert!(!is_supported_import_ext("pptx"));
        assert!(!is_supported_import_ext("xlsx"));
        assert!(!is_supported_import_ext("png"));
    }

    #[test]
    fn markdown_split_uses_h1_sections() {
        let chapters = split_markdown_chapters("# One\n\na\n\n# Two\n\nb", "Fallback");
        assert_eq!(chapters.len(), 2);
        assert!(chapters[0].body.starts_with("# One"));
        assert!(chapters[1].body.starts_with("# Two"));
    }

    #[test]
    fn markdown_split_adds_title_when_missing() {
        let chapters = split_markdown_chapters("hello", "Fallback");
        assert_eq!(chapters.len(), 1);
        assert!(chapters[0].body.starts_with("# Fallback"));
    }

    #[test]
    fn markitdown_text_conversion_gets_honya_cleanup() {
        let dir =
            std::env::temp_dir().join(format!("honya_markitdown_text_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.txt");
        std::fs::write(&path, "「hello」\n\n\nworld").unwrap();

        let converted = convert_markitdown_path(&path, "Sample", |_| {}).unwrap();
        assert_eq!(converted.chapters.len(), 1);
        assert!(converted.chapters[0].body.contains("“hello”"));
        assert!(!converted.chapters[0].body.contains("\n\n\n"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn csv_conversion_renders_markdown_table() {
        let dir = std::env::temp_dir().join(format!("honya_markitdown_csv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("terms.csv");
        std::fs::write(&path, "jp,th\n猫,แมว\n").unwrap();

        let converted = convert_markitdown_path(&path, "Terms", |_| {}).unwrap();
        assert!(converted.chapters[0].body.contains("| jp | th |"));
        assert!(converted.chapters[0].body.contains("| 猫 | แมว |"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

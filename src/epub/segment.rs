//! Group spine documents into logical chapters.
//!
//! A real chapter spans several spine docs: an `m###` title-header image page,
//! mid-chapter full-page illustrations, and continuation text. `segment` combines
//! a leading run of image-only pages (cover/inserts/title) into one block, starts
//! each chapter at a header-image or TOC anchor while absorbing the following
//! illustrations + continuation, and combines a trailing image run (colophon).
//! Pure + sync (operates on cleansed markdown), so it's testable without fs/zip.

use crate::cleanse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocClass {
    ImageOnly,
    /// Prose whose first visible content is an image link (the `m###` header).
    ChapterHeaderImage,
    Prose,
    /// In-spine TOC/nav page (link list) — skipped.
    NavToc,
    Empty,
}

/// One spine document's contribution to segmentation, in reading order.
#[derive(Debug, Clone)]
pub struct DocInput {
    #[allow(dead_code)]
    pub archive_path: String,
    pub markdown: String,
    /// `Some` iff the path is a TOC content target.
    pub toc_title: Option<String>,
    /// `<a href="*.xhtml…">` link count in the source HTML (nav signal).
    pub internal_link_count: usize,
    /// First `<body class="…">` value, if any.
    pub body_class: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalKind {
    ImageOnly,
    Prose,
}

/// An emitted logical chapter, ready to write to `ch_NNN.md` (title not yet prepended).
#[derive(Debug, Clone)]
pub struct LogicalChapter {
    pub title: Option<String>,
    pub kind: LogicalKind,
    pub body: String,
}

/// Non-whitespace char count of `md` with `![alt](url)` spans removed.
fn prose_char_count(md: &str) -> usize {
    let mut count = 0usize;
    let mut rest = md;
    while let Some(open) = rest.find("![") {
        count += rest[..open].chars().filter(|c| !c.is_whitespace()).count();
        let after = &rest[open..];
        match after.find(')') {
            Some(close) => rest = &after[close + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    count + rest.chars().filter(|c| !c.is_whitespace()).count()
}

fn starts_with_image_link(md: &str) -> bool {
    md.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim_start().starts_with("!["))
        .unwrap_or(false)
}

fn class_says_toc(body_class: Option<&str>) -> bool {
    body_class
        .map(|c| c.split_whitespace().any(|tok| tok.contains("toc")))
        .unwrap_or(false)
}

/// Order is load-bearing: image pages settle first (never a TOC), then the
/// nav/TOC page, then a header-image start, then plain prose.
pub fn classify(doc: &DocInput) -> DocClass {
    if doc.markdown.trim().is_empty() {
        return DocClass::Empty;
    }
    if cleanse::is_image_only(&doc.markdown) {
        return DocClass::ImageOnly;
    }
    // Explicit `toc` body-class, or a page dominated by internal `.xhtml` links;
    // real chapters never link to ~8+ siblings, so the threshold is conservative.
    if class_says_toc(doc.body_class.as_deref())
        || (doc.internal_link_count >= 8
            && prose_char_count(&doc.markdown) < doc.internal_link_count * 40)
    {
        return DocClass::NavToc;
    }
    if starts_with_image_link(&doc.markdown) {
        return DocClass::ChapterHeaderImage;
    }
    DocClass::Prose
}

fn open_prose(doc: &DocInput) -> LogicalChapter {
    LogicalChapter {
        title: doc.toc_title.clone(),
        kind: LogicalKind::Prose,
        body: doc.markdown.clone(),
    }
}

fn absorb(chapter: &mut LogicalChapter, doc: &DocInput) {
    chapter.body.push_str("\n\n");
    chapter.body.push_str(&doc.markdown);
}

/// Close the open chapter; a prose chapter holding only an image (header image,
/// no prose) downgrades to image-only so the pipeline skips it.
fn flush(cur: &mut Option<LogicalChapter>, out: &mut Vec<LogicalChapter>) {
    if let Some(mut ch) = cur.take() {
        ch.body = ch.body.trim().to_string();
        if ch.kind == LogicalKind::Prose && cleanse::is_image_only(&ch.body) {
            ch.kind = LogicalKind::ImageOnly;
        }
        out.push(ch);
    }
}

fn combined_image_block(docs: &[&DocInput]) -> LogicalChapter {
    let body = docs
        .iter()
        .map(|d| d.markdown.trim())
        .collect::<Vec<_>>()
        .join("\n\n");
    LogicalChapter {
        title: None,
        kind: LogicalKind::ImageOnly,
        body: body.trim().to_string(),
    }
}

/// Group spine docs (reading order) into logical chapters.
pub fn segment(docs: &[DocInput]) -> Vec<LogicalChapter> {
    let n = docs.len();
    let classes: Vec<DocClass> = docs.iter().map(classify).collect();

    // No header image and no prose TOC anchor → fall back to one chapter per prose doc.
    let has_any_start_signal = classes.contains(&DocClass::ChapterHeaderImage)
        || docs.iter().zip(&classes).any(|(d, c)| {
            d.toc_title.is_some()
                && !matches!(c, DocClass::NavToc | DocClass::ImageOnly | DocClass::Empty)
        });

    let is_start = |i: usize| -> bool {
        match classes[i] {
            DocClass::NavToc | DocClass::Empty | DocClass::ImageOnly => false,
            DocClass::ChapterHeaderImage => true,
            DocClass::Prose => docs[i].toc_title.is_some() || !has_any_start_signal,
        }
    };

    let mut out: Vec<LogicalChapter> = Vec::new();

    // Front matter: leading run of image-only/empty pages → one combined block.
    let mut i = 0;
    let mut front: Vec<&DocInput> = Vec::new();
    while i < n && matches!(classes[i], DocClass::ImageOnly | DocClass::Empty) && !is_start(i) {
        if classes[i] == DocClass::ImageOnly {
            front.push(&docs[i]);
        }
        i += 1;
    }
    if !front.is_empty() {
        out.push(combined_image_block(&front));
    }

    // Back matter: maximal trailing run of image-only pages (colophon).
    let mut back_start = n;
    while back_start > i && classes[back_start - 1] == DocClass::ImageOnly {
        back_start -= 1;
    }

    // Body: docs[i..back_start] grouped into chapters.
    let mut cur: Option<LogicalChapter> = None;
    let mut j = i;
    while j < back_start {
        match classes[j] {
            DocClass::NavToc | DocClass::Empty => {}
            _ if is_start(j) => {
                flush(&mut cur, &mut out);
                cur = Some(open_prose(&docs[j]));
            }
            DocClass::ImageOnly => match cur.as_mut() {
                Some(ch) => absorb(ch, &docs[j]),
                None => out.push(combined_image_block(&[&docs[j]])),
            },
            _ => match cur.as_mut() {
                Some(ch) => absorb(ch, &docs[j]),
                None => cur = Some(open_prose(&docs[j])),
            },
        }
        j += 1;
    }
    flush(&mut cur, &mut out);

    if back_start < n {
        let back: Vec<&DocInput> = (back_start..n)
            .filter(|&k| classes[k] == DocClass::ImageOnly)
            .map(|k| &docs[k])
            .collect();
        if !back.is_empty() {
            out.push(combined_image_block(&back));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMG: &str = "![ภาพประกอบ](../../images/";

    fn img(name: &str) -> String {
        format!("{IMG}{name})")
    }

    fn doc(path: &str, md: String) -> DocInput {
        DocInput {
            archive_path: path.to_string(),
            markdown: md,
            toc_title: None,
            internal_link_count: 0,
            body_class: None,
        }
    }

    fn doc_titled(path: &str, md: String, title: &str) -> DocInput {
        let mut d = doc(path, md);
        d.toc_title = Some(title.to_string());
        d
    }

    fn header_doc(path: &str, img_name: &str, prose: &str, title: &str) -> DocInput {
        doc_titled(path, format!("{}\n\n{prose}", img(img_name)), title)
    }

    fn toc_doc(path: &str) -> DocInput {
        let mut d = doc(path, "目次\n\nプロローグ\n第一章".to_string());
        d.body_class = Some("p-toc".to_string());
        d.internal_link_count = 14;
        d
    }

    #[test]
    fn classify_image_only() {
        assert_eq!(classify(&doc("p1", img("cover.png"))), DocClass::ImageOnly);
    }

    #[test]
    fn classify_header_image() {
        let d = header_doc("p2", "m005.png", "ある日のこと。", "第一章");
        assert_eq!(classify(&d), DocClass::ChapterHeaderImage);
    }

    #[test]
    fn classify_plain_prose() {
        assert_eq!(classify(&doc("p3", "ただの本文。".to_string())), DocClass::Prose);
    }

    #[test]
    fn classify_nav_toc_by_class() {
        assert_eq!(classify(&toc_doc("p-toc")), DocClass::NavToc);
    }

    #[test]
    fn classify_nav_toc_by_link_count() {
        let mut d = doc("toc", "Contents\n\nA\nB\nC".to_string());
        d.internal_link_count = 12; // no class hint, links dominate
        assert_eq!(classify(&d), DocClass::NavToc);
    }

    #[test]
    fn classify_prose_with_few_links_is_not_toc() {
        let mut d = doc("p", "とても長い本文がここにある。".repeat(20));
        d.internal_link_count = 2;
        assert_eq!(classify(&d), DocClass::Prose);
    }

    #[test]
    fn classify_empty() {
        assert_eq!(classify(&doc("e", "   \n\n".to_string())), DocClass::Empty);
    }

    #[test]
    fn reference_spine_groups_into_logical_chapters() {
        let docs = vec![
            doc("p-cover", img("cover.png")),
            doc("p-fm1", img("k001.png")),
            doc("p-fm2", img("k002-004.png")),
            doc("p-title", img("p001.png")),
            toc_doc("p-toc-001"),
            header_doc("p-001", "m003.png", "プロローグ本文。", "プロローグ"),
            header_doc("p-002", "m005.png", "第一章の本文。", "第一章"),
            doc("p-003", img("p012.png")), // mid-chapter illustration
            doc("p-004", "第一章の続きの本文。".to_string()), // continuation
            header_doc("p-005", "m022.png", "思い出その一。", "思い出1"),
            doc("p-colophon", img("ok.png")),
        ];

        let out = segment(&docs);
        // front block, プロローグ, 第一章, 思い出1, colophon
        assert_eq!(out.len(), 5, "logical chapter count: {:#?}", out);

        assert_eq!(out[0].kind, LogicalKind::ImageOnly);
        assert!(out[0].title.is_none());
        for name in ["cover.png", "k001.png", "k002-004.png", "p001.png"] {
            assert!(out[0].body.contains(name), "front missing {name}: {}", out[0].body);
        }

        assert_eq!(out[1].title.as_deref(), Some("プロローグ"));

        // 第一章 is ONE chapter that absorbed the mid illustration + continuation, in order.
        assert_eq!(out[2].kind, LogicalKind::Prose);
        assert_eq!(out[2].title.as_deref(), Some("第一章"));
        let body = &out[2].body;
        let pos_header = body.find("m005.png").expect("header image present");
        let pos_mid = body.find("p012.png").expect("mid illustration absorbed");
        let pos_cont = body.find("続きの本文").expect("continuation absorbed");
        assert!(pos_header < pos_mid && pos_mid < pos_cont, "order preserved: {body}");

        assert_eq!(out[3].title.as_deref(), Some("思い出1"));

        // Colophon stays its own trailing block, not glued onto 思い出1.
        assert_eq!(out[4].kind, LogicalKind::ImageOnly);
        assert!(out[4].body.contains("ok.png"));
        assert!(!out[3].body.contains("ok.png"));
    }

    #[test]
    fn nav_toc_produces_no_chapter() {
        let docs = vec![
            toc_doc("p-toc"),
            header_doc("p-001", "m003.png", "本文。", "第一章"),
        ];
        let out = segment(&docs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title.as_deref(), Some("第一章"));
    }

    #[test]
    fn front_block_is_image_only() {
        let docs = vec![doc("c", img("cover.png")), doc("k", img("k001.png"))];
        let out = segment(&docs);
        assert_eq!(out.len(), 1);
        assert!(cleanse::is_image_only(&out[0].body), "combined front stays image-only");
    }

    #[test]
    fn fallback_one_chapter_per_prose_doc_absorbing_images() {
        // No m### headers, no toc_title: each prose doc starts a chapter, images absorbed.
        let docs = vec![
            doc("p1", "第一話の本文。".to_string()),
            doc("p2", img("ill1.png")),
            doc("p3", "第二話の本文。".to_string()),
        ];
        let out = segment(&docs);
        assert_eq!(out.len(), 2, "{:#?}", out);
        assert!(out[0].body.contains("第一話") && out[0].body.contains("ill1.png"));
        assert!(out[1].body.contains("第二話"));
    }

    #[test]
    fn header_only_chapter_downgrades_to_image_only() {
        // p1 (image, no prose) joins front matter; p2 is the prose chapter.
        let docs = vec![
            doc_titled("p1", img("m003.png"), "扉"),
            header_doc("p2", "m005.png", "本文。", "第一章"),
        ];
        let out = segment(&docs);
        assert_eq!(out.len(), 2, "{:#?}", out);
        assert_eq!(out[0].kind, LogicalKind::ImageOnly);
        assert_eq!(out[1].kind, LogicalKind::Prose);
    }

    #[test]
    fn all_images_collapse_to_single_block() {
        let docs = vec![doc("a", img("a.png")), doc("b", img("b.png")), doc("c", img("c.png"))];
        let out = segment(&docs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, LogicalKind::ImageOnly);
    }

    #[test]
    fn rendered_prose_chapter_keeps_title_as_first_line() {
        // run_import prepends `# {title}` ABOVE the leading image so scan.rs's
        // first_md_heading recovers it before the image-link line.
        let docs = vec![header_doc("p", "m005.png", "本文。", "第一章")];
        let out = segment(&docs);
        let ch = &out[0];
        let rendered = format!("# {}\n\n{}", ch.title.as_deref().unwrap(), ch.body);
        assert_eq!(rendered.lines().next(), Some("# 第一章"));
        assert!(rendered.contains("m005.png"));
    }
}

//! src/epub/opf.rs — parse the OPF package document: Dublin Core metadata,
//! the manifest, the spine (reading order), and the cover image.
//!
//! Namespace tolerance is deliberate: we match elements both by their proper
//! (namespace, local) tuple AND by bare local name, because real-world EPUBs
//! frequently declare the OPF namespace as the default (so children carry it)
//! or omit it entirely.

use std::collections::HashMap;

use roxmltree::Node;

use super::paths::{dir_of, resolve_href};
use super::{EpubError, ManifestItem, Metadata, Result, SpineEntry, ns};

/// Result of parsing the OPF: everything `import.rs` needs to build an `EpubBook`.
#[derive(Debug, Clone)]
pub struct ParsedOpf {
    pub metadata: Metadata,
    pub manifest: Vec<ManifestItem>,
    pub manifest_by_id: HashMap<String, usize>,
    pub spine: Vec<SpineEntry>,
    /// Manifest id of the EPUB3 nav document (properties contains "nav"), if any.
    pub nav_id: Option<String>,
    /// Manifest id of the NCX (spine `toc=` or media-type x-dtbncx+xml), if any.
    pub ncx_id: Option<String>,
}

/// True when a node's namespace is either `expected` or absent (default-ns
/// tolerance: many EPUBs declare the package/DC ns as the default).
fn ns_matches(actual: Option<&str>, expected: &str) -> bool {
    match actual {
        Some(uri) => uri == expected,
        None => true,
    }
}

/// True if `node` is an element matching `local`, accepting either the OPF
/// namespace or no namespace (default-ns tolerance).
fn is_opf_elem(node: &Node, local: &str) -> bool {
    if !node.is_element() {
        return false;
    }
    let tag = node.tag_name();
    tag.name() == local && ns_matches(tag.namespace(), ns::OPF)
}

/// True if `node` is a Dublin Core element with the given local name. DC always
/// carries its own namespace, but we tolerate a bare local name too.
fn is_dc_elem(node: &Node, local: &str) -> bool {
    if !node.is_element() {
        return false;
    }
    let tag = node.tag_name();
    tag.name() == local && ns_matches(tag.namespace(), ns::DC)
}

/// Concatenate the *direct* text children of an element. We filter on
/// `is_text()` ourselves rather than calling `Node::text()`, because roxmltree's
/// `.text()` returns only the first text node and mixing it with descendant
/// walks can duplicate content; doing it explicitly keeps behavior predictable.
pub fn element_text(node: &Node) -> Option<String> {
    let mut s = String::new();
    for child in node.children() {
        if child.is_text()
            && let Some(t) = child.text() {
                s.push_str(t);
            }
    }
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parse the OPF XML. `opf_path` is the archive-relative path of the OPF itself
/// (used to resolve manifest hrefs against the OPF's directory).
pub fn parse_opf(opf_xml: &str, opf_path: &str) -> Result<ParsedOpf> {
    let doc = roxmltree::Document::parse(opf_xml).map_err(|e| EpubError::Xml {
        context: opf_path.to_string(),
        source: e,
    })?;

    let opf_dir = dir_of(opf_path);

    // ---- metadata ----------------------------------------------------------
    let mut metadata = Metadata::default();
    // EPUB2 cover lives in <meta name="cover" content="ITEM_ID">.
    let mut meta_cover_id: Option<String> = None;

    for node in doc.descendants() {
        if is_dc_elem(&node, "title") && metadata.title.is_none() {
            metadata.title = element_text(&node);
        } else if is_dc_elem(&node, "creator") && metadata.creator.is_none() {
            metadata.creator = element_text(&node);
        } else if is_dc_elem(&node, "language") && metadata.language.is_none() {
            metadata.language = element_text(&node);
        } else if is_opf_elem(&node, "meta") {
            // <meta name="cover" content="..."> (EPUB2 idiom).
            let name = node.attribute("name").unwrap_or("");
            if name == "cover"
                && let Some(content) = node.attribute("content") {
                    meta_cover_id = Some(content.to_string());
                }
        }
    }

    // ---- manifest ----------------------------------------------------------
    let mut manifest: Vec<ManifestItem> = Vec::new();
    let mut manifest_by_id: HashMap<String, usize> = HashMap::new();
    let mut nav_id: Option<String> = None;
    let mut ncx_id: Option<String> = None;
    // EPUB3 cover-image (properties token) resolved path, if found.
    let mut epub3_cover_path: Option<String> = None;

    for node in doc.descendants() {
        if !is_opf_elem(&node, "item") {
            continue;
        }
        let Some(id) = node.attribute("id") else {
            continue;
        };
        let Some(href) = node.attribute("href") else {
            continue;
        };
        let media_type = node.attribute("media-type").unwrap_or("").to_string();
        let properties: Vec<String> = node
            .attribute("properties")
            .map(|p| p.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        let resolved_path = resolve_href(&opf_dir, href);

        let item = ManifestItem {
            id: id.to_string(),
            href: href.to_string(),
            resolved_path: resolved_path.clone(),
            media_type: media_type.clone(),
            properties: properties.clone(),
        };

        if item.has_property("nav") && nav_id.is_none() {
            nav_id = Some(id.to_string());
        }
        if item.has_property("cover-image") && epub3_cover_path.is_none() {
            epub3_cover_path = Some(resolved_path.clone());
        }
        if media_type == "application/x-dtbncx+xml" && ncx_id.is_none() {
            ncx_id = Some(id.to_string());
        }

        let idx = manifest.len();
        manifest_by_id.insert(id.to_string(), idx);
        manifest.push(item);
    }

    // ---- spine (only the first <spine> is authoritative) -------------------
    let mut spine: Vec<SpineEntry> = Vec::new();
    if let Some(spine_node) = doc.descendants().find(|n| is_opf_elem(n, "spine")) {
        // spine `toc` attribute (EPUB2) points at the NCX manifest id.
        if ncx_id.is_none()
            && let Some(toc_ref) = spine_node.attribute("toc")
                && manifest_by_id.contains_key(toc_ref) {
                    ncx_id = Some(toc_ref.to_string());
                }
        for itemref in spine_node.children().filter(|n| is_opf_elem(n, "itemref")) {
            let Some(idref) = itemref.attribute("idref") else {
                continue;
            };
            let linear = itemref
                .attribute("linear")
                .map(|v| v != "no")
                .unwrap_or(true);

            let Some(&idx) = manifest_by_id.get(idref) else {
                return Err(EpubError::DanglingSpineRef(idref.to_string()));
            };
            let mi = &manifest[idx];
            spine.push(SpineEntry {
                idref: idref.to_string(),
                linear,
                resolved_path: mi.resolved_path.clone(),
                media_type: mi.media_type.clone(),
            });
        }
    }

    // ---- cover resolution: EPUB3 properties first, then EPUB2 meta ---------
    metadata.cover_image_path = epub3_cover_path.or_else(|| {
        meta_cover_id
            .and_then(|id| manifest_by_id.get(&id).copied())
            .map(|idx| manifest[idx].resolved_path.clone())
    });

    Ok(ParsedOpf {
        metadata,
        manifest,
        manifest_by_id,
        spine,
        nav_id,
        ncx_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>影の物語</dc:title>
    <dc:creator>テスト著者</dc:creator>
    <dc:language>ja</dc:language>
    <meta name="cover" content="cover-img"/>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="ch1" href="Text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="Text/ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="cover-img" href="Images/cover.png" media-type="image/png" properties="cover-image"/>
  </manifest>
  <spine toc="ncx">
    <itemref idref="ch1"/>
    <itemref idref="ch2" linear="no"/>
  </spine>
</package>"#;

    #[test]
    fn parses_metadata() {
        let p = parse_opf(OPF, "OEBPS/content.opf").unwrap();
        assert_eq!(p.metadata.title.as_deref(), Some("影の物語"));
        assert_eq!(p.metadata.creator.as_deref(), Some("テスト著者"));
        assert_eq!(p.metadata.language.as_deref(), Some("ja"));
    }

    #[test]
    fn resolves_hrefs_against_opf_dir() {
        let p = parse_opf(OPF, "OEBPS/content.opf").unwrap();
        let ch1 = p.manifest.iter().find(|m| m.id == "ch1").unwrap();
        assert_eq!(ch1.resolved_path, "OEBPS/Text/ch1.xhtml");
    }

    #[test]
    fn spine_order_and_linear() {
        let p = parse_opf(OPF, "OEBPS/content.opf").unwrap();
        assert_eq!(p.spine.len(), 2);
        assert_eq!(p.spine[0].resolved_path, "OEBPS/Text/ch1.xhtml");
        assert!(p.spine[0].linear);
        assert!(!p.spine[1].linear);
    }

    #[test]
    fn nav_and_ncx_ids() {
        let p = parse_opf(OPF, "OEBPS/content.opf").unwrap();
        assert_eq!(p.nav_id.as_deref(), Some("nav"));
        assert_eq!(p.ncx_id.as_deref(), Some("ncx"));
    }

    #[test]
    fn epub3_cover_image_wins() {
        let p = parse_opf(OPF, "OEBPS/content.opf").unwrap();
        assert_eq!(
            p.metadata.cover_image_path.as_deref(),
            Some("OEBPS/Images/cover.png")
        );
    }

    #[test]
    fn dangling_spine_ref_errors() {
        let bad = r#"<package xmlns="http://www.idpf.org/2007/opf">
            <manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/></manifest>
            <spine><itemref idref="ghost"/></spine></package>"#;
        let err = parse_opf(bad, "content.opf").unwrap_err();
        assert!(matches!(err, EpubError::DanglingSpineRef(ref s) if s == "ghost"));
    }

    #[test]
    fn element_text_concatenates_direct_text() {
        let doc = roxmltree::Document::parse("<a>Hello <b>X</b> World</a>").unwrap();
        let root = doc.root_element();
        // Only direct text children: "Hello " + " World" = "Hello  World".
        assert_eq!(element_text(&root).as_deref(), Some("Hello  World"));
    }
}

//! Build a flattened `Vec<TocEntry>` from the EPUB2 NCX `navMap` or the EPUB3
//! `nav.xhtml` `<nav epub:type="toc">`, preserving nesting depth and order.

use roxmltree::Node;

use super::opf::element_text;
use super::paths::{dir_of, resolve_href, split_fragment};
use super::{EpubError, Result, TocEntry, ns};

/// Parse an EPUB2 NCX; `ncx_path` resolves `content/@src` against the NCX's dir.
pub fn parse_ncx(ncx_xml: &str, ncx_path: &str) -> Result<Vec<TocEntry>> {
    let doc = roxmltree::Document::parse(ncx_xml).map_err(|e| EpubError::Xml {
        context: ncx_path.to_string(),
        source: e,
    })?;
    let base_dir = dir_of(ncx_path);

    let mut out: Vec<TocEntry> = Vec::new();

    if let Some(nav_map) = doc.descendants().find(|n| is_ncx_elem(n, "navMap")) {
        for np in nav_map.children().filter(|n| is_ncx_elem(n, "navPoint")) {
            walk_navpoint(&np, &base_dir, 0, &mut out);
        }
    }

    Ok(out)
}

/// Namespace match with default-ns tolerance (absent namespace accepted).
fn ns_matches(actual: Option<&str>, expected: &str) -> bool {
    match actual {
        Some(uri) => uri == expected,
        None => true,
    }
}

fn is_ncx_elem(node: &Node, local: &str) -> bool {
    if !node.is_element() {
        return false;
    }
    let tag = node.tag_name();
    tag.name() == local && ns_matches(tag.namespace(), ns::NCX)
}

fn walk_navpoint(np: &Node, base_dir: &str, depth: usize, out: &mut Vec<TocEntry>) {
    let title = np
        .children()
        .find(|n| is_ncx_elem(n, "navLabel"))
        .and_then(|label| label.children().find(|n| is_ncx_elem(n, "text")))
        .and_then(|t| element_text(&t))
        .unwrap_or_default();

    if let Some(content) = np.children().find(|n| is_ncx_elem(n, "content"))
        && let Some(src) = content.attribute("src")
    {
        let (_, fragment) = split_fragment(src);
        let content_path = resolve_href(base_dir, src);
        out.push(TocEntry {
            title,
            content_path,
            fragment,
            depth,
        });
    }

    for child in np.children().filter(|n| is_ncx_elem(n, "navPoint")) {
        walk_navpoint(&child, base_dir, depth + 1, out);
    }
}

/// Parse an EPUB3 nav document (`nav_path` resolves `a/@href`). Prefers
/// `<nav epub:type="toc">`, else the first `<nav>` containing a list.
pub fn parse_nav_xhtml(nav_xml: &str, nav_path: &str) -> Result<Vec<TocEntry>> {
    let doc = roxmltree::Document::parse(nav_xml).map_err(|e| EpubError::Xml {
        context: nav_path.to_string(),
        source: e,
    })?;
    let base_dir = dir_of(nav_path);

    let navs: Vec<Node> = doc
        .descendants()
        .filter(|n| is_xhtml_elem(n, "nav"))
        .collect();

    let toc_nav = navs
        .iter()
        .find(|n| {
            // epub:type="toc" — carried in the OPS namespace.
            n.attribute((ns::OPS, "type"))
                .or_else(|| n.attribute("type"))
                .map(|t| t.split_whitespace().any(|tok| tok == "toc"))
                .unwrap_or(false)
        })
        .copied()
        .or_else(|| navs.first().copied());

    let mut out: Vec<TocEntry> = Vec::new();
    if let Some(nav) = toc_nav
        && let Some(ol) = nav.children().find(|n| is_xhtml_elem(n, "ol"))
    {
        walk_ol(&ol, &base_dir, 0, &mut out);
    }

    Ok(out)
}

fn is_xhtml_elem(node: &Node, local: &str) -> bool {
    if !node.is_element() {
        return false;
    }
    let tag = node.tag_name();
    tag.name() == local && ns_matches(tag.namespace(), ns::XHTML)
}

fn walk_ol(ol: &Node, base_dir: &str, depth: usize, out: &mut Vec<TocEntry>) {
    for li in ol.children().filter(|n| is_xhtml_elem(n, "li")) {
        if let Some(a) = li.children().find(|n| is_xhtml_elem(n, "a")) {
            let title = anchor_text(&a);
            if let Some(href) = a.attribute("href") {
                let (_, fragment) = split_fragment(href);
                let content_path = resolve_href(base_dir, href);
                out.push(TocEntry {
                    title,
                    content_path,
                    fragment,
                    depth,
                });
            }
        }

        if let Some(sub) = li.children().find(|n| is_xhtml_elem(n, "ol")) {
            walk_ol(&sub, base_dir, depth + 1, out);
        }
    }
}

/// Collect all descendant text of an anchor (handles `<a><span>...</span></a>`).
fn anchor_text(a: &Node) -> String {
    let mut s = String::new();
    for d in a.descendants() {
        if d.is_text()
            && let Some(t) = d.text()
        {
            s.push_str(t);
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NCX: &str = r#"<?xml version="1.0"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <navMap>
    <navPoint id="np1"><navLabel><text>第一章</text></navLabel><content src="Text/ch1.xhtml"/>
      <navPoint id="np1a"><navLabel><text>第一節</text></navLabel><content src="Text/ch1.xhtml#s1"/></navPoint>
    </navPoint>
    <navPoint id="np2"><navLabel><text>第二章</text></navLabel><content src="Text/ch2.xhtml"/></navPoint>
  </navMap>
</ncx>"#;

    #[test]
    fn ncx_flattens_with_depth() {
        let t = parse_ncx(NCX, "OEBPS/toc.ncx").unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].title, "第一章");
        assert_eq!(t[0].depth, 0);
        assert_eq!(t[0].content_path, "OEBPS/Text/ch1.xhtml");
        assert_eq!(t[1].title, "第一節");
        assert_eq!(t[1].depth, 1);
        assert_eq!(t[1].fragment.as_deref(), Some("s1"));
        assert_eq!(t[2].title, "第二章");
        assert_eq!(t[2].depth, 0);
    }

    const NAV: &str = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
  <body>
    <nav epub:type="landmarks"><ol><li><a href="cover.xhtml">Cover</a></li></ol></nav>
    <nav epub:type="toc">
      <ol>
        <li><a href="Text/ch1.xhtml">Chapter 1</a>
          <ol><li><a href="Text/ch1.xhtml#s1"><span>Section 1</span></a></li></ol>
        </li>
        <li><a href="Text/ch2.xhtml">Chapter 2</a></li>
      </ol>
    </nav>
  </body>
</html>"#;

    #[test]
    fn nav_picks_toc_and_preserves_depth() {
        let t = parse_nav_xhtml(NAV, "OEBPS/nav.xhtml").unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].title, "Chapter 1");
        assert_eq!(t[0].depth, 0);
        assert_eq!(t[0].content_path, "OEBPS/Text/ch1.xhtml");
        assert_eq!(t[1].title, "Section 1");
        assert_eq!(t[1].depth, 1);
        assert_eq!(t[1].fragment.as_deref(), Some("s1"));
        assert_eq!(t[2].title, "Chapter 2");
        assert_eq!(t[2].depth, 0);
    }
}

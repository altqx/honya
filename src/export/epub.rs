//! EPUB3 export for translated volumes. Writes the required OCF/OPF/nav parts plus
//! one XHTML file per chapter, using hand-built XML and the vendored zip writer.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;

use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use super::blocks::parse_blocks;
use super::book::ExportBook;
use super::html::{blocks_to_xhtml, esc, esc_attr};

/// Deterministic EPUB metadata timestamp.
const MODIFIED: &str = "2024-01-01T00:00:00Z";

/// Write the EPUB to `out_path`.
pub fn write(book: &ExportBook, out_path: &Path) -> io::Result<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out_path)?;
    let mut zip = ZipWriter::new(file);

    // mimetype MUST be the first entry and stored uncompressed (OCF spec).
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/epub+zip")?;

    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let add = |zip: &mut ZipWriter<std::fs::File>, name: &str, bytes: &[u8]| -> io::Result<()> {
        zip.start_file(name, deflated)?;
        zip.write_all(bytes)?;
        Ok(())
    };

    add(&mut zip, "META-INF/container.xml", CONTAINER.as_bytes())?;

    let embedded: HashSet<String> = book.images.iter().cloned().collect();

    add(&mut zip, "OEBPS/front.xhtml", front_xhtml(book).as_bytes())?;
    for ch in &book.chapters {
        let body = blocks_to_xhtml(&parse_blocks(&ch.markdown), "images/", &embedded);
        let doc = chapter_xhtml(&ch.title, &body, &book.language);
        add(
            &mut zip,
            &format!("OEBPS/{}", chapter_href(ch.number)),
            doc.as_bytes(),
        )?;
    }

    for file in &book.images {
        let bytes = std::fs::read(book.images_dir.join(file))?;
        add(&mut zip, &format!("OEBPS/images/{file}"), &bytes)?;
    }

    add(&mut zip, "OEBPS/nav.xhtml", nav_xhtml(book).as_bytes())?;
    add(&mut zip, "OEBPS/content.opf", content_opf(book).as_bytes())?;

    zip.finish()?;
    Ok(())
}

const CONTAINER: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

fn chapter_href(n: u32) -> String {
    format!("chap_{n:03}.xhtml")
}

/// Media type for an image basename, by extension (defaults to PNG).
pub fn media_type(file: &str) -> &'static str {
    match file
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => "image/png",
    }
}

fn xhtml_shell(title: &str, lang: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="{lang}" lang="{lang}">
<head><meta charset="utf-8"/><title>{title}</title></head>
<body>
{body}</body>
</html>"#,
        lang = esc_attr(lang),
        title = esc(title),
        body = body,
    )
}

fn chapter_xhtml(title: &str, body_html: &str, lang: &str) -> String {
    let body = format!("<h1>{}</h1>\n{}", esc(title), body_html);
    xhtml_shell(title, lang, &body)
}

fn front_xhtml(book: &ExportBook) -> String {
    let mut body = format!("<h1>{}</h1>\n", esc(&book.display_title()));
    if let Some(label) = book.volume_label.as_ref().filter(|l| !l.trim().is_empty()) {
        body.push_str(&format!("<p><strong>{}</strong></p>\n", esc(label.trim())));
    }
    let synopsis = book.translated_synopsis.trim();
    if !synopsis.is_empty() {
        body.push_str(&format!("<h2>{}</h2>\n", esc(book.synopsis_heading())));
        for para in synopsis.split("\n\n") {
            let p = para.trim();
            if !p.is_empty() {
                body.push_str(&format!("<p>{}</p>\n", esc(p)));
            }
        }
    }
    xhtml_shell(&book.display_title(), &book.language, &body)
}

fn nav_xhtml(book: &ExportBook) -> String {
    let mut items = String::new();
    items.push_str(&format!(
        "      <li><a href=\"front.xhtml\">{}</a></li>\n",
        esc(&book.display_title())
    ));
    for ch in &book.chapters {
        items.push_str(&format!(
            "      <li><a href=\"{}\">{}</a></li>\n",
            chapter_href(ch.number),
            esc(&ch.title)
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="{lang}" lang="{lang}">
<head><meta charset="utf-8"/><title>สารบัญ</title></head>
<body>
<nav epub:type="toc" id="toc">
  <h1>สารบัญ</h1>
  <ol>
{items}  </ol>
</nav>
</body>
</html>"#,
        lang = esc_attr(&book.language),
        items = items,
    )
}

fn content_opf(book: &ExportBook) -> String {
    let mut manifest = String::from(
        "    <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n\
         \x20   <item id=\"front\" href=\"front.xhtml\" media-type=\"application/xhtml+xml\"/>\n",
    );
    let mut spine = String::from("    <itemref idref=\"front\"/>\n");
    for ch in &book.chapters {
        let id = format!("ch{}", ch.number);
        manifest.push_str(&format!(
            "    <item id=\"{id}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n",
            chapter_href(ch.number)
        ));
        spine.push_str(&format!("    <itemref idref=\"{id}\"/>\n"));
    }
    for (i, file) in book.images.iter().enumerate() {
        manifest.push_str(&format!(
            "    <item id=\"img{i}\" href=\"images/{file}\" media-type=\"{}\"/>\n",
            media_type(file)
        ));
    }
    let identifier = format!("urn:honya:{}:vol{:02}", book.project_id, book.volume_number);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bookid">{id}</dc:identifier>
    <dc:title>{title}</dc:title>
    <dc:language>{lang}</dc:language>
    <meta property="dcterms:modified">{modified}</meta>
  </metadata>
  <manifest>
{manifest}  </manifest>
  <spine>
{spine}  </spine>
</package>"#,
        id = esc(&identifier),
        title = esc(&book.display_title()),
        lang = esc(&book.language),
        modified = MODIFIED,
        manifest = manifest,
        spine = spine,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_type_by_extension() {
        assert_eq!(media_type("a.png"), "image/png");
        assert_eq!(media_type("a.JPG"), "image/jpeg");
        assert_eq!(media_type("a.jpeg"), "image/jpeg");
        assert_eq!(media_type("a.gif"), "image/gif");
        assert_eq!(media_type("noext"), "image/png");
    }
}

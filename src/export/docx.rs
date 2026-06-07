//! DOCX export for editors/proofreaders. Builds minimal OpenXML by hand:
//! headings are bold runs, `**` / `*` become `<w:b/>` / `<w:i/>`, `---` becomes
//! centered `* * *`, and PNG/JPEG/GIF dimensions size inline drawings.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use super::blocks::{Block, Inline, parse_blocks};
use super::book::ExportBook;
use super::html::{esc, esc_attr};

/// English Metric Units per pixel (96-dpi assumption: 914400 EMU/in ÷ 96 px/in).
const EMU_PER_PX: u64 = 9525;
/// Max inline image width — 6 inches of content area.
const MAX_WIDTH_EMU: u64 = 5_486_400;
/// Fallback box when an image's dimensions can't be parsed (~4×3 in).
const FALLBACK_EMU: (u64, u64) = (3_657_600, 2_743_200);

/// Embedded image metadata for the DOCX package.
struct Embedded {
    rid: String,
    media: String,
    /// Display size in EMU (cx, cy).
    extent: (u64, u64),
}

/// Write the DOCX to `out_path`.
pub fn write(book: &ExportBook, out_path: &Path) -> io::Result<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Read bytes, dimensions, and relationship ids for embedded images.
    let mut embedded: HashMap<String, Embedded> = HashMap::new();
    let mut media: Vec<(String, Vec<u8>)> = Vec::new();
    for (i, file) in book.images.iter().enumerate() {
        let bytes = match std::fs::read(book.images_dir.join(file)) {
            Ok(b) => b,
            Err(_) => continue, // gather verified existence; tolerate a race
        };
        let ext = file
            .rsplit('.')
            .next()
            .unwrap_or("png")
            .to_ascii_lowercase();
        let media_name = format!("media/image{}.{}", i + 1, ext);
        let extent = scaled_extent(image_dims(&bytes));
        embedded.insert(
            file.clone(),
            Embedded {
                rid: format!("rId{}", i + 100),
                media: media_name.clone(),
                extent,
            },
        );
        media.push((media_name, bytes));
    }

    let document = document_xml(book, &embedded);

    let file = std::fs::File::create(out_path)?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let add = |zip: &mut ZipWriter<std::fs::File>, name: &str, bytes: &[u8]| -> io::Result<()> {
        zip.start_file(name, opts)?;
        zip.write_all(bytes)?;
        Ok(())
    };

    add(&mut zip, "[Content_Types].xml", content_types().as_bytes())?;
    add(&mut zip, "_rels/.rels", ROOT_RELS.as_bytes())?;
    add(&mut zip, "word/document.xml", document.as_bytes())?;
    add(
        &mut zip,
        "word/_rels/document.xml.rels",
        document_rels(&embedded).as_bytes(),
    )?;
    for (name, bytes) in &media {
        add(&mut zip, &format!("word/{name}"), bytes)?;
    }
    zip.finish()?;
    Ok(())
}

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

fn content_types() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="png" ContentType="image/png"/>
  <Default Extension="jpeg" ContentType="image/jpeg"/>
  <Default Extension="jpg" ContentType="image/jpeg"/>
  <Default Extension="gif" ContentType="image/gif"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#
        .to_string()
}

fn document_rels(embedded: &HashMap<String, Embedded>) -> String {
    let mut rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n",
    );
    // Deterministic order by rId so output is stable.
    let mut entries: Vec<&Embedded> = embedded.values().collect();
    entries.sort_by(|a, b| a.media.cmp(&b.media));
    for e in entries {
        rels.push_str(&format!(
            "  <Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"{}\"/>\n",
            e.rid, e.media
        ));
    }
    rels.push_str("</Relationships>");
    rels
}

fn document_xml(book: &ExportBook, embedded: &HashMap<String, Embedded>) -> String {
    let mut body = String::new();
    let mut drawing_id = 1u32;

    body.push_str(&heading_p(&book.display_title(), 36));
    if let Some(label) = book.volume_label.as_ref().filter(|l| !l.trim().is_empty()) {
        body.push_str(&para_runs(&[run(label.trim(), true, false)]));
    }
    let synopsis = book.synopsis_th.trim();
    if !synopsis.is_empty() {
        body.push_str(&heading_p("เรื่องย่อ", 28));
        for para in synopsis.split("\n\n") {
            let p = para.trim();
            if !p.is_empty() {
                body.push_str(&para_runs(&[run(p, false, false)]));
            }
        }
    }

    for ch in &book.chapters {
        body.push_str(&heading_p(&ch.title, 32));
        for block in parse_blocks(&ch.markdown) {
            body.push_str(&block_xml(&block, embedded, &mut drawing_id));
        }
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
  <w:body>
{body}    <w:sectPr/>
  </w:body>
</w:document>"#,
        body = body
    )
}

fn block_xml(block: &Block, embedded: &HashMap<String, Embedded>, did: &mut u32) -> String {
    match block {
        Block::Heading { level, spans } => {
            let sz = match level {
                1 => 32,
                2 => 28,
                _ => 26,
            };
            let text = inline_plain(spans);
            heading_p(&text, sz)
        }
        Block::Para(spans) => {
            let runs = inline_runs(spans, embedded, did);
            if runs.trim().is_empty() {
                String::new()
            } else {
                format!("    <w:p>{runs}</w:p>\n")
            }
        }
        Block::Image { alt, file } => {
            let inner = image_run(alt, file, embedded, did);
            format!("    <w:p>{inner}</w:p>\n")
        }
        Block::Rule => para_centered("* * *"),
    }
}

/// A heading paragraph: bold run at `half_pt` half-points, with spacing.
fn heading_p(text: &str, half_pt: u32) -> String {
    format!(
        "    <w:p><w:pPr><w:spacing w:before=\"240\" w:after=\"120\"/></w:pPr>\
         <w:r><w:rPr><w:b/><w:sz w:val=\"{half_pt}\"/><w:szCs w:val=\"{half_pt}\"/></w:rPr>\
         <w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>\n",
        esc(text)
    )
}

fn para_centered(text: &str) -> String {
    format!(
        "    <w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>\
         <w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>\n",
        esc(text)
    )
}

fn para_runs(runs: &[String]) -> String {
    format!("    <w:p>{}</w:p>\n", runs.concat())
}

/// A single text run with optional bold/italic.
fn run(text: &str, bold: bool, italic: bool) -> String {
    let mut rpr = String::new();
    if bold {
        rpr.push_str("<w:b/>");
    }
    if italic {
        rpr.push_str("<w:i/>");
    }
    let rpr = if rpr.is_empty() {
        String::new()
    } else {
        format!("<w:rPr>{rpr}</w:rPr>")
    };
    format!(
        "<w:r>{rpr}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
        esc(text)
    )
}

/// Render inline runs (bold/italic/image) as concatenated `<w:r>`/drawings.
fn inline_runs(spans: &[Inline], embedded: &HashMap<String, Embedded>, did: &mut u32) -> String {
    fn walk(
        spans: &[Inline],
        bold: bool,
        italic: bool,
        embedded: &HashMap<String, Embedded>,
        did: &mut u32,
        out: &mut String,
    ) {
        for span in spans {
            match span {
                Inline::Text(t) => out.push_str(&run(t, bold, italic)),
                Inline::Bold(inner) => walk(inner, true, italic, embedded, did, out),
                Inline::Italic(inner) => walk(inner, bold, true, embedded, did, out),
                Inline::Image { alt, file } => out.push_str(&image_run(alt, file, embedded, did)),
            }
        }
    }
    let mut out = String::new();
    walk(spans, false, false, embedded, did, &mut out);
    out
}

/// Flatten inline runs to plain text (for headings, which are a single bold run).
fn inline_plain(spans: &[Inline]) -> String {
    let mut s = String::new();
    for span in spans {
        match span {
            Inline::Text(t) => s.push_str(t),
            Inline::Bold(inner) | Inline::Italic(inner) => s.push_str(&inline_plain(inner)),
            Inline::Image { alt, .. } => s.push_str(alt),
        }
    }
    s
}

/// An inline image drawing run, or the alt text when the image isn't embedded.
fn image_run(alt: &str, file: &str, embedded: &HashMap<String, Embedded>, did: &mut u32) -> String {
    let Some(e) = embedded.get(file) else {
        return if alt.is_empty() {
            String::new()
        } else {
            run(alt, false, false)
        };
    };
    let id = *did;
    *did += 1;
    let (cx, cy) = e.extent;
    let name = esc_attr(file);
    format!(
        "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
<wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\
<wp:docPr id=\"{id}\" name=\"{name}\"/>\
<a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">\
<a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr>\
<pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>",
        cx = cx,
        cy = cy,
        id = id,
        name = name,
        rid = e.rid,
    )
}

/// Scale pixel dimensions to EMU, capping width at `MAX_WIDTH_EMU`.
fn scaled_extent(dims: Option<(u32, u32)>) -> (u64, u64) {
    let Some((w, h)) = dims else {
        return FALLBACK_EMU;
    };
    if w == 0 || h == 0 {
        return FALLBACK_EMU;
    }
    let cx = w as u64 * EMU_PER_PX;
    let cy = h as u64 * EMU_PER_PX;
    if cx <= MAX_WIDTH_EMU {
        (cx, cy)
    } else {
        let cy = cy * MAX_WIDTH_EMU / cx;
        (MAX_WIDTH_EMU, cy)
    }
}

/// Parse the pixel dimensions of a PNG, JPEG, or GIF from its header bytes.
pub fn image_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() >= 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        // IHDR width/height are big-endian u32 at offsets 16 and 20.
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((w, h));
    }
    if bytes.len() >= 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        // Logical screen width/height are little-endian u16 at offsets 6 and 8.
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some((w, h));
    }
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        return jpeg_dims(bytes);
    }
    None
}

/// Walk JPEG marker segments to the start-of-frame and read its height/width.
fn jpeg_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        // Skip fill bytes (runs of 0xFF).
        let mut marker = bytes[i + 1];
        let mut j = i + 1;
        while marker == 0xFF && j + 1 < bytes.len() {
            j += 1;
            marker = bytes[j];
        }
        // Standalone markers (no length): RSTn, SOI, EOI, TEM.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            i = j + 1;
            continue;
        }
        let len_hi = *bytes.get(j + 1)? as usize;
        let len_lo = *bytes.get(j + 2)? as usize;
        let seg_len = (len_hi << 8) | len_lo;
        // SOF markers carry the frame dimensions (exclude DHT/DAC/SOS and friends).
        let is_sof = matches!(marker,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
        if is_sof {
            let h = u16::from_be_bytes([*bytes.get(j + 4)?, *bytes.get(j + 5)?]) as u32;
            let w = u16::from_be_bytes([*bytes.get(j + 6)?, *bytes.get(j + 7)?]) as u32;
            return Some((w, h));
        }
        i = j + 1 + seg_len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_dims_from_ihdr() {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        b.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&640u32.to_be_bytes());
        b.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(image_dims(&b), Some((640, 480)));
    }

    #[test]
    fn jpeg_dims_from_sof0() {
        // Minimal SOF0 JPEG header.
        let b = vec![
            0xFF, 0xD8, // SOI
            0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0xC8, 0x01, 0x2C, 0x03, 0x01, 0x22, 0x00, 0x02,
            0x11, 0x01, 0x03, 0x11, 0x01,
        ];
        assert_eq!(image_dims(&b), Some((300, 200)));
    }

    #[test]
    fn gif_dims_from_header() {
        let mut b = Vec::from(*b"GIF89a");
        b.extend_from_slice(&100u16.to_le_bytes());
        b.extend_from_slice(&50u16.to_le_bytes());
        assert_eq!(image_dims(&b), Some((100, 50)));
    }

    #[test]
    fn docx_has_required_parts_and_embeds_image() {
        use crate::export::book::{ExportBook, ExportChapter};
        use std::io::Read;

        let dir = std::env::temp_dir().join(format!("honya_docx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let images = dir.join("images");
        std::fs::create_dir_all(&images).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&8u32.to_be_bytes());
        png.extend_from_slice(&8u32.to_be_bytes());
        std::fs::write(images.join("p.png"), &png).unwrap();

        let book = ExportBook {
            project_title: "T".to_string(),
            project_id: "t".to_string(),
            volume_number: 1,
            volume_label: None,
            synopsis_th: "ย่อ".to_string(),
            language: "th".to_string(),
            chapters: vec![ExportChapter {
                number: 1,
                title: "บท".to_string(),
                markdown: "เนื้อหา **หนา**\n\n![x](../../images/p.png)".to_string(),
            }],
            images_dir: images.clone(),
            images: vec!["p.png".to_string()],
            warnings: vec![],
        };
        let out = dir.join("out.docx");
        write(&book, &out).unwrap();

        let file = std::fs::File::open(&out).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        for part in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/_rels/document.xml.rels",
        ] {
            assert!(names.iter().any(|n| n == part), "missing {part}");
        }
        assert!(names.iter().any(|n| n.starts_with("word/media/image")));

        let mut doc = String::new();
        zip.by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut doc)
            .unwrap();
        assert!(doc.contains("เนื้อหา"));
        assert!(doc.contains("<w:b/>"));
        assert!(doc.contains("r:embed="));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extent_caps_width() {
        let (cx, cy) = scaled_extent(Some((1000, 500)));
        assert_eq!(cx, MAX_WIDTH_EMU);
        assert_eq!(cy, MAX_WIDTH_EMU / 2);
        assert_eq!(
            scaled_extent(Some((100, 100))),
            (100 * EMU_PER_PX, 100 * EMU_PER_PX)
        );
        assert_eq!(scaled_extent(None), FALLBACK_EMU);
    }
}

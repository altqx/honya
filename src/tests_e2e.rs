//! Crate-internal runtime tests: a UI render smoke test (catches layout panics the
//! type system can't) and a full backend end-to-end (synthetic EPUB → cleanse →
//! mock translation pipeline → translated file on disk). Only compiled under `cargo test`.

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::app::Screen;
use crate::app::overlay::{ImportState, Overlay};
use crate::model::{AppConfig, EventTx, ModelSet};

fn fresh_app() -> App {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    App::new(EventTx(tx), AppConfig::default())
}

/// Render every screen at a range of terminal sizes — including pathologically small
/// ones — and assert nothing panics (layout math, unicode width, slicing).
#[test]
fn renders_all_screens_without_panic() {
    let screens = [
        Screen::Shelf,
        Screen::Project,
        Screen::Translate,
        Screen::Reader,
        Screen::Lexicon,
    ];
    let sizes = [(120u16, 40u16), (80, 24), (50, 16), (30, 10), (16, 6)];

    for &(w, h) in &sizes {
        for screen in screens {
            let mut app = fresh_app();
            app.screen = screen;
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| app.render(f)).unwrap();
        }
    }
}

/// Overlays render over their base screen at small sizes without panicking.
#[test]
fn renders_overlays_without_panic() {
    let sizes = [(120u16, 40u16), (60, 20), (28, 9)];
    for &(w, h) in &sizes {
        // Help + Log are unit overlays.
        for ov in [Overlay::Help(0), Overlay::Log(0)] {
            let mut app = fresh_app();
            app.overlay = ov;
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| app.render(f)).unwrap();
        }
        // Import wizard, name step, with a candidate epub.
        let mut app = fresh_app();
        app.overlay = Overlay::Import(ImportState {
            step: 1,
            epubs: vec![PathBuf::from("/tmp/sample.epub")],
            sel: 0,
            name: "Test Novel".to_string(),
            vol: 1,
            progress: Some((7, 22, "cleansing ch 7/22".to_string())),
        });
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }
}

/// Driving keys through the router must never panic (tab switch, help, nav, esc).
#[test]
fn key_routing_without_panic() {
    let mut app = fresh_app();
    let keys = [
        KeyCode::Char('?'),
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::Char('2'),
        KeyCode::Char('3'),
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Char('l'),
        KeyCode::Char('1'),
        KeyCode::Enter,
    ];
    for code in keys {
        app.on_key(KeyEvent::new(code, KeyModifiers::empty()));
        // Render after each key so any state change is exercised by the renderer.
        let mut term = Terminal::new(TestBackend::new(90, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        assert!(app.running, "navigation keys must not quit the app");
    }
}

/// A confirm modal must dismiss itself when the user presses `y`/Enter, even when
/// the wrapped action (e.g. a placeholder `Action::None`) doesn't reset the overlay.
#[test]
fn confirm_modal_closes_on_yes() {
    use crate::app::Action;
    use crate::app::overlay::Overlay;
    let mut app = fresh_app();
    app.overlay = Overlay::confirm("Confirm?", "body".to_string(), Action::None);
    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::None),
        "confirm modal should close on y"
    );
}

// ---------------------------------------------------------------------------
// End-to-end backend: build a tiny EPUB, run the real ingest + cleanse, then the
// agents pipeline with the offline MockClient, and assert a translated file lands.
// ---------------------------------------------------------------------------

fn build_sample_epub(path: &std::path::Path) {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let file = std::fs::File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let add = |zip: &mut ZipWriter<std::fs::File>, name: &str, bytes: &[u8]| {
        zip.start_file(name, opts).unwrap();
        zip.write_all(bytes).unwrap();
    };

    add(&mut zip, "META-INF/container.xml", br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#);

    add(
        &mut zip,
        "OEBPS/content.opf",
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Test Novel</dc:title>
    <dc:creator>Author</dc:creator>
    <dc:language>ja</dc:language>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="img1" href="images/pic.png" media-type="image/png"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
  </spine>
</package>"#
            .as_bytes(),
    );

    add(
        &mut zip,
        "OEBPS/ch1.xhtml",
        r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>第一章</title></head>
<body>
<p>これは<ruby>漢字<rt>かんじ</rt></ruby>のテストです。</p>
<p><b>太字</b>と<i>斜体</i>。</p>
<p>「こんにちは」と彼は言った。</p>
<p><img src="images/pic.png" alt="x"/></p>
</body></html>"#
            .as_bytes(),
    );

    add(
        &mut zip,
        "OEBPS/images/pic.png",
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\x00fake-png-bytes",
    );

    zip.finish().unwrap();
}

#[tokio::test]
async fn end_to_end_import_and_mock_translate() {
    use crate::agents::pipeline::{PipelineCtx, RunControl, run_pipeline};
    use crate::llm::mock::MockClient;
    use crate::workspace::Workspace;

    // Unique scratch dir (no Date/rand available — use pid + nanos-free counter).
    let base = std::env::temp_dir().join(format!("honya_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let work = base.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let epub_path = base.join("sample.epub");
    build_sample_epub(&epub_path);

    let project_root = base.join("test-novel");
    let images_dir = project_root.join("images");

    // 1. Ingest: extract + parse spine + relocate media.
    let (book, media) =
        crate::epub::import::import_with_media(&epub_path, &work, &images_dir, "images")
            .expect("import_with_media");
    let order = book.reading_order_paths();
    assert_eq!(order.len(), 1, "one spine content doc");

    // 2. Scaffold the project tree.
    crate::workspace::scaffold::create_project(
        &project_root,
        "Test Novel",
        &ModelSet::default(),
        1,
    )
    .expect("create_project");
    let ws = Workspace::new(project_root.clone(), 1);

    // 3. Cleanse each chapter → raw/ch_001.md.
    let doc_path = order[0];
    let xhtml = std::fs::read_to_string(book.disk_path(doc_path)).unwrap();
    let dir = crate::epub::paths::dir_of(doc_path);
    let mut image_map = std::collections::HashMap::new();
    let resolved = crate::epub::paths::resolve_href(&dir, "images/pic.png");
    if let Some(basename) = media.by_resolved_path.get(&resolved) {
        image_map.insert("images/pic.png".to_string(), basename.clone());
    }
    let md = crate::cleanse::xhtml_to_markdown(&xhtml, &image_map);

    // Cleanse-rule spot checks on the real pipeline output.
    assert!(md.contains("漢字 (かんじ)"), "ruby merged: {md}");
    assert!(md.contains("**太字**"), "bold converted: {md}");
    assert!(
        md.contains('“') && md.contains('”'),
        "JP quotes → Thai quotes: {md}"
    );
    assert!(
        md.contains("![ภาพประกอบ](../../images/pic.png)"),
        "image link: {md}"
    );

    crate::workspace::translation::write_raw(&ws, 1, &md).unwrap();

    // 4. Run the real agents pipeline with the offline mock client.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = PipelineCtx {
        client: Arc::new(MockClient::default()) as Arc<dyn crate::llm::client::LlmClient>,
        ws,
        models: ModelSet::default(),
        cfg: AppConfig::default(),
        tx: EventTx(tx),
        ctl: RunControl::new(),
    };
    run_pipeline(ctx, vec![1]).await.expect("run_pipeline");

    // 5. Assert the translated file exists and carries the mock Thai + idempotency marker.
    let translated = project_root.join("Vol_01/translated/ch_001.md");
    let out = std::fs::read_to_string(&translated).expect("translated file written");
    assert!(out.contains("honya:chunk"), "chunk marker present: {out}");
    assert!(out.contains("ข้อความแปลจำลอง"), "mock Thai appended: {out}");

    let _ = std::fs::remove_dir_all(&base);
}

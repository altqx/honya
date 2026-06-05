//! Crate-internal runtime tests: UI render smoke tests plus a full backend end-to-end
//! (synthetic EPUB -> cleanse -> mock translation pipeline -> translated file on disk).

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::app::Screen;
use crate::app::overlay::{ImportState, Overlay, SynopsisState};
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

/// The usage surfaces (Project detail card + context roll-up, split Translate
/// meter) render with realistic nonzero data, at a range of sizes, panic-free.
#[test]
fn renders_usage_surfaces_without_panic() {
    use crate::app::ActiveProject;
    use crate::model::{
        AppEvent, Chapter, ChapterKind, ChapterStatus, Project, TokenUsage, UsageStats, Volume,
    };
    use crate::workspace::Workspace;

    let usage = UsageStats {
        tokens: TokenUsage {
            prompt: 1234,
            completion: 567,
            total: 1801,
        },
        cost_usd: 0.0421,
        tool_calls: 7,
    };
    let dir = std::env::temp_dir().join("honya_usage_render");
    let chapter = Chapter {
        number: 1,
        title: "第一章".to_string(),
        kind: ChapterKind::Prose,
        status: ChapterStatus::Done,
        source_segments: 42,
        total_chunks: 3,
        committed_chunks: 3,
        last_run: None,
        usage,
    };
    let project = Project {
        id: "novel".to_string(),
        dir: dir.clone(),
        title: "Novel".to_string(),
        created: None,
        touched: None,
        volumes: vec![Volume {
            number: 1,
            dir: dir.join("Vol_01"),
            label: None,
            chapters: vec![chapter],
        }],
        models: None,
    };

    let sizes = [(120u16, 40u16), (80, 24), (50, 16), (30, 10)];
    for &(w, h) in &sizes {
        let mut app = fresh_app();
        app.active = Some(ActiveProject {
            project: project.clone(),
            workspace: Workspace::new(dir.clone(), 1),
            client: Arc::new(crate::llm::mock::MockClient::default())
                as Arc<dyn crate::llm::client::LlmClient>,
            models: ModelSet::default(),
        });
        // Drive the Translate split meter with run + chapter sub-totals.
        app.translate.on_app_event(&AppEvent::ChapterStarted { chapter: 1 });
        app.translate.on_app_event(&AppEvent::UsageUpdate {
            run: usage,
            chapter: usage,
        });

        for screen in [Screen::Project, Screen::Translate] {
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
        // The theme picker must render every theme's swatch row at any size.
        for id in crate::theme::ALL_THEMES {
            let mut app = fresh_app();
            app.theme = id.build();
            app.overlay = Overlay::theme(*id);
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
            syn: SynopsisState::new(String::new(), String::new()),
            progress: Some((7, 22, "cleansing ch 7/22".to_string())),
        });
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        // Import wizard, synopsis step (raw entered + a translation in hand).
        let mut app = fresh_app();
        app.overlay = Overlay::Import(ImportState {
            step: 3,
            epubs: vec![PathBuf::from("/tmp/sample.epub")],
            sel: 0,
            name: "Test Novel".to_string(),
            vol: 1,
            syn: SynopsisState::new("主人公は故郷に帰る。".to_string(), "พระเอกกลับบ้านเกิด".to_string()),
            progress: None,
        });
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        // Standalone synopsis editor overlay.
        let mut app = fresh_app();
        app.overlay = Overlay::synopsis_edit(
            "あらすじの原文".to_string(),
            "เรื่องย่อภาษาไทย".to_string(),
        );
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

/// The theme picker live-previews on navigation, commits on Enter, and reverts
/// on Esc — all driven through the real key router.
#[test]
fn theme_picker_preview_commit_and_revert() {
    use crate::app::overlay::Overlay;
    use crate::model::ThemeId;

    // Redirect config writes to a throwaway dir so committing a theme can't
    // clobber the real ~/.config/honya/config.json. No other test touches it.
    let tmp = std::env::temp_dir().join(format!("honya-test-cfg-{}", std::process::id()));
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &tmp);
    }
    // Fail loud if the redirect didn't take, rather than clobber the real config.
    assert!(
        crate::config::config_dir().starts_with(&tmp),
        "config writes must be redirected into the throwaway dir"
    );

    let mut app = fresh_app();
    assert_eq!(app.cfg.theme, ThemeId::Washi, "fresh config defaults to Washi");

    // Open via Ctrl-T.
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert!(matches!(app.overlay, Overlay::Theme(_)), "Ctrl-T opens picker");

    // Navigate down once: the live theme must change, but config stays put.
    let baseline_bg = app.theme.bg;
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()));
    assert_ne!(app.theme.bg, baseline_bg, "preview recolors the live theme");
    assert_eq!(app.cfg.theme, ThemeId::Washi, "preview does not persist");

    // Esc reverts the live theme back to the saved one and closes.
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
    assert!(matches!(app.overlay, Overlay::None), "Esc closes the picker");
    assert_eq!(app.theme.bg, baseline_bg, "Esc reverts the preview");

    // Reopen, move down twice, and commit with Enter — config persists.
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    let committed = crate::theme::ALL_THEMES[2];
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert!(matches!(app.overlay, Overlay::None), "Enter closes the picker");
    assert_eq!(app.cfg.theme, committed, "Enter persists the selected theme");
    assert_eq!(app.theme.bg, committed.build().bg, "live theme matches commit");

    // Restore the process-wide env so no later config-touching test inherits it.
    unsafe {
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}

/// Ctrl-T must open the theme picker even while the Settings overlay is focused
/// (the panel advertises it, and an open overlay otherwise swallows the global).
#[test]
fn ctrl_t_opens_theme_picker_from_settings() {
    use crate::app::overlay::Overlay;

    let mut app = fresh_app();
    app.overlay = Overlay::settings(&app.cfg.clone());
    assert!(matches!(app.overlay, Overlay::Settings(_)));
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert!(
        matches!(app.overlay, Overlay::Theme(_)),
        "Ctrl-T inside Settings opens the theme picker"
    );
}

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

    // Unique scratch dir keyed on pid (no Date/rand available).
    let base = std::env::temp_dir().join(format!("honya_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let work = base.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let epub_path = base.join("sample.epub");
    build_sample_epub(&epub_path);

    let project_root = base.join("test-novel");
    let images_dir = project_root.join("images");

    let (book, media) =
        crate::epub::import::import_with_media(&epub_path, &work, &images_dir, "images")
            .expect("import_with_media");
    let order = book.reading_order_paths();
    assert_eq!(order.len(), 1, "one spine content doc");

    crate::workspace::scaffold::create_project(
        &project_root,
        "Test Novel",
        &ModelSet::default(),
        1,
    )
    .expect("create_project");
    let ws = Workspace::new(project_root.clone(), 1);

    let doc_path = order[0];
    let xhtml = std::fs::read_to_string(book.disk_path(doc_path)).unwrap();
    let dir = crate::epub::paths::dir_of(doc_path);
    let mut image_map = std::collections::HashMap::new();
    let resolved = crate::epub::paths::resolve_href(&dir, "images/pic.png");
    if let Some(basename) = media.by_resolved_path.get(&resolved) {
        image_map.insert("images/pic.png".to_string(), basename.clone());
    }
    let md = crate::cleanse::xhtml_to_markdown(&xhtml, &image_map);

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

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = PipelineCtx {
        client: Arc::new(MockClient::default()) as Arc<dyn crate::llm::client::LlmClient>,
        ws,
        models: ModelSet::default(),
        cfg: AppConfig::default(),
        tx: EventTx(tx),
        ctl: RunControl::new(),
    };
    run_pipeline(ctx, vec![1]).await.expect("run_pipeline");

    // Drain the event channel and confirm cost accounting reached the UI: the
    // last UsageUpdate must carry the BYOK-aware running total, not a hardcoded 0.
    let mut last_run = None;
    let mut chapter_delta = None;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            crate::model::AppEvent::UsageUpdate { run, .. } => last_run = Some(run),
            crate::model::AppEvent::ChapterUsage { chapter: 1, delta } => {
                chapter_delta = Some(delta)
            }
            _ => {}
        }
    }
    let last_run = last_run.expect("at least one UsageUpdate emitted");
    assert!(
        last_run.cost_usd > 0.0,
        "run cost should accumulate from API usage, got {}",
        last_run.cost_usd
    );
    // Total tokens must accumulate from prompt+completion across every call.
    assert!(last_run.tokens.total > 0, "run token total should accumulate");

    // The chapter's spend is emitted for the in-memory roll-up...
    let delta = chapter_delta.expect("ChapterUsage emitted for chapter 1");
    assert!(delta.cost_usd > 0.0 && delta.tokens.total > 0);

    // ...and persisted into VOLUME.md's data block (per-chapter, cumulative).
    let vol_data = crate::workspace::volume::load(&Workspace::new(project_root.clone(), 1));
    let ch1 = vol_data
        .chapter_usage
        .get("1")
        .expect("chapter 1 usage persisted");
    assert!(
        ch1.cost_usd > 0.0 && ch1.tokens.total > 0,
        "persisted per-chapter usage should be nonzero: {ch1:?}"
    );

    let translated = project_root.join("Vol_01/translated/ch_001.md");
    let out = std::fs::read_to_string(&translated).expect("translated file written");
    assert!(out.contains("honya:chunk"), "chunk marker present: {out}");
    assert!(out.contains("ข้อความแปลจำลอง"), "mock Thai appended: {out}");
    // VOLUME.md re-renders a Usage & Cost table from the data block.
    let vol_md = std::fs::read_to_string(project_root.join("Vol_01/VOLUME.md")).unwrap();
    assert!(vol_md.contains("Usage & Cost"), "usage table rendered");

    // Re-scan from disk: the persisted usage must round-trip into the in-memory
    // model and roll up to identical volume and project totals (the path that
    // makes per-project cost survive across sessions).
    let projects = crate::workspace::scan::scan_projects(&base);
    let project = projects
        .iter()
        .find(|p| p.dir == project_root)
        .expect("project re-scanned");
    let scanned = project.volumes[0]
        .chapters
        .iter()
        .find(|c| c.number == 1)
        .expect("chapter 1 scanned");
    let approx = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(approx(scanned.usage.cost_usd, ch1.cost_usd));
    assert_eq!(scanned.usage.tokens.total, ch1.tokens.total);
    assert!(approx(project.volumes[0].usage_total().cost_usd, ch1.cost_usd));
    assert!(approx(project.usage_total().cost_usd, ch1.cost_usd));

    let _ = std::fs::remove_dir_all(&base);
}

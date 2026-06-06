//! Crate-internal runtime tests: UI render smoke tests plus a full backend end-to-end
//! (synthetic EPUB -> cleanse -> mock translation pipeline -> translated file on disk).

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::overlay::{ImportState, Overlay, SynopsisState};
use crate::app::{ActiveProject, App, Screen, Toast};
use crate::model::{AppConfig, EventTx, LogLevel, ModelSet};

fn fresh_app() -> App {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    App::new(EventTx(tx), AppConfig::default())
}

/// Serializes EVERY test that mutates process environment variables. `set_var` /
/// `remove_var` rewrite the shared C `environ`, so a concurrent set from one test
/// races a read/set from any other test regardless of the variable name (it is
/// `unsafe` for exactly this reason). One global lock — held across each test's
/// whole set/use/remove window — is the only sound guard. Poisoning is tolerated
/// so one failing test doesn't cascade. Acquire this before touching env in tests.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            vol: 1,
        });
        // Drive the Translate split meter with run + chapter sub-totals.
        app.translate
            .on_app_event(&AppEvent::ChapterStarted { chapter: 1 });
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
            syn: SynopsisState::new(
                "主人公は故郷に帰る。".to_string(),
                "พระเอกกลับบ้านเกิด".to_string(),
            ),
            progress: None,
        });
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        // Standalone synopsis editor overlay.
        let mut app = fresh_app();
        app.overlay =
            Overlay::synopsis_edit("あらすじの原文".to_string(), "เรื่องย่อภาษาไทย".to_string());
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        // QA inbox: grouped findings (review chunks, a failed chapter, continuity
        // warning/conflict, an unanchored note) with a mid-list selection.
        use crate::app::qa::{QaIssue, QaKind, QaReport, Severity};
        let review = |chapter: u32, chunk: u32, detail: &str| QaIssue {
            chapter: Some(chapter),
            title: format!("第{chapter}章"),
            kind: QaKind::ReviewChunk { chunk },
            detail: detail.to_string(),
        };
        let report = QaReport {
            done: 2,
            review: 1,
            failed: 1,
            issues: vec![
                review(3, 4, "meaning drift on the final sentence"),
                review(3, 8, "honorific mismatch"),
                QaIssue {
                    chapter: Some(7),
                    title: "第七章".to_string(),
                    kind: QaKind::ChapterFailed,
                    detail: String::new(),
                },
                QaIssue {
                    chapter: Some(11),
                    title: "第十一章".to_string(),
                    kind: QaKind::Continuity {
                        severity: Severity::Conflict,
                    },
                    detail: "name romanization differs from the glossary".to_string(),
                },
                QaIssue {
                    chapter: None,
                    title: String::new(),
                    kind: QaKind::Continuity {
                        severity: Severity::Warning,
                    },
                    detail: "timeline ambiguity".to_string(),
                },
            ],
        };
        let mut app = fresh_app();
        app.overlay = Overlay::qa("Novel · Vol.01".to_string(), report);
        if let Overlay::Qa(st) = &mut app.overlay {
            st.sel = 3; // a continuity row, mid-list (exercises windowing)
        }
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        // QA inbox, empty (all-clear) state.
        let mut app = fresh_app();
        app.overlay = Overlay::qa(
            "Novel · Vol.01".to_string(),
            QaReport {
                done: 4,
                ..QaReport::default()
            },
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

#[test]
fn backspace_dismisses_toast_only_when_not_captured() {
    let mut app = fresh_app();
    app.toast = Some(Toast {
        msg: "saved".to_string(),
        level: LogLevel::Info,
    });
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
    assert!(
        app.toast.is_none(),
        "Backspace should dismiss a visible toast when no input owns the key"
    );

    let mut app = fresh_app();
    app.overlay = Overlay::palette();
    app.toast = Some(Toast {
        msg: "still visible".to_string(),
        level: LogLevel::Info,
    });
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
    assert!(
        app.toast.is_some(),
        "overlay text fields keep Backspace instead of dismissing the toast"
    );
    assert!(matches!(app.overlay, Overlay::Palette(_)));

    let mut app = fresh_app();
    app.screen = Screen::Lexicon;
    app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::empty()));
    app.toast = Some(Toast {
        msg: "still visible".to_string(),
        level: LogLevel::Info,
    });
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
    assert!(
        app.toast.is_some(),
        "captured Lexicon text input keeps Backspace instead of dismissing the toast"
    );
}

#[test]
fn project_l_key_is_not_shadowed_by_activity_log() {
    use crate::model::{
        Chapter, ChapterKind, ChapterStatus, Project, TokenUsage, UsageStats, Volume,
    };
    use crate::workspace::Workspace;

    let dir = std::env::temp_dir().join("honya_project_l_key");
    let usage = UsageStats {
        tokens: TokenUsage::default(),
        cost_usd: 0.0,
        tool_calls: 0,
    };
    let chapter = Chapter {
        number: 1,
        title: "第一章".to_string(),
        kind: ChapterKind::Prose,
        status: ChapterStatus::Pending,
        source_segments: 1,
        total_chunks: 0,
        committed_chunks: 0,
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

    let mut app = fresh_app();
    app.screen = Screen::Project;
    app.active = Some(ActiveProject {
        project,
        workspace: Workspace::new(dir, 1),
        client: Arc::new(crate::llm::mock::MockClient::default())
            as Arc<dyn crate::llm::client::LlmClient>,
        models: ModelSet::default(),
        vol: 1,
    });

    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::None),
        "Project l should reach the Project screen instead of opening the activity log"
    );

    app.on_key(KeyEvent::new(KeyCode::Char('`'), KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::Log(_)),
        "Backtick remains the unambiguous activity-log key on Project"
    );
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

    // Serialize against every other env-mutating test (shared process `environ`).
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Redirect config writes to a throwaway dir so committing a theme can't
    // clobber the real ~/.config/honya/config.json.
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
    assert_eq!(
        app.cfg.theme,
        ThemeId::Washi,
        "fresh config defaults to Washi"
    );

    // Open via Ctrl-T.
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert!(
        matches!(app.overlay, Overlay::Theme(_)),
        "Ctrl-T opens picker"
    );

    // Navigate down once: the live theme must change, but config stays put.
    let baseline_bg = app.theme.bg;
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()));
    assert_ne!(app.theme.bg, baseline_bg, "preview recolors the live theme");
    assert_eq!(app.cfg.theme, ThemeId::Washi, "preview does not persist");

    // Esc reverts the live theme back to the saved one and closes.
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::None),
        "Esc closes the picker"
    );
    assert_eq!(app.theme.bg, baseline_bg, "Esc reverts the preview");

    // Reopen, move down twice, and commit with Enter — config persists.
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    let committed = crate::theme::ALL_THEMES[2];
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::None),
        "Enter closes the picker"
    );
    assert_eq!(
        app.cfg.theme, committed,
        "Enter persists the selected theme"
    );
    assert_eq!(
        app.theme.bg,
        committed.build().bg,
        "live theme matches commit"
    );

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

/// A checkpoint left behind by an interrupted run raises the recovery overlay at
/// startup, and `d` discards it (clearing the on-disk file). Drives the real key
/// router so the modal's discard-key wiring is exercised end to end.
#[test]
fn recovery_prompt_appears_and_discards() {
    use crate::workspace::session::{self, SessionCheckpoint};

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = std::env::temp_dir().join(format!("honya_recover_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let session_file = tmp.join("session.json");
    unsafe {
        std::env::set_var("HONYA_SESSION_FILE", &session_file);
    }
    // Fail loud rather than touch the real recovery file.
    assert_eq!(
        session::path(),
        session_file,
        "recovery path must be redirected into the throwaway dir"
    );

    // A real, resumable project on disk (PROJECT.md present).
    let project_dir = tmp.join("re-zero");
    crate::workspace::scaffold::create_project(&project_dir, "Re:Zero", &ModelSet::default(), 1)
        .expect("scaffold project");

    let cp = SessionCheckpoint::new(
        project_dir.clone(),
        "re-zero".to_string(),
        "Re:Zero".to_string(),
        1,
        vec![1, 2],
    );
    session::save(&cp).expect("write checkpoint");

    let mut app = fresh_app();
    app.init_recovery_prompt();
    assert!(
        matches!(app.overlay, Overlay::Modal(_)),
        "an interrupted run raises the recovery modal at startup"
    );
    assert!(
        app.pending_recovery.is_some(),
        "the checkpoint is held pending the user's choice"
    );

    // Discard: clears both the in-memory pending state and the on-disk file.
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::None),
        "discard closes the modal"
    );
    assert!(
        app.pending_recovery.is_none(),
        "discard drops the pending checkpoint"
    );
    assert!(
        session::load().is_none(),
        "discard removes the checkpoint file"
    );

    unsafe {
        std::env::remove_var("HONYA_SESSION_FILE");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `PipelineFinished` retires the recovery checkpoint — the invariant that the
/// file exists only while a run is genuinely in flight.
#[test]
fn pipeline_finished_clears_checkpoint() {
    use crate::model::AppEvent;
    use crate::workspace::session::{self, SessionCheckpoint};

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = std::env::temp_dir().join(format!("honya_recover_fin_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let session_file = tmp.join("session.json");
    unsafe {
        std::env::set_var("HONYA_SESSION_FILE", &session_file);
    }
    assert_eq!(session::path(), session_file, "recovery path redirected");

    let cp = SessionCheckpoint::new(
        tmp.join("proj"),
        "proj".to_string(),
        "Proj".to_string(),
        1,
        vec![1],
    );
    session::save(&cp).expect("write checkpoint");
    assert!(
        session::load().is_some(),
        "checkpoint present before finish"
    );

    let mut app = fresh_app();
    app.pending_recovery = Some(cp);
    app.on_app_event(AppEvent::PipelineFinished {
        chapters_done: 1,
        chapters_failed: 0,
        chapters_need_review: 0,
    });

    assert!(
        session::load().is_none(),
        "a finished run clears the checkpoint file"
    );
    assert!(
        app.pending_recovery.is_none(),
        "a finished run drops the in-memory pending checkpoint"
    );

    unsafe {
        std::env::remove_var("HONYA_SESSION_FILE");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A checkpoint pointing at a project that no longer exists (no PROJECT.md) is
/// stale: `init_recovery_prompt` clears it quietly instead of prompting.
#[test]
fn stale_checkpoint_is_cleared_without_prompting() {
    use crate::workspace::session::{self, SessionCheckpoint};

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = std::env::temp_dir().join(format!("honya_recover_stale_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe {
        std::env::set_var("HONYA_SESSION_FILE", tmp.join("session.json"));
    }

    // Points at a directory with no PROJECT.md → not resumable.
    let cp = SessionCheckpoint::new(
        tmp.join("ghost"),
        "ghost".to_string(),
        "Ghost".to_string(),
        1,
        vec![1],
    );
    session::save(&cp).unwrap();

    let mut app = fresh_app();
    app.init_recovery_prompt();
    assert!(
        matches!(app.overlay, Overlay::None),
        "a stale checkpoint must not raise the recovery prompt"
    );
    assert!(app.pending_recovery.is_none());
    assert!(
        session::load().is_none(),
        "a stale checkpoint is cleared from disk"
    );

    unsafe {
        std::env::remove_var("HONYA_SESSION_FILE");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// When every queued chapter is already finished on disk (the crash landed right
/// after the last commit), `init_recovery_prompt` clears the checkpoint instead of
/// nagging — there is nothing left to resume.
#[test]
fn all_done_checkpoint_is_cleared_without_prompting() {
    use crate::model::{Chapter, ChapterKind, ChapterStatus, Project, Volume};
    use crate::workspace::session::{self, SessionCheckpoint};

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = std::env::temp_dir().join(format!("honya_recover_done_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe {
        std::env::set_var("HONYA_SESSION_FILE", tmp.join("session.json"));
    }

    // A real project on disk so the checkpoint is "resumable" (PROJECT.md exists).
    let project_dir = tmp.join("novel");
    crate::workspace::scaffold::create_project(&project_dir, "Novel", &ModelSet::default(), 1)
        .expect("scaffold project");

    let cp = SessionCheckpoint::new(
        project_dir.clone(),
        "novel".to_string(),
        "Novel".to_string(),
        1,
        vec![1, 2],
    );
    session::save(&cp).unwrap();

    let mut app = fresh_app();
    // Inject the project with both queued chapters already Done so recovery_progress
    // computes done == total (fresh_app's CWD scan won't contain this temp project).
    let done_chapter = |n: u32| Chapter {
        number: n,
        title: format!("Chapter {n}"),
        kind: ChapterKind::Prose,
        status: ChapterStatus::Done,
        source_segments: 1,
        total_chunks: 1,
        committed_chunks: 1,
        last_run: None,
        usage: Default::default(),
    };
    app.projects.push(Project {
        id: "novel".to_string(),
        dir: project_dir.clone(),
        title: "Novel".to_string(),
        created: None,
        touched: None,
        volumes: vec![Volume {
            number: 1,
            dir: project_dir.join("Vol_01"),
            label: None,
            chapters: vec![done_chapter(1), done_chapter(2)],
        }],
        models: None,
    });

    app.init_recovery_prompt();
    assert!(
        matches!(app.overlay, Overlay::None),
        "an already-finished run must not raise the recovery prompt"
    );
    assert!(app.pending_recovery.is_none());
    assert!(
        session::load().is_none(),
        "an already-finished run's checkpoint is cleared"
    );

    unsafe {
        std::env::remove_var("HONYA_SESSION_FILE");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Resuming a run whose project has vanished since the checkpoint was written
/// fails gracefully: the checkpoint is cleared and the user gets an error toast,
/// rather than the app silently doing nothing or panicking.
#[test]
fn resume_with_missing_project_clears_and_reports() {
    use crate::workspace::session::{self, SessionCheckpoint};

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = std::env::temp_dir().join(format!("honya_recover_gone_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe {
        std::env::set_var("HONYA_SESSION_FILE", tmp.join("session.json"));
    }

    // Scaffold a real project so the prompt appears, then delete it to simulate the
    // project being removed between the crash and the resume attempt.
    let project_dir = tmp.join("novel");
    crate::workspace::scaffold::create_project(&project_dir, "Novel", &ModelSet::default(), 1)
        .expect("scaffold project");
    let cp = SessionCheckpoint::new(
        project_dir.clone(),
        "novel".to_string(),
        "Novel".to_string(),
        1,
        vec![1],
    );
    session::save(&cp).unwrap();

    let mut app = fresh_app();
    app.init_recovery_prompt();
    assert!(matches!(app.overlay, Overlay::Modal(_)), "prompt appears");

    // Project disappears before the user picks "resume".
    std::fs::remove_dir_all(&project_dir).unwrap();

    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::None),
        "the modal closes even when resume can't find the project"
    );
    assert!(
        app.pending_recovery.is_none(),
        "a failed resume drops the pending checkpoint"
    );
    assert!(
        session::load().is_none(),
        "a failed resume clears the unusable checkpoint"
    );
    let toast = app.toast.as_ref().expect("an error toast is shown");
    assert!(
        matches!(toast.level, LogLevel::Error),
        "the failure is surfaced as an error toast"
    );

    unsafe {
        std::env::remove_var("HONYA_SESSION_FILE");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The QA inbox gathers the active volume's flagged issues from disk — a
/// review-needed chunk (with the reviewer's reason) and a failed chapter — and
/// Enter on a finding closes the overlay and jumps to that chapter in the Reader.
#[tokio::test]
async fn qa_overlay_gathers_and_navigates() {
    use crate::app::qa::{self, QaKind};
    use crate::model::{Chapter, ChapterKind, ChapterStatus, Project, UsageStats, Volume};
    use crate::workspace::Workspace;

    let dir = std::env::temp_dir().join(format!("honya_qa_nav_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ws = Workspace::new(dir.clone(), 1);

    // Flag chunk 5 (index 4) of chapter 3 as review-needed on disk, with a reason.
    crate::workspace::translation::append_chunk_needs_review(
        &ws,
        3,
        4,
        "คำแปลที่ยังไม่ผ่านการตรวจ",
        3,
        "meaning drift on the final line",
    )
    .await
    .unwrap();

    let chapter = |n: u32, status: ChapterStatus| Chapter {
        number: n,
        title: format!("第{n}章"),
        kind: ChapterKind::Prose,
        status,
        source_segments: 1,
        total_chunks: 0,
        committed_chunks: 0,
        last_run: None,
        usage: UsageStats::default(),
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
            chapters: vec![
                chapter(3, ChapterStatus::NeedsReview),
                chapter(7, ChapterStatus::Failed),
            ],
        }],
        models: None,
    };
    let active = ActiveProject {
        project,
        workspace: Workspace::new(dir.clone(), 1),
        client: Arc::new(crate::llm::mock::MockClient::default())
            as Arc<dyn crate::llm::client::LlmClient>,
        models: ModelSet::default(),
        vol: 1,
    };

    // collect() surfaces the review chunk (with reason) and the failed chapter.
    let report = qa::collect(&active);
    assert_eq!(report.review, 1, "one NeedsReview chapter counted");
    assert_eq!(report.failed, 1, "one failed chapter counted");
    let review = report
        .issues
        .iter()
        .find(|i| matches!(i.kind, QaKind::ReviewChunk { chunk: 4 }))
        .expect("review-needed chunk surfaced");
    assert_eq!(review.chapter, Some(3));
    assert!(
        review.detail.contains("meaning drift"),
        "reviewer reason surfaced: {:?}",
        review.detail
    );
    assert!(
        report
            .issues
            .iter()
            .any(|i| matches!(i.kind, QaKind::ChapterFailed) && i.chapter == Some(7)),
        "failed chapter surfaced as a finding"
    );
    // Sorted by chapter → the first finding belongs to chapter 3.
    assert_eq!(report.issues.first().and_then(|i| i.chapter), Some(3));

    // Enter on the first finding closes the overlay and jumps to its chapter.
    let mut app = fresh_app();
    app.active = Some(active);
    app.screen = Screen::Project;
    app.overlay = Overlay::qa("Novel · Vol.01".to_string(), report);
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::None),
        "QA overlay closes on Enter"
    );
    assert_eq!(
        app.screen,
        Screen::Reader,
        "Enter on a finding jumps to the Reader"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `collect()` reads continuity notes from VOLUME.md (skipping info, keeping
/// warning/conflict, case-insensitively), and the overlay handles edge keys:
/// Enter on an unanchored note is a no-op, and j/k clamp at the list ends.
#[test]
fn qa_collect_continuity_and_key_edges() {
    use crate::app::qa::{self, QaKind, Severity};
    use crate::model::{
        Chapter, ChapterKind, ChapterStatus, ContinuityNote, Project, UsageStats, Volume,
    };
    use crate::workspace::Workspace;

    let dir = std::env::temp_dir().join(format!("honya_qa_cont_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Vol_01")).unwrap();
    let ws = Workspace::new(dir.clone(), 1);

    let note = |chapter: Option<u32>, severity: &str, text: &str| ContinuityNote {
        chapter,
        severity: severity.to_string(),
        kind: None,
        note: text.to_string(),
    };
    // Warning + conflict are QA issues; info is not; severity match is case-insensitive.
    crate::workspace::volume::add_note(&ws, note(Some(3), "warning", "glossary drift")).unwrap();
    crate::workspace::volume::add_note(&ws, note(Some(3), "conflict", "name romanization"))
        .unwrap();
    crate::workspace::volume::add_note(&ws, note(Some(3), "info", "trivial aside")).unwrap();
    crate::workspace::volume::add_note(&ws, note(None, "Warning", "timeline ambiguity")).unwrap();

    let chapter = Chapter {
        number: 3,
        title: "第三章".to_string(),
        kind: ChapterKind::Prose,
        status: ChapterStatus::Done,
        source_segments: 1,
        total_chunks: 0,
        committed_chunks: 0,
        last_run: None,
        usage: UsageStats::default(),
    };
    let active = ActiveProject {
        project: Project {
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
        },
        workspace: Workspace::new(dir.clone(), 1),
        client: Arc::new(crate::llm::mock::MockClient::default())
            as Arc<dyn crate::llm::client::LlmClient>,
        models: ModelSet::default(),
        vol: 1,
    };

    let report = qa::collect(&active);
    // Info note dropped; warning + conflict (anchored) + warning (unanchored) kept.
    assert_eq!(
        report.issues.len(),
        3,
        "info note skipped: {:?}",
        report.issues
    );
    assert_eq!(report.done, 1);
    assert_eq!(report.review, 0);
    assert!(
        report.issues.iter().any(|i| i.chapter == Some(3)
            && matches!(
                i.kind,
                QaKind::Continuity {
                    severity: Severity::Conflict
                }
            )),
        "conflict note surfaced"
    );
    // The anchored notes resolve their chapter title; the unanchored note has none.
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.chapter == Some(3) && i.title == "第三章"),
        "anchored note carries the chapter title"
    );
    let unanchored = report
        .issues
        .iter()
        .find(|i| i.chapter.is_none())
        .expect("unanchored warning surfaced (case-insensitive)");
    assert!(matches!(
        unanchored.kind,
        QaKind::Continuity {
            severity: Severity::Warning
        }
    ));
    // Chapter-sorted: the unanchored (None → u32::MAX) note sorts last.
    assert!(report.issues.last().unwrap().chapter.is_none());

    // Drive the overlay through the real router. Selecting the last (unanchored)
    // finding and pressing Enter is a no-op: the overlay stays open, no jump.
    let n = report.issues.len();
    let mut app = fresh_app();
    app.active = Some(active);
    app.screen = Screen::Project;
    app.overlay = Overlay::qa("Novel · Vol.01".to_string(), report);

    let sel = |app: &App| match &app.overlay {
        Overlay::Qa(st) => st.sel,
        _ => usize::MAX,
    };
    // j past the end clamps at n-1.
    for _ in 0..(n + 2) {
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()));
    }
    assert_eq!(sel(&app), n - 1, "j clamps at the last finding");

    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::Qa(_)),
        "Enter on an unanchored note keeps the overlay open"
    );
    assert_eq!(
        app.screen,
        Screen::Project,
        "Enter on an unanchored note does not navigate"
    );

    // k past the top clamps at 0.
    for _ in 0..(n + 2) {
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty()));
    }
    assert_eq!(sel(&app), 0, "k clamps at the first finding");

    // An empty report tolerates j/k/Enter without panicking.
    let mut empty = fresh_app();
    empty.overlay = Overlay::qa("Novel · Vol.01".to_string(), qa::QaReport::default());
    for code in [KeyCode::Char('j'), KeyCode::Char('k'), KeyCode::Enter] {
        empty.on_key(KeyEvent::new(code, KeyModifiers::empty()));
    }
    assert!(matches!(empty.overlay, Overlay::Qa(_)));

    let _ = std::fs::remove_dir_all(&dir);
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
    assert!(
        last_run.tokens.total > 0,
        "run token total should accumulate"
    );

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
    assert!(approx(
        project.volumes[0].usage_total().cost_usd,
        ch1.cost_usd
    ));
    assert!(approx(project.usage_total().cost_usd, ch1.cost_usd));

    let _ = std::fs::remove_dir_all(&base);
}

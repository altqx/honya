//! Crate-internal runtime tests: UI render smoke tests plus a full backend end-to-end
//! (synthetic EPUB -> cleanse -> mock translation pipeline -> translated file on disk).

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::overlay::Overlay;
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

#[test]
fn reader_note_overlay_saves_line_annotation() {
    use crate::model::{Chapter, ChapterKind, ChapterStatus, Project, UsageStats, Volume};
    use crate::workspace::Workspace;

    let dir = std::env::temp_dir().join(format!("honya_reader_note_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ws = Workspace::new(dir.clone(), 1);
    crate::workspace::translation::write_raw(&ws, 1, "# 第一章\n原文一\n原文二").unwrap();
    std::fs::create_dir_all(ws.translated(1).parent().unwrap()).unwrap();
    std::fs::write(ws.translated(1), "แถวหนึ่ง\nแถวสอง\n").unwrap();

    let chapter = Chapter {
        number: 1,
        title: "第一章".to_string(),
        kind: ChapterKind::Prose,
        status: ChapterStatus::Done,
        source_segments: 2,
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
        workspace: ws.clone(),
        client: Some(Arc::new(crate::llm::mock::MockClient::default())
            as Arc<dyn crate::llm::client::LlmClient>),
        models: ModelSet::default(),
        vol: 1,
    };

    let mut app = fresh_app();
    app.active = Some(active);
    app.reader.load(&ws, 1);
    app.screen = Screen::Reader;
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
    match &app.overlay {
        Overlay::ReaderNote(st) => {
            assert_eq!(st.chapter, 1);
            assert_eq!(st.line, 2);
        }
        other => panic!("expected ReaderNote overlay, got {other:?}"),
    }

    for ch in "awkward phrasing".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert!(matches!(app.overlay, Overlay::None));

    let annotations = crate::workspace::volume::reader_annotations(&ws, 1);
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].line, 2);
    assert_eq!(annotations[0].note, "awkward phrasing");

    let mut term = Terminal::new(TestBackend::new(90, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let _ = std::fs::remove_dir_all(&dir);
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
        client: Some(Arc::new(crate::llm::mock::MockClient::default())
            as Arc<dyn crate::llm::client::LlmClient>),
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

/// Each Welcome menu row maps to its expected Action, and the menu wraps + Esc
/// dismisses (which the App turns into persisting onboarding completion).
#[test]
fn welcome_menu_returns_expected_actions() {
    use crate::app::Action;

    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
    let mk = || Overlay::welcome(false, false);

    // Row 0 → create the sample project.
    assert!(matches!(mk().handle_key(enter), Action::CreateSample));

    // Row 1 → open import.
    let mut ov = mk();
    ov.handle_key(down);
    assert!(matches!(ov.handle_key(enter), Action::OpenImport));

    // Row 2 → open Settings (focused on the key field).
    let mut ov = mk();
    ov.handle_key(down);
    ov.handle_key(down);
    assert!(
        matches!(ov.handle_key(enter), Action::ShowOverlay(b) if matches!(*b, Overlay::Settings(_)))
    );

    // Row 3 → dismiss.
    let mut ov = mk();
    for _ in 0..3 {
        ov.handle_key(down);
    }
    assert!(matches!(ov.handle_key(enter), Action::DismissWelcome));

    // Up from row 0 wraps to the last row (dismiss).
    let mut ov = mk();
    ov.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
    assert!(matches!(ov.handle_key(enter), Action::DismissWelcome));

    // Esc dismisses outright.
    assert!(matches!(
        mk().handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
        Action::DismissWelcome
    ));
}

/// The API-key field in Settings is editable and surfaces the typed key on save;
/// under an env override it is read-only and save leaves the config key untouched.
#[test]
fn settings_api_key_field_edits_and_respects_env_override() {
    use crate::app::Action;
    use crate::app::overlay::SettingsState;

    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());

    // Editable case: type into the key field (index 4), Enter → SaveSettings{api_key}.
    let mut ov = Overlay::Settings(SettingsState {
        base_url: "https://x".into(),
        orchestrator: "o".into(),
        translator: "t".into(),
        reviewer: "r".into(),
        api_key: String::new(),
        api_key_env: false,
        update_mode: crate::model::UpdateMode::Auto,
        max_attempts: "3".into(),
        field: 4,
    });
    for c in "sk-or-1".chars() {
        ov.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
    }
    match ov.handle_key(enter) {
        Action::SaveSettings { api_key, .. } => assert_eq!(api_key.as_deref(), Some("sk-or-1")),
        other => panic!("expected SaveSettings, got {other:?}"),
    }

    // Env-override case: typing is ignored and save passes api_key: None.
    let mut ov = Overlay::Settings(SettingsState {
        base_url: "u".into(),
        orchestrator: "o".into(),
        translator: "t".into(),
        reviewer: "r".into(),
        api_key: "saved".into(),
        api_key_env: true,
        update_mode: crate::model::UpdateMode::Auto,
        max_attempts: "3".into(),
        field: 4,
    });
    ov.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty()));
    ov.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
    match &ov {
        Overlay::Settings(st) => assert_eq!(st.api_key, "saved", "env key field is read-only"),
        _ => panic!("settings overlay"),
    }
    match ov.handle_key(enter) {
        Action::SaveSettings { api_key, .. } => {
            assert!(
                api_key.is_none(),
                "env override → config key left untouched"
            )
        }
        other => panic!("expected SaveSettings, got {other:?}"),
    }
}

/// Ctrl-U in Settings toggles the startup update mode in place, and Enter saves the
/// toggled value through `SaveSettings` (it is not a focusable text field).
#[test]
fn settings_ctrl_u_toggles_update_mode_and_saves_it() {
    use crate::app::Action;
    use crate::app::overlay::SettingsState;
    use crate::model::UpdateMode;

    let mut ov = Overlay::Settings(SettingsState {
        base_url: "u".into(),
        orchestrator: "o".into(),
        translator: "t".into(),
        reviewer: "r".into(),
        api_key: String::new(),
        api_key_env: false,
        update_mode: UpdateMode::Auto,
        max_attempts: "3".into(),
        field: 0,
    });

    // Ctrl-U flips Auto → Notify without typing into the focused field.
    ov.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    match &ov {
        Overlay::Settings(st) => {
            assert_eq!(st.update_mode, UpdateMode::Notify);
            assert_eq!(st.base_url, "u", "Ctrl-U must not type into the field");
        }
        _ => panic!("settings overlay"),
    }

    // Enter carries the toggled mode out for persistence.
    match ov.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())) {
        Action::SaveSettings { update_mode, .. } => assert_eq!(update_mode, UpdateMode::Notify),
        other => panic!("expected SaveSettings, got {other:?}"),
    }
}

/// The retries field accepts digits only, and Enter carries the parsed (clamped)
/// attempt count out through `SaveSettings`.
#[test]
fn settings_retries_field_is_digit_only_and_clamped() {
    use crate::app::Action;
    use crate::app::overlay::SettingsState;

    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());

    let mk = || {
        Overlay::Settings(SettingsState {
            base_url: "u".into(),
            orchestrator: "o".into(),
            translator: "t".into(),
            reviewer: "r".into(),
            api_key: String::new(),
            api_key_env: false,
            update_mode: crate::model::UpdateMode::Auto,
            // Focus the retries field (index 5).
            max_attempts: String::new(),
            field: 5,
        })
    };

    // Non-digits are dropped; digits accumulate.
    let mut ov = mk();
    for c in "a1b2".chars() {
        ov.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
    }
    match &ov {
        Overlay::Settings(st) => assert_eq!(st.max_attempts, "12", "letters ignored, digits kept"),
        _ => panic!("settings overlay"),
    }
    match ov.handle_key(enter) {
        Action::SaveSettings { max_attempts, .. } => assert_eq!(max_attempts, 12),
        other => panic!("expected SaveSettings, got {other:?}"),
    }

    // Empty / 0 clamps up to 1; oversized clamps down to 20.
    let mut empty = mk();
    match empty.handle_key(enter) {
        Action::SaveSettings { max_attempts, .. } => assert_eq!(max_attempts, 1, "empty → 1"),
        other => panic!("expected SaveSettings, got {other:?}"),
    }

    let mut big = mk();
    for c in "999".chars() {
        big.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
    }
    match big.handle_key(enter) {
        Action::SaveSettings { max_attempts, .. } => assert_eq!(max_attempts, 20, "capped at 20"),
        other => panic!("expected SaveSettings, got {other:?}"),
    }
}

/// First-run onboarding: the Welcome overlay opens once on an empty, never-onboarded
/// shelf and is suppressed for already-onboarded users, users with projects, and
/// whenever another overlay (e.g. crash recovery) already has priority.
#[test]
fn onboarding_shows_welcome_on_first_run_only() {
    // Redirect config writes (mark_onboarded persists) and serialize env access.
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("honya-onboard-cfg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &tmp);
    }
    assert!(crate::config::config_dir().starts_with(&tmp));

    let dummy_project = || crate::model::Project {
        id: "x".into(),
        dir: PathBuf::from("/tmp/honya-x"),
        title: "X".into(),
        created: None,
        touched: None,
        volumes: vec![],
        models: None,
    };

    // Fresh + empty + not onboarded → Welcome opens and the flag is persisted.
    let mut app = fresh_app();
    app.projects.clear();
    app.cfg.onboarded = false;
    app.overlay = Overlay::None;
    app.init_onboarding();
    assert!(
        matches!(app.overlay, Overlay::Welcome(_)),
        "first run shows Welcome"
    );
    assert!(app.cfg.onboarded, "shown once → marked onboarded");

    // Already onboarded → nothing opens.
    let mut app = fresh_app();
    app.projects.clear();
    app.cfg.onboarded = true;
    app.overlay = Overlay::None;
    app.init_onboarding();
    assert!(
        matches!(app.overlay, Overlay::None),
        "onboarded → no Welcome"
    );

    // Returning user (has projects) without the flag → quietly marked, no Welcome.
    let mut app = fresh_app();
    app.cfg.onboarded = false;
    app.overlay = Overlay::None;
    app.projects = vec![dummy_project()];
    app.init_onboarding();
    assert!(
        matches!(app.overlay, Overlay::None),
        "existing projects suppress Welcome"
    );
    assert!(app.cfg.onboarded, "returning user marked onboarded");

    // Another overlay already up (recovery prompt) keeps priority.
    let mut app = fresh_app();
    app.projects.clear();
    app.cfg.onboarded = false;
    app.overlay = Overlay::Help(0);
    app.init_onboarding();
    assert!(
        matches!(app.overlay, Overlay::Help(_)),
        "a pending overlay wins over Welcome"
    );

    unsafe {
        std::env::remove_var("XDG_CONFIG_HOME");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// remove_project_dir deletes a real project directory but refuses anything that is
/// not a honya project (no PROJECT.md), guarding the recursive delete.
#[test]
fn remove_project_dir_guards_non_projects() {
    let base = std::env::temp_dir().join(format!("honya-rmguard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    // A directory without PROJECT.md must be refused and left intact.
    let bystander = base.join("not-a-project");
    std::fs::create_dir_all(&bystander).unwrap();
    assert!(crate::app::remove_project_dir(&bystander).is_err());
    assert!(bystander.exists(), "non-project directory is not deleted");

    // A real project directory (has PROJECT.md) is removed wholesale.
    let proj = base.join("re-zero");
    std::fs::create_dir_all(proj.join("Vol_01/translated")).unwrap();
    std::fs::write(proj.join("PROJECT.md"), "# Re:Zero\n").unwrap();
    assert!(crate::app::remove_project_dir(&proj).is_ok());
    assert!(!proj.exists(), "project directory is deleted");

    let _ = std::fs::remove_dir_all(&base);
}

/// Pressing `d` on the shelf then confirming actually deletes the project directory
/// from disk and closes the overlay (the action used to be a no-op stub).
#[test]
fn shelf_delete_confirm_removes_project() {
    let base = std::env::temp_dir().join(format!("honya-delflow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("re-zero");
    std::fs::create_dir_all(dir.join("Vol_01/translated")).unwrap();
    std::fs::write(dir.join("PROJECT.md"), "# Re:Zero\n").unwrap();

    let mut app = fresh_app();
    app.screen = Screen::Shelf;
    app.projects = vec![crate::model::Project {
        id: "re-zero".into(),
        dir: dir.clone(),
        title: "Re:Zero".into(),
        created: None,
        touched: None,
        volumes: vec![],
        models: None,
    }];
    app.shelf.select_first();

    // `d` raises a confirm modal whose confirm action is the real delete.
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()));
    assert!(
        matches!(app.overlay, Overlay::Modal(_)),
        "delete asks for confirmation first"
    );
    // `y` confirms → the directory is gone and the overlay closes.
    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
    assert!(
        !dir.exists(),
        "confirming delete removes the project directory"
    );
    assert!(
        matches!(app.overlay, Overlay::None),
        "overlay closes after delete"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Ctrl-T must open the theme picker even while the Settings overlay is focused
/// (the panel advertises it, and an open overlay otherwise swallows the global).
#[test]
fn ctrl_t_opens_theme_picker_from_settings() {
    use crate::app::overlay::Overlay;

    let mut app = fresh_app();
    app.overlay = Overlay::settings_with_field(&app.cfg.clone(), 0);
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
        stopped: false,
        run: Default::default(),
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
    use crate::model::{
        Chapter, ChapterKind, ChapterStatus, Project, RunHistoryEntry, RunHistoryStatus, Volume,
    };
    use crate::workspace::Workspace;
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
    let ws = Workspace::new(project_dir.clone(), 1);
    crate::workspace::volume::record_run_started(
        &ws,
        RunHistoryEntry::started(
            cp.run_id.clone(),
            cp.started_at,
            cp.chapters.clone(),
            cp.honya_version.clone(),
        ),
    )
    .unwrap();

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
    let history = crate::workspace::volume::load(&ws).run_history;
    assert_eq!(
        history[0].status,
        RunHistoryStatus::Completed,
        "an already-finished recovery closes the run-history row"
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
        client: Some(Arc::new(crate::llm::mock::MockClient::default())
            as Arc<dyn crate::llm::client::LlmClient>),
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
        client: Some(Arc::new(crate::llm::mock::MockClient::default())
            as Arc<dyn crate::llm::client::LlmClient>),
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

    {
        use crate::export::{ExportFormat, export_volume, gather};
        let ws = Workspace::new(project_root.clone(), 1);
        let vol = &project.volumes[0];
        let exbook = gather(
            &ws,
            &project.title,
            &project.id,
            1,
            vol.label.clone(),
            &vol.chapters,
        )
        .await;
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        let (paths, _warnings) = export_volume(
            &ws,
            exbook,
            &[
                ExportFormat::Markdown,
                ExportFormat::Epub,
                ExportFormat::Docx,
            ],
            &EventTx(tx2),
        )
        .await
        .expect("export_volume");

        assert_eq!(paths.len(), 3, "one file per requested format");
        for p in &paths {
            let meta = std::fs::metadata(p).expect("export file exists");
            assert!(meta.len() > 0, "export file non-empty: {}", p.display());
            assert!(
                p.parent().is_some_and(|d| d.ends_with("exports")),
                "export lands under exports/: {}",
                p.display()
            );
        }
        let epub = paths
            .iter()
            .find(|p| p.extension().and_then(|e| e.to_str()) == Some("epub"))
            .expect("epub produced");
        let reimport = base.join("reexport_work");
        let reimported =
            crate::epub::import::import_epub(epub, &reimport).expect("re-import exported epub");
        assert!(!reimported.spine.is_empty(), "exported epub has a spine");
    }

    let _ = std::fs::remove_dir_all(&base);
}

/// A Translator stream that gets cut off mid-JSON (provider truncation / hitting a
/// token ceiling) must NOT sink the whole chapter. The streamed `translated_text`
/// captured before the cutoff is salvaged onto disk flagged `[REVIEW NEEDED]`, so the
/// run completes and a human can finish that one chunk — never `bail!` with nothing.
#[tokio::test]
async fn partial_translator_stream_is_salvaged_as_needs_review() {
    use crate::agents::pipeline::{PipelineCtx, RunControl, run_pipeline};
    use crate::llm::client::{LlmClient, Result as LlmResult};
    use crate::llm::{ChatRequest, ChatResponse, Choice, ResponseFormat, ResponseMessage, Usage};
    use crate::model::AppEvent;
    use crate::workspace::Workspace;
    use async_trait::async_trait;

    const PARTIAL: &str = "สวัสดีนี่คือบทแปลที่ถูกตัดกลางคัน";

    /// Returns a `translation_result` whose JSON is truncated mid-`translated_text`
    /// (no closing quote/brace), mirroring a real stream cutoff. Note the schema-order
    /// violation (`continuity_notes`/`new_characters` first) seen in the wild.
    struct TruncatingTranslatorClient;

    #[async_trait]
    impl LlmClient for TruncatingTranslatorClient {
        async fn chat(&self, req: &ChatRequest) -> LlmResult<ChatResponse> {
            let schema_name = match &req.response_format {
                Some(ResponseFormat::JsonSchema { json_schema }) => Some(json_schema.name.as_str()),
                _ => None,
            };
            let content = match schema_name {
                Some("translation_result") => format!(
                    "{{\"continuity_notes\":[],\"new_characters\":[],\"new_terms\":[],\
                     \"thought_process\":{{\"scene_analysis\":\"a\",\"glossary_check\":\"b\"}},\
                     \"translated_text\":\"{PARTIAL}"
                ),
                Some("review_result") => "{\"status\":\"approve\",\"feedback\":[]}".to_string(),
                _ => "(mock)".to_string(),
            };
            Ok(ChatResponse {
                id: Some("mock".to_string()),
                model: Some("honya/mock".to_string()),
                choices: vec![Choice {
                    index: 0,
                    message: ResponseMessage {
                        role: Some("assistant".to_string()),
                        content: Some(content),
                        tool_calls: None,
                    },
                    // A length-capped provider reports "length"; the JSON is truncated
                    // either way, which is what drives the recovery path.
                    finish_reason: Some("length".to_string()),
                }],
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    cost: 0.001,
                    cost_details: None,
                }),
            })
        }
    }

    let base = std::env::temp_dir().join(format!("honya_partial_salvage_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let project_root = base.join("novel");
    std::fs::create_dir_all(&project_root).unwrap();
    crate::workspace::scaffold::create_project(
        &project_root,
        "Salvage Test",
        &ModelSet::default(),
        1,
    )
    .expect("create_project");
    let ws = Workspace::new(project_root.clone(), 1);
    crate::workspace::translation::write_raw(&ws, 1, "# 第一章\n\nこれはテストの文章です。")
        .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = PipelineCtx {
        client: Arc::new(TruncatingTranslatorClient) as Arc<dyn LlmClient>,
        ws,
        models: ModelSet::default(),
        cfg: AppConfig::default(),
        tx: EventTx(tx),
        ctl: RunControl::new(),
    };
    // Must complete by salvaging, never bail the chapter on a cut-off stream.
    run_pipeline(ctx, vec![1])
        .await
        .expect("run_pipeline salvages the partial instead of bailing");

    let mut needs_review = false;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::ChunkNeedsReview { chapter: 1, .. } = ev {
            needs_review = true;
        }
    }
    assert!(needs_review, "a ChunkNeedsReview event was emitted");

    let translated = std::fs::read_to_string(project_root.join("Vol_01/translated/ch_001.md"))
        .expect("translated file written despite the cutoff");
    assert!(
        translated.contains(PARTIAL),
        "salvaged partial Thai landed on disk: {translated}"
    );
    assert!(
        translated.contains(crate::workspace::translation::REVIEW_NEEDED_MARKER),
        "salvaged chunk is flagged for manual review: {translated}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// In a multi-volume project, chapter numbers collide across volumes (each `Vol_NN`
/// has its own `ch_001`). A pipeline event must update the *running* volume's
/// chapter (recorded in the checkpoint), never another volume's same-numbered one —
/// even when the cursor (active volume) has moved elsewhere mid-run.
#[test]
fn pipeline_events_route_to_the_running_volume() {
    use crate::app::ActiveProject;
    use crate::model::{
        AppEvent, Chapter, ChapterKind, ChapterStatus, ModelSet, Project, UsageStats, Volume,
    };
    use crate::workspace::Workspace;
    use crate::workspace::session::SessionCheckpoint;

    let dir = std::env::temp_dir().join(format!("honya_route_vol_{}", std::process::id()));
    let mk_ch = |n: u32| Chapter {
        number: n,
        title: format!("ch{n}"),
        kind: ChapterKind::Prose,
        status: ChapterStatus::Pending,
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
        volumes: vec![
            Volume {
                number: 1,
                dir: dir.join("Vol_01"),
                label: None,
                chapters: vec![mk_ch(1), mk_ch(2)],
            },
            Volume {
                number: 2,
                dir: dir.join("Vol_02"),
                label: None,
                chapters: vec![mk_ch(1), mk_ch(2)],
            },
        ],
        models: None,
    };
    let mut app = fresh_app();
    app.active = Some(ActiveProject {
        project,
        workspace: Workspace::new(dir.clone(), 1),
        client: None,
        models: ModelSet::default(),
        vol: 1, // cursor/active volume is Vol.01 ...
    });
    // ... but the in-flight run targets Vol.02.
    app.active_run = Some(SessionCheckpoint::new(
        dir.clone(),
        "novel".to_string(),
        "Novel".to_string(),
        2,
        vec![1],
    ));

    app.on_app_event(AppEvent::ChapterCompleted { chapter: 1 });

    let status = |app: &App, vol: u32, ch: u32| {
        app.active
            .as_ref()
            .unwrap()
            .project
            .volumes
            .iter()
            .find(|v| v.number == vol)
            .unwrap()
            .chapters
            .iter()
            .find(|c| c.number == ch)
            .unwrap()
            .status
    };
    assert_eq!(
        status(&app, 2, 1),
        ChapterStatus::Done,
        "the running volume's chapter is updated"
    );
    assert_eq!(
        status(&app, 1, 1),
        ChapterStatus::Pending,
        "the other volume's same-numbered chapter is untouched"
    );
}

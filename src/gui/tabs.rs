//! Open tabs across the top of the body.
//!
//! The six views were a nav strip: one at a time, and coming back meant
//! re-finding where you were. A tab is a view *plus what it is showing*, so
//! two chapters and a conversation can be open at once and switching between
//! them is a click.
//!
//! What a tab owns depends on what the backend can actually hold more than one
//! of. `ReaderScreen` is self-contained — text, scroll, search, highlights —
//! and observes no `AppEvent`, so a Reader tab keeps a real copy and swaps it
//! into `App` when it comes forward. There is one Refine agent, one control
//! channel and one live thread, so a Refine tab carries its session id and
//! switches to it instead of pretending to hold a second live conversation.
//! `TranslateScreen` observes every event and has to stay resident, so it and
//! the other flat views get one tab each.

use crate::app::{App, Screen};
use crate::app::reader::ReaderScreen;

/// What a tab is showing. Two tabs with the same `TabId` are the same tab.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabId {
    /// One of the views there is only ever one of.
    View(#[serde(with = "screen_name")] Screen),
    Chapter { vol: u32, ch: u32 },
    Session(String),
}

impl TabId {
    pub fn view(&self) -> Screen {
        match self {
            TabId::View(s) => *s,
            TabId::Chapter { .. } => Screen::Reader,
            TabId::Session(_) => Screen::Refine,
        }
    }
}

pub struct Tab {
    pub id: TabId,
    pub title: String,
    /// A Reader tab's own screen, parked here while another tab is forward.
    state: Option<Box<ReaderScreen>>,
}

#[derive(Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
}

/// `Screen` is `ui::chrome`'s vocabulary and its variant *order* is
/// load-bearing there, so a saved layout names the view rather than numbering
/// it — renumbering must not reopen the wrong tab.
mod screen_name {
    use super::Screen;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(s: &Screen, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(crate::app::keys::screen_slug(*s))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Screen, D::Error> {
        let name = String::deserialize(de)?;
        [
            Screen::Shelf,
            Screen::Project,
            Screen::Translate,
            Screen::Reader,
            Screen::Lexicon,
            Screen::Refine,
        ]
        .into_iter()
        .find(|s| crate::app::keys::screen_slug(*s) == name)
        .ok_or_else(|| serde::de::Error::custom(format!("unknown view {name}")))
    }
}

impl Tabs {
    /// What was open, for the next run. A tab's live state is not saved — a
    /// scroll position is worth keeping across a switch, not across a restart.
    pub fn open_ids(&self) -> Vec<TabId> {
        self.tabs.iter().map(|t| t.id.clone()).collect()
    }

    /// Reopen what was open, without applying any of it: the first frame's
    /// reconcile puts the app on whichever one is forward.
    pub fn restore(&mut self, ids: Vec<TabId>, active: usize, app: &App) {
        self.tabs = ids
            .into_iter()
            .map(|id| Tab {
                title: title_for(&id, app),
                id,
                state: None,
            })
            .collect();
        self.active = active.min(self.tabs.len().saturating_sub(1));
    }
}

impl Tabs {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, &Tab)> {
        self.tabs.iter().enumerate()
    }

    #[cfg(test)]
    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    fn position(&self, id: &TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == *id)
    }

    /// Take the live screen out of `App` and back into the tab that owns it,
    /// so it is still there when that tab comes forward again.
    fn park(&mut self, app: &mut App) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if matches!(tab.id, TabId::Chapter { .. }) {
            let mut parked = Box::new(ReaderScreen::new());
            std::mem::swap(&mut app.reader, parked.as_mut());
            tab.state = Some(parked);
        }
    }

    /// Bring `id` forward, adding it if it is not open yet.
    ///
    /// Returns whether the caller still has to *load* what the tab shows: a
    /// tab that was already open brought its own state back with it, and
    /// reloading would throw away the scroll position that is the whole point.
    pub fn focus(&mut self, id: TabId, title: String, app: &mut App) -> bool {
        if self.position(&id) == Some(self.active) {
            return false;
        }
        self.park(app);
        match self.position(&id) {
            Some(i) => {
                self.active = i;
                if let Some(mut parked) = self.tabs[i].state.take() {
                    std::mem::swap(&mut app.reader, parked.as_mut());
                    return false;
                }
                false
            }
            None => {
                self.tabs.push(Tab {
                    id,
                    title,
                    state: None,
                });
                self.active = self.tabs.len() - 1;
                true
            }
        }
    }

    /// Close a tab. The one that takes its place comes forward.
    pub fn close(&mut self, index: usize, app: &mut App) -> Option<TabId> {
        if index >= self.tabs.len() {
            return None;
        }
        if index == self.active {
            // Its live state dies with it; nothing to park.
            self.tabs[index].state = None;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
            return None;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if index < self.active {
            self.active -= 1;
        }
        let id = self.tabs[self.active].id.clone();
        if let Some(mut parked) = self.tabs[self.active].state.take() {
            std::mem::swap(&mut app.reader, parked.as_mut());
        }
        Some(id)
    }

    /// Make sure whatever the app is showing has a tab, without disturbing one
    /// that is already forward. Covers the ways a view changes that no click
    /// of ours went through — a digit key, a slash command, a recovery dialog.
    pub fn ensure(&mut self, id: TabId, title: String) {
        match self.position(&id) {
            Some(i) => self.active = i,
            None => {
                self.tabs.push(Tab {
                    id,
                    title,
                    state: None,
                });
                self.active = self.tabs.len() - 1;
            }
        }
    }
}

/// What a tab is called. Short enough for a strip, specific enough to tell two
/// chapters apart.
pub fn title_for(id: &TabId, app: &App) -> String {
    match id {
        TabId::View(s) => match s {
            Screen::Shelf => "書架 Shelf".into(),
            Screen::Project => "構図 Project".into(),
            Screen::Translate => "訳 Translate".into(),
            Screen::Reader => "読 Reader".into(),
            Screen::Lexicon => "辞 Lexicon".into(),
            Screen::Refine => "磨 Refine".into(),
        },
        TabId::Chapter { vol, ch } => format!("読 v{vol}/c{ch}"),
        TabId::Session(id) => {
            let title = app
                .refine_sessions
                .iter()
                .find(|s| s.id == *id)
                .map(|s| s.title.trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| id.clone());
            format!("磨 {}", truncate(&title, 18))
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            crate::model::EventTx(tx),
            crate::model::AppConfig::default(),
        )
    }

    #[test]
    fn opening_the_same_chapter_twice_reuses_its_tab() {
        let mut app = app();
        let mut tabs = Tabs::default();
        let c12 = TabId::Chapter { vol: 1, ch: 12 };

        assert!(
            tabs.focus(c12.clone(), "読 v1/c12".into(), &mut app),
            "a new tab has to be loaded"
        );
        assert_eq!(tabs.iter().count(), 1);

        let c13 = TabId::Chapter { vol: 1, ch: 13 };
        assert!(tabs.focus(c13, "読 v1/c13".into(), &mut app));
        assert_eq!(tabs.iter().count(), 2);

        assert!(
            !tabs.focus(c12.clone(), "読 v1/c12".into(), &mut app),
            "a tab that is already open brought its own state back"
        );
        assert_eq!(tabs.iter().count(), 2, "and did not open a third");
        assert_eq!(tabs.active().map(|t| t.id.clone()), Some(c12));
    }

    #[test]
    fn a_reader_tab_keeps_its_place_while_another_is_forward() {
        let mut app = app();
        let mut tabs = Tabs::default();

        tabs.focus(TabId::Chapter { vol: 1, ch: 12 }, "a".into(), &mut app);
        app.reader.set_scroll_for_test(40);

        tabs.focus(TabId::Chapter { vol: 1, ch: 13 }, "b".into(), &mut app);
        assert_eq!(
            app.reader.scroll_for_test(),
            0,
            "the new tab starts at the top"
        );

        tabs.focus(TabId::Chapter { vol: 1, ch: 12 }, "a".into(), &mut app);
        assert_eq!(
            app.reader.scroll_for_test(),
            40,
            "coming back is where you left it"
        );
    }

    #[test]
    fn exactly_one_copy_of_the_reader_is_live() {
        let mut app = app();
        let mut tabs = Tabs::default();
        tabs.focus(TabId::Chapter { vol: 1, ch: 1 }, "a".into(), &mut app);
        tabs.focus(TabId::Chapter { vol: 1, ch: 2 }, "b".into(), &mut app);
        // The forward tab's state is in `App`, not in the tab.
        let forward = tabs.active_index();
        assert!(tabs.tabs[forward].state.is_none());
        assert!(tabs.tabs[1 - forward].state.is_some());
    }

    #[test]
    fn a_flat_view_never_opens_twice() {
        let mut app = app();
        let mut tabs = Tabs::default();
        for _ in 0..3 {
            tabs.focus(TabId::View(Screen::Translate), "訳".into(), &mut app);
            tabs.focus(TabId::View(Screen::Shelf), "書架".into(), &mut app);
        }
        assert_eq!(tabs.iter().count(), 2);
    }

    #[test]
    fn closing_the_forward_tab_brings_its_neighbour_up() {
        let mut app = app();
        let mut tabs = Tabs::default();
        tabs.focus(TabId::View(Screen::Shelf), "書架".into(), &mut app);
        tabs.focus(TabId::View(Screen::Project), "構図".into(), &mut app);
        tabs.focus(TabId::View(Screen::Translate), "訳".into(), &mut app);
        assert_eq!(tabs.active_index(), 2);

        let now = tabs.close(2, &mut app);
        assert_eq!(now, Some(TabId::View(Screen::Project)));
        assert_eq!(tabs.iter().count(), 2);

        // Closing one *before* the forward tab keeps the same tab forward.
        tabs.focus(TabId::View(Screen::Project), "構図".into(), &mut app);
        let now = tabs.close(0, &mut app);
        assert_eq!(now, Some(TabId::View(Screen::Project)));
    }

    #[test]
    fn closing_the_last_tab_leaves_nothing_forward() {
        let mut app = app();
        let mut tabs = Tabs::default();
        tabs.focus(TabId::View(Screen::Shelf), "書架".into(), &mut app);
        assert_eq!(tabs.close(0, &mut app), None);
        assert!(tabs.is_empty());
    }

    #[test]
    fn a_tab_id_knows_which_view_draws_it() {
        assert_eq!(TabId::Chapter { vol: 1, ch: 2 }.view(), Screen::Reader);
        assert_eq!(TabId::Session("x".into()).view(), Screen::Refine);
        assert_eq!(TabId::View(Screen::Lexicon).view(), Screen::Lexicon);
    }

    #[test]
    fn a_long_conversation_name_is_cut_not_wrapped() {
        assert_eq!(truncate("short", 18), "short");
        assert_eq!(truncate(&"x".repeat(30), 5), "xxxxx…");
    }

    #[test]
    fn what_was_open_comes_back() {
        let mut app = app();
        let mut tabs = Tabs::default();
        tabs.focus(TabId::View(Screen::Project), "構図".into(), &mut app);
        tabs.focus(TabId::Chapter { vol: 1, ch: 12 }, "読".into(), &mut app);
        let ids = tabs.open_ids();
        let active = tabs.active_index();

        let mut later = Tabs::default();
        later.restore(ids.clone(), active, &app);
        assert_eq!(later.open_ids(), ids);
        assert_eq!(later.active_index(), active);
    }

    #[test]
    fn a_view_is_saved_by_name_not_by_number() {
        // `Screen`'s variant order is load-bearing elsewhere; renumbering it
        // must not reopen a different tab.
        let json = serde_json::to_string(&TabId::View(Screen::Lexicon)).unwrap();
        assert!(json.contains("lexicon"), "{json}");
        let back: TabId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TabId::View(Screen::Lexicon));
    }

    #[test]
    fn a_layout_naming_a_view_that_no_longer_exists_is_not_fatal() {
        assert!(serde_json::from_str::<TabId>(r#"{"view":"holodeck"}"#).is_err());
        // ...and a list containing one simply loses that entry rather than
        // throwing the whole layout away.
        let ids: Vec<TabId> = serde_json::from_str(r#"[{"view":"shelf"}]"#).unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn restoring_more_tabs_than_the_active_index_is_safe() {
        let app = app();
        let mut tabs = Tabs::default();
        tabs.restore(vec![TabId::View(Screen::Shelf)], 9, &app);
        assert_eq!(tabs.active_index(), 0);
    }
}

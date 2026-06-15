//! Pure serde wire types shared with the relay backend and web dashboard.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteState {
    #[default]
    Disconnected,
    Pairing,
    Connecting,
    Connected,
    Error,
}

impl RemoteState {
    pub fn label(self) -> &'static str {
        match self {
            RemoteState::Disconnected => "offline",
            RemoteState::Pairing => "pairing",
            RemoteState::Connecting => "connecting",
            RemoteState::Connected => "live",
            RemoteState::Error => "retrying",
        }
    }
}

/// A `(volume, chapter)` identity. Chapter numbers repeat across volumes, so the
/// queue is always keyed by the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterId {
    pub vol: u32,
    pub ch: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TallySnapshot {
    pub done: u32,
    pub working: u32,
    pub pending: u32,
    pub failed: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub prompt: u32,
    pub completion: u32,
    pub total: u32,
    pub cost_usd: f64,
}

impl From<&crate::model::UsageStats> for UsageSnapshot {
    fn from(u: &crate::model::UsageStats) -> Self {
        Self {
            prompt: u.tokens.prompt,
            completion: u.tokens.completion,
            total: u.tokens.total,
            cost_usd: u.cost_usd,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub level: String,
    pub msg: String,
}

/// One chapter in the active project's roster, projected for the dashboard's
/// chapter board. `kind` is `"prose" | "image" | "empty"`; `status` uses the
/// same vocabulary as the `chapter` delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteChapter {
    pub vol: u32,
    pub ch: u32,
    pub title: String,
    pub kind: String,
    pub status: String,
}

/// One project on the shelf, projected so the dashboard can switch the open
/// project remotely. `active` marks the one currently open in the app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteProject {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub title_th: String,
    pub volumes: u32,
    pub chapters: u32,
    pub done: u32,
    pub active: bool,
}

/// One volume of the open project, projected for the volume switcher + recap.
/// `recap` / `synopsis_th` are only populated for the *active* volume (a single
/// disk read); other volumes carry the counts and label only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteVolume {
    pub number: u32,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub synopsis_th: String,
    #[serde(default)]
    pub recap: String,
    pub done: u32,
    pub total: u32,
    pub active: bool,
}

/// A cast member, projected for the dashboard's lexicon view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCharacter {
    pub jp_name: String,
    pub thai_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub romaji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub honorific: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_chapter: Option<u32>,
}

/// A glossary term, projected for the dashboard's lexicon view. `policy` is the
/// effective terminology policy label (`hard_locked` / `preferred` / `forbidden`
/// / `context_dependent`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTerm {
    pub jp_term: String,
    pub thai_term: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub romaji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gloss: Option<String>,
    pub policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_chapter: Option<u32>,
}

/// The open project's cast + glossary, projected together so the dashboard's
/// lexicon view can refresh in one delta.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteLexicon {
    pub characters: Vec<RemoteCharacter>,
    pub glossary: Vec<RemoteTerm>,
}

/// Full state cached by the relay for newly-opened dashboards.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoteSnapshot {
    pub app_version: String,
    pub project: Option<String>,
    pub vol: Option<u32>,
    pub run_active: bool,
    pub paused: bool,
    pub running: Option<ChapterId>,
    pub queue: Vec<ChapterId>,
    pub tally: TallySnapshot,
    pub usage_run: UsageSnapshot,
    pub usage_chapter: UsageSnapshot,
    pub log_tail: Vec<LogLine>,
    /// Full chapter roster across every volume, ascending. Empty when no project
    /// is open. Lets the dashboard render the whole board on first paint instead
    /// of only the chapters it has since seen `chapter` deltas for.
    #[serde(default)]
    pub chapters: Vec<RemoteChapter>,
    /// Every project on the shelf, so the dashboard can switch the open project.
    #[serde(default)]
    pub projects: Vec<RemoteProject>,
    /// The open project's volumes (counts + active-volume recap). Empty when no
    /// project is open.
    #[serde(default)]
    pub volumes: Vec<RemoteVolume>,
    /// The open project's cast + glossary.
    #[serde(default)]
    pub lexicon: RemoteLexicon,
}

/// Incremental update projected from a single [`crate::model::AppEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteDelta {
    Queue {
        running: Option<ChapterId>,
        pending: Vec<ChapterId>,
    },
    Chapter {
        chapter: u32,
        status: String,
    },
    Chunk {
        chapter: u32,
        chunk: usize,
        total: usize,
        state: String,
    },
    Stream {
        chapter: u32,
        chunk: usize,
        role: String,
        delta: String,
    },
    Usage {
        run: UsageSnapshot,
        chapter: UsageSnapshot,
    },
    Tally(TallySnapshot),
    /// The cast/glossary changed (Orchestrator upsert). Carries the full refreshed
    /// lexicon so the dashboard can replace its view wholesale.
    Lexicon(RemoteLexicon),
    Log(LogLine),
    RunFinished {
        done: u32,
        failed: u32,
        need_review: u32,
        stopped: bool,
    },
}

/// Browser command delivered through the relay and mapped onto an app action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RemoteCommand {
    Pause,
    Stop,
    StartProject,
    Enqueue {
        vol: u32,
        chapters: Vec<u32>,
    },
    QueueMoveUp {
        vol: u32,
        ch: u32,
    },
    QueueMoveDown {
        vol: u32,
        ch: u32,
    },
    Dequeue {
        vol: u32,
        ch: u32,
    },
    /// Open a different project from the shelf (by slug/id).
    OpenProject {
        id: String,
    },
    /// Switch the open project's active volume.
    SetVolume {
        vol: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RemoteOutbound {
    // Boxed: the snapshot (full roster + lexicon + projects) dwarfs a delta, and
    // deltas are by far the hotter variant on the channel.
    Snapshot(Box<RemoteSnapshot>),
    Delta(RemoteDelta),
}

impl RemoteOutbound {
    /// Serialize to the on-wire envelope the relay + browser expect.
    pub fn encode(&self) -> String {
        let v = match self {
            RemoteOutbound::Snapshot(s) => {
                serde_json::json!({ "type": "snapshot", "data": s })
            }
            RemoteOutbound::Delta(d) => serde_json::json!({ "type": "delta", "data": d }),
        };
        v.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inbound {
    Status { watchers: u32 },
    Command { data: RemoteCommand },
}

/// Unknown frames decode to `None` for forward-compatible relay messages.
pub fn decode_inbound(text: &str) -> Option<Inbound> {
    serde_json::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips() {
        let snap = RemoteSnapshot {
            app_version: "0.2.6".into(),
            project: Some("Some LN · Vol.01".into()),
            vol: Some(1),
            run_active: true,
            paused: false,
            running: Some(ChapterId { vol: 1, ch: 3 }),
            queue: vec![ChapterId { vol: 1, ch: 4 }, ChapterId { vol: 1, ch: 5 }],
            tally: TallySnapshot {
                done: 2,
                working: 1,
                pending: 2,
                failed: 0,
                total: 5,
            },
            usage_run: UsageSnapshot {
                prompt: 100,
                completion: 50,
                total: 150,
                cost_usd: 0.42,
            },
            usage_chapter: UsageSnapshot::default(),
            log_tail: vec![LogLine {
                level: "info".into(),
                msg: "chapter 2 done".into(),
            }],
            chapters: vec![RemoteChapter {
                vol: 1,
                ch: 3,
                title: "第三章".into(),
                kind: "prose".into(),
                status: "translating".into(),
            }],
            projects: vec![RemoteProject {
                id: "some-ln".into(),
                title: "Some LN".into(),
                title_th: "ไลท์โนเวลบางเล่ม".into(),
                volumes: 2,
                chapters: 20,
                done: 7,
                active: true,
            }],
            volumes: vec![RemoteVolume {
                number: 1,
                label: Some("黎明".into()),
                synopsis_th: "เรื่องย่อ".into(),
                recap: "สรุปจนถึงตอนนี้".into(),
                done: 2,
                total: 5,
                active: true,
            }],
            lexicon: RemoteLexicon {
                characters: vec![RemoteCharacter {
                    jp_name: "鈴".into(),
                    thai_name: "ริน".into(),
                    romaji: Some("Rin".into()),
                    first_seen_chapter: Some(1),
                    ..Default::default()
                }],
                glossary: vec![RemoteTerm {
                    jp_term: "魔法".into(),
                    thai_term: "เวทมนตร์".into(),
                    policy: "preferred".into(),
                    ..Default::default()
                }],
            },
        };
        let out = RemoteOutbound::Snapshot(Box::new(snap.clone()));
        let wire = out.encode();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "snapshot");
        let back: RemoteSnapshot = serde_json::from_value(v["data"].clone()).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn delta_envelope_and_tag() {
        let d = RemoteDelta::Stream {
            chapter: 3,
            chunk: 1,
            role: "translator".into(),
            delta: "สวัสดี".into(),
        };
        let wire = RemoteOutbound::Delta(d.clone()).encode();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "delta");
        assert_eq!(v["data"]["kind"], "stream");
        let back: RemoteDelta = serde_json::from_value(v["data"].clone()).unwrap();
        assert_eq!(back, d);
    }

    /// The lexicon delta is an internally-tagged newtype variant — its struct
    /// fields must flatten alongside `kind` (the web reads `characters`/`glossary`
    /// straight off `data`), not nest under a wrapper key.
    #[test]
    fn lexicon_delta_flattens_alongside_kind() {
        let d = RemoteDelta::Lexicon(RemoteLexicon {
            characters: vec![RemoteCharacter {
                jp_name: "鈴".into(),
                thai_name: "ริน".into(),
                ..Default::default()
            }],
            glossary: vec![RemoteTerm {
                jp_term: "魔法".into(),
                thai_term: "เวทมนตร์".into(),
                policy: "preferred".into(),
                ..Default::default()
            }],
        });
        let wire = RemoteOutbound::Delta(d.clone()).encode();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["data"]["kind"], "lexicon");
        assert_eq!(v["data"]["characters"][0]["thai_name"], "ริน");
        assert_eq!(v["data"]["glossary"][0]["policy"], "preferred");
        let back: RemoteDelta = serde_json::from_value(v["data"].clone()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn inbound_status_and_command_decode() {
        let s = decode_inbound(r#"{"type":"status","watchers":2}"#).unwrap();
        assert_eq!(s, Inbound::Status { watchers: 2 });

        let c = decode_inbound(r#"{"type":"command","data":{"op":"pause"}}"#).unwrap();
        assert_eq!(
            c,
            Inbound::Command {
                data: RemoteCommand::Pause
            }
        );

        let e = decode_inbound(
            r#"{"type":"command","data":{"op":"enqueue","vol":1,"chapters":[4,5]}}"#,
        )
        .unwrap();
        assert_eq!(
            e,
            Inbound::Command {
                data: RemoteCommand::Enqueue {
                    vol: 1,
                    chapters: vec![4, 5]
                }
            }
        );
    }

    #[test]
    fn inbound_open_project_and_set_volume_decode() {
        let o = decode_inbound(r#"{"type":"command","data":{"op":"open_project","id":"re-zero"}}"#)
            .unwrap();
        assert_eq!(
            o,
            Inbound::Command {
                data: RemoteCommand::OpenProject {
                    id: "re-zero".into()
                }
            }
        );
        let v = decode_inbound(r#"{"type":"command","data":{"op":"set_volume","vol":3}}"#).unwrap();
        assert_eq!(
            v,
            Inbound::Command {
                data: RemoteCommand::SetVolume { vol: 3 }
            }
        );
    }

    #[test]
    fn unknown_inbound_is_ignored() {
        assert!(decode_inbound(r#"{"type":"future_thing","x":1}"#).is_none());
        assert!(decode_inbound("not json").is_none());
    }

    #[test]
    fn usage_snapshot_from_stats() {
        let mut u = crate::model::UsageStats::default();
        u.tokens.prompt = 10;
        u.tokens.completion = 5;
        u.tokens.total = 15;
        u.cost_usd = 0.01;
        let snap = UsageSnapshot::from(&u);
        assert_eq!(snap.total, 15);
        assert_eq!(snap.prompt, 10);
        assert!((snap.cost_usd - 0.01).abs() < f64::EPSILON);
    }
}

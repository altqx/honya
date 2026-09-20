//! Editing a glossary term or a character, from the same declarations the TUI
//! form is built from.
//!
//! The GUI could not create or edit either one — `L_NEW` and `L_EDIT` had no
//! equivalent at all, so the whole `lexicon_defs` form was terminal-only and
//! the GUI was a viewer of the lexicon rather than a place to maintain it.
//!
//! Drawn from `lexicon_defs::ORDER` and read and written through
//! `lexicon_defs::{get, set}`, so the two front ends show the same rows in the
//! same order, and a field with no row still cannot be lost: `CARRIED` names
//! the omissions and the draft carries everything else untouched.

use egui::{RichText, Ui};

use crate::app::lexicon_defs::{self as defs, DraftEntry, FieldValue, Kind, LexField};
use crate::model::AltName;
use crate::workspace::Workspace;

use super::theme_map::GuiPalette;

/// An open form. The draft *is* the entry, so committing is a write rather
/// than a reassembly — which is what lets a cleared field actually clear.
pub struct LexiconForm {
    pub draft: DraftEntry,
    seed: DraftEntry,
    pub is_new: bool,
    /// One pending chip per list row, keyed by the row's field.
    buffers: std::collections::HashMap<u8, String>,
    /// Cancel was pressed with something typed; the next press discards.
    confirm_discard: bool,
}

impl LexiconForm {
    pub fn new(draft: DraftEntry, is_new: bool) -> Self {
        Self {
            seed: draft.clone(),
            draft,
            is_new,
            buffers: Default::default(),
            confirm_discard: false,
        }
    }

    pub fn title(&self) -> String {
        format!(
            "{} {}",
            if self.is_new { "New" } else { "Edit" },
            self.draft.kind().title()
        )
    }

    /// Whether anything has been typed, so cancelling can ask first.
    pub fn is_dirty(&self) -> bool {
        defs::ORDER.iter().any(|d| {
            d.entry == self.draft.kind()
                && defs::get(&self.draft, d.field) != defs::get(&self.seed, d.field)
        })
    }

    /// The name a new entry cannot be saved without.
    pub fn missing_key(&self) -> Option<&'static str> {
        let (field, what) = match self.draft.kind() {
            defs::EntryKind::Glossary => (LexField::GJpTerm, "a Japanese term"),
            defs::EntryKind::Character => (LexField::CJpName, "a Japanese name"),
            defs::EntryKind::StyleNote => return None,
        };
        defs::get(&self.draft, field)
            .as_text()
            .trim()
            .is_empty()
            .then_some(what)
    }
}

/// Values already in use for a combo row, gathered from the whole roster
/// rather than the filtered view — the vocabulary is the project's, and in the
/// project's own language.
fn known_values(ws: Option<&Workspace>, field: LexField) -> Vec<String> {
    let Some(ws) = ws else { return Vec::new() };
    let mut out: Vec<String> = match field {
        LexField::CGender => crate::workspace::characters::load(ws)
            .into_iter()
            .filter_map(|c| c.gender)
            .collect(),
        LexField::GCategory => crate::workspace::glossary::load(ws)
            .into_iter()
            .filter_map(|t| t.category)
            .collect(),
        _ => Vec::new(),
    };
    out.retain(|s| !s.trim().is_empty());
    out.sort();
    out.dedup();
    out
}

/// Draw the form. Returns true when the user asked to save.
pub fn show(
    ui: &mut Ui,
    form: &mut LexiconForm,
    ws: Option<&Workspace>,
    pal: &GuiPalette,
) -> Outcome {
    let mut outcome = Outcome::Open;
    let kind = form.draft.kind();

    if let DraftEntry::StyleNote(text) = &mut form.draft {
        ui.label(
            RichText::new("A style note is prose, so it is an editor rather than a form.")
                .color(pal.ink_faint)
                .small(),
        );
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::multiline(text)
                .desired_width(f32::INFINITY)
                .desired_rows(8),
        );
    } else {
        egui::Grid::new("lexicon_form")
            .num_columns(2)
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                for (i, def) in defs::ORDER.iter().enumerate() {
                    if def.entry != kind {
                        continue;
                    }
                    ui.label(RichText::new(def.label).color(pal.ink))
                        .on_hover_text(def.help);
                    row(ui, form, i as u8, def, ws, pal);
                    ui.end_row();
                }
            });
    }

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if super::widgets::primary_button(ui, pal, "Save").clicked() {
            outcome = Outcome::Save;
        }
        // Typed work is not thrown away on one click; an untouched form closes
        // straight off, because there is nothing to ask about.
        if form.confirm_discard {
            if ui.button("Discard changes").clicked() {
                outcome = Outcome::Cancel;
            }
            if ui.button("Keep editing").clicked() {
                form.confirm_discard = false;
            }
        } else if ui.button("Cancel").clicked() {
            if form.is_dirty() {
                form.confirm_discard = true;
            } else {
                outcome = Outcome::Cancel;
            }
        }
        if let Some(what) = form.missing_key() {
            ui.label(
                RichText::new(format!("needs {what}"))
                    .color(pal.status_warn)
                    .small(),
            );
        }
    });
    outcome
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Open,
    Save,
    Cancel,
}

fn row(
    ui: &mut Ui,
    form: &mut LexiconForm,
    index: u8,
    def: &defs::Def,
    ws: Option<&Workspace>,
    pal: &GuiPalette,
) {
    let field = def.field;
    let value = defs::get(&form.draft, field);
    match def.kind {
        Kind::Text | Kind::Numeric => {
            let mut text = value.as_text().to_string();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width(280.0)
                    .hint_text(if def.kind == Kind::Numeric { "—" } else { "" }),
            );
            if resp.changed() {
                // A numeric row keeps "unset" as empty rather than inventing a
                // zero, so anything unparseable is simply not taken.
                if def.kind == Kind::Numeric
                    && !text.trim().is_empty()
                    && text.trim().parse::<u32>().is_err()
                {
                    return;
                }
                defs::set(&mut form.draft, field, &FieldValue::Text(text));
            }
        }
        Kind::Toggle => {
            let mut on = matches!(value, FieldValue::Flag(true));
            if ui.checkbox(&mut on, "").changed() {
                defs::set(&mut form.draft, field, &FieldValue::Flag(on));
            }
        }
        Kind::Select => {
            let current = match value {
                FieldValue::Choice(n) => n.min(defs::POLICIES.len() - 1),
                _ => 0,
            };
            let mut pick = current;
            egui::ComboBox::from_id_salt(("lex_select", index))
                .selected_text(defs::policy_label(defs::POLICIES[current]))
                .width(180.0)
                .show_ui(ui, |ui| {
                    for (n, p) in defs::POLICIES.iter().enumerate() {
                        ui.selectable_value(&mut pick, n, defs::policy_label(*p));
                    }
                });
            if pick != current {
                defs::set(&mut form.draft, field, &FieldValue::Choice(pick));
            }
        }
        Kind::Combo => {
            let mut text = value.as_text().to_string();
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut text).desired_width(180.0));
                if resp.changed() {
                    defs::set(&mut form.draft, field, &FieldValue::Text(text.clone()));
                }
                let known = known_values(ws, field);
                if !known.is_empty() {
                    egui::ComboBox::from_id_salt(("lex_combo", index))
                        .selected_text("")
                        .width(28.0)
                        .show_ui(ui, |ui| {
                            for k in known {
                                if ui.selectable_label(k == text, &k).clicked() {
                                    defs::set(&mut form.draft, field, &FieldValue::Text(k));
                                }
                            }
                        });
                }
            });
        }
        Kind::Chips => {
            let mut items = match value {
                FieldValue::List(v) => v,
                _ => Vec::new(),
            };
            chips(ui, form, index, field, &mut items, pal, |s| s.to_string(), |s| {
                Some(s.to_string())
            });
        }
        Kind::Alts => {
            let alts = match value {
                FieldValue::Alts(v) => v,
                _ => Vec::new(),
            };
            let mut items: Vec<String> = alts.iter().map(format_alt).collect();
            chips(ui, form, index, field, &mut items, pal, |s| s.to_string(), |_| None);
        }
    }
}

fn format_alt(a: &AltName) -> String {
    format!("{}→{}", a.jp, a.translated_name)
}

fn parse_alt(s: &str) -> Option<AltName> {
    let (jp, translated) = s.split_once('→').or_else(|| s.split_once("->"))?;
    let (jp, translated) = (jp.trim(), translated.trim());
    (!jp.is_empty() && !translated.is_empty()).then(|| AltName {
        jp: jp.to_string(),
        translated_name: translated.to_string(),
        by: None,
    })
}

/// A list row: one chip per entry, and a box that becomes the next. Clicking a
/// chip lifts it back into the box, so one gesture serves edit and delete.
#[allow(clippy::too_many_arguments)]
fn chips(
    ui: &mut Ui,
    form: &mut LexiconForm,
    index: u8,
    field: LexField,
    items: &mut Vec<String>,
    pal: &GuiPalette,
    _show: impl Fn(&str) -> String,
    _parse: impl Fn(&str) -> Option<String>,
) {
    let is_alts = matches!(defs::at(index).map(|d| d.kind), Some(Kind::Alts));
    let mut changed = false;
    let mut lifted = None;
    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            for (n, item) in items.iter().enumerate() {
                if ui
                    .small_button(format!("{item}  ✕"))
                    .on_hover_text("lift it back into the box")
                    .clicked()
                {
                    lifted = Some(n);
                }
            }
        });
        let buffer = form.buffers.entry(index).or_default();
        let resp = ui.add(
            egui::TextEdit::singleline(buffer)
                .desired_width(240.0)
                .hint_text(if is_alts { "日本語→target" } else { "add…" }),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let text = buffer.trim().to_string();
            if !text.is_empty() && (!is_alts || parse_alt(&text).is_some()) {
                items.push(text);
                buffer.clear();
                changed = true;
            }
        }
        if is_alts {
            ui.label(
                RichText::new("written as 日本語→target")
                    .color(pal.ink_faint)
                    .small(),
            );
        }
    });

    if let Some(n) = lifted {
        let item = items.remove(n);
        form.buffers.insert(index, item);
        changed = true;
    }
    if changed {
        let value = if is_alts {
            FieldValue::Alts(items.iter().filter_map(|s| parse_alt(s)).collect())
        } else {
            FieldValue::List(std::mem::take(items))
        };
        defs::set(&mut form.draft, field, &value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Character;

    fn character() -> DraftEntry {
        DraftEntry::Character(Box::new(Character {
            id: "char-1".into(),
            jp_name: "高橋陽菜".into(),
            translated_name: "ทาคาฮาชิ ฮินะ".into(),
            ..Default::default()
        }))
    }

    #[test]
    fn a_form_shows_every_row_its_kind_declares_and_no_others() {
        for (draft, kind) in [
            (character(), defs::EntryKind::Character),
            (
                DraftEntry::Glossary(Box::default()),
                defs::EntryKind::Glossary,
            ),
        ] {
            let form = LexiconForm::new(draft, true);
            let rows: Vec<_> = defs::ORDER
                .iter()
                .filter(|d| d.entry == form.draft.kind())
                .collect();
            assert!(!rows.is_empty(), "{kind:?} declares rows");
            assert_eq!(rows.len(), kind.fields().len());
        }
    }

    #[test]
    fn a_new_entry_says_what_it_still_needs() {
        let form = LexiconForm::new(
            DraftEntry::Character(Box::default()),
            true,
        );
        assert_eq!(form.missing_key(), Some("a Japanese name"));

        let named = LexiconForm::new(character(), true);
        assert_eq!(named.missing_key(), None);
    }

    #[test]
    fn a_form_is_clean_until_something_is_typed_into_it() {
        let mut form = LexiconForm::new(character(), false);
        assert!(!form.is_dirty());
        defs::set(
            &mut form.draft,
            LexField::CNotes,
            &FieldValue::Text("hums when nervous".into()),
        );
        assert!(form.is_dirty());
    }

    #[test]
    fn clearing_a_field_is_a_change_the_form_can_see() {
        // The whole reason the draft *is* the entry: a blanked field has to
        // survive to the write, or the merge path puts the old value back.
        let mut form = LexiconForm::new(character(), false);
        defs::set(
            &mut form.draft,
            LexField::CTargetName,
            &FieldValue::Text(String::new()),
        );
        assert!(form.is_dirty());
        assert_eq!(
            defs::get(&form.draft, LexField::CTargetName).as_text(),
            ""
        );
    }

    #[test]
    fn an_alternate_name_round_trips_through_its_written_form() {
        let a = AltName {
            jp: "先輩".into(),
            translated_name: "รุ่นพี่".into(),
            by: None,
        };
        let written = format_alt(&a);
        assert_eq!(written, "先輩→รุ่นพี่");
        let back = parse_alt(&written).expect("it parses back");
        assert_eq!(back.jp, a.jp);
        assert_eq!(back.translated_name, a.translated_name);
        // An ascii arrow is accepted too, because it is what a keyboard types.
        assert!(parse_alt("先輩->รุ่นพี่").is_some());
        // Half of a mapping is not a mapping.
        assert!(parse_alt("先輩").is_none());
        assert!(parse_alt("→x").is_none());
    }

    #[test]
    fn cancelling_a_touched_form_asks_before_throwing_the_typing_away() {
        let mut form = LexiconForm::new(character(), false);
        assert!(!form.confirm_discard);
        // Untouched: nothing to ask about.
        assert!(!form.is_dirty());

        defs::set(
            &mut form.draft,
            LexField::CNotes,
            &FieldValue::Text("hums when nervous".into()),
        );
        assert!(form.is_dirty());
    }
}

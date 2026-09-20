//! The Lexicon's entries, described once.
//!
//! The edit form used to hold every value as a `String` in a positional
//! `Vec<(&str, String)>`, and reassemble the entry by reading `get(0)…get(7)`.
//! Reordering a row silently corrupted saves, and a field with no row was
//! simply dropped on the floor. Here each row is declared once and the form is
//! generated from the declarations, so an entry is edited in place and there is
//! nothing to reassemble.
//!
//! **Declaration order is the on-screen order.** Every index other code needs is
//! derived from [`ORDER`].

use crate::model::{AltName, Character, GlossaryTerm, TermPolicy};

/// One focusable row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexField {
    // --- glossary ---
    GJpTerm,
    GTargetTerm,
    GRomaji,
    GCategory,
    GPolicy,
    GDoNotTranslate,
    GForbidden,
    GContextRule,
    GGloss,
    GFirstSeen,
    // --- character ---
    CJpName,
    CTargetName,
    CRomaji,
    CAliases,
    CAlsoCalled,
    CGender,
    CHonorific,
    CSpeechStyle,
    CNotes,
    CFirstSeen,
}

/// Which kind of entry a row belongs to. Replaces the `SUB_*` discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Glossary,
    Character,
    /// Prose, so it is an editor rather than a form and declares no rows.
    StyleNote,
}

impl EntryKind {
    pub fn title(self) -> &'static str {
        match self {
            EntryKind::Glossary => "glossary term",
            EntryKind::Character => "character",
            EntryKind::StyleNote => "style note",
        }
    }

    /// Rows belonging to this kind, as indices into [`ORDER`].
    pub fn fields(self) -> Vec<u8> {
        ORDER
            .iter()
            .enumerate()
            .filter(|(_, d)| d.entry == self)
            .map(|(i, _)| i as u8)
            .collect()
    }

    /// The first row of this kind. `None` for a kind that declares none.
    pub fn first_field(self) -> Option<u8> {
        ORDER.iter().position(|d| d.entry == self).map(|i| i as u8)
    }

}

/// How a row is edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// Digits only, and empty means unset — which `form::Kind::Number` cannot
    /// say, since it would have to pick a number to mean "no chapter".
    Numeric,
    /// Cycles what the project already uses; anything else is typed.
    Combo,
    /// A fixed enum.
    Select,
    Toggle,
    /// A list of plain strings.
    Chips,
    /// A list of [`AltName`], shown through `format_alt_name`.
    Alts,
}

/// One declared row.
#[derive(Debug, Clone, Copy)]
pub struct Def {
    pub field: LexField,
    pub label: &'static str,
    pub help: &'static str,
    pub entry: EntryKind,
    pub kind: Kind,
}

const fn def(
    field: LexField,
    label: &'static str,
    entry: EntryKind,
    kind: Kind,
    help: &'static str,
) -> Def {
    Def {
        field,
        label,
        help,
        entry,
        kind,
    }
}

/// Every row, in the order it appears on screen.
pub const ORDER: &[Def] = &[
    // --- Glossary.
    def(LexField::GJpTerm, "JP term", EntryKind::Glossary, Kind::Text,
        "The Japanese surface this entry controls. It is the key."),
    def(LexField::GTargetTerm, "Target term", EntryKind::Glossary, Kind::Text,
        "The rendering the translator should use."),
    def(LexField::GRomaji, "Romaji", EntryKind::Glossary, Kind::Text,
        "Optional reading, for terms whose pronunciation is not obvious."),
    def(LexField::GCategory, "Category", EntryKind::Glossary, Kind::Combo,
        "A loose grouping — place, item, title. Cycles what this project already uses."),
    def(LexField::GPolicy, "Policy", EntryKind::Glossary, Kind::Select,
        "How hard the rendering is held: locked, preferred, forbidden, or context-dependent."),
    def(LexField::GDoNotTranslate, "Do not translate", EntryKind::Glossary, Kind::Toggle,
        "Leave the Japanese as it stands rather than rendering it."),
    def(LexField::GForbidden, "Forbidden", EntryKind::Glossary, Kind::Chips,
        "Renderings that must not appear for this term."),
    def(LexField::GContextRule, "Context rule", EntryKind::Glossary, Kind::Text,
        "When the rendering depends on context, what decides it."),
    def(LexField::GGloss, "Gloss", EntryKind::Glossary, Kind::Text,
        "A note for the translator about what the term means."),
    def(LexField::GFirstSeen, "First seen", EntryKind::Glossary, Kind::Numeric,
        "Chapter this first appeared in. Empty when unknown."),

    // --- Character.
    def(LexField::CJpName, "JP name", EntryKind::Character, Kind::Text,
        "The fullest known Japanese name. It seeds the id for a new character."),
    def(LexField::CTargetName, "Target name", EntryKind::Character, Kind::Text,
        "The canonical rendering of this character's name."),
    def(LexField::CRomaji, "Romaji", EntryKind::Character, Kind::Text,
        "Optional reading of the Japanese name."),
    def(LexField::CAliases, "Aliases", EntryKind::Character, Kind::Chips,
        "Other Japanese surfaces that mean this same character."),
    def(LexField::CAlsoCalled, "Also called", EntryKind::Character, Kind::Alts,
        "Address forms, each with its own rendering and who uses it."),
    def(LexField::CGender, "Gender", EntryKind::Character, Kind::Combo,
        "Cycles what this project already uses; type anything else."),
    def(LexField::CHonorific, "Honorific", EntryKind::Character, Kind::Text,
        "The suffix this character is usually addressed with."),
    def(LexField::CSpeechStyle, "Speech style", EntryKind::Character, Kind::Text,
        "How they speak — register, self-pronoun, verbal tics."),
    def(LexField::CNotes, "Notes", EntryKind::Character, Kind::Text,
        "Anything else the translator should know."),
    def(LexField::CFirstSeen, "First seen", EntryKind::Character, Kind::Numeric,
        "Chapter this character first appeared in. Empty when unknown."),
];

/// Model fields deliberately given no row, so that an omission has to be a
/// decision. Both are carried through an edit untouched.
///
/// `relationships` is a graph of `target_id` references with no sane
/// single-entry form. `protected` is not independent: it follows `policy`,
/// because it is what stops an automatic Orchestrator upsert rewriting a term a
/// human controls. `id` is the key, seeded from the JP name for a new entry.
#[cfg(test)]
pub const CARRIED: &[&str] = &["relationships", "protected", "id"];

/// The policy options, in cycle order.
pub const POLICIES: [TermPolicy; 4] = [
    TermPolicy::Preferred,
    TermPolicy::HardLocked,
    TermPolicy::Forbidden,
    TermPolicy::ContextDependent,
];

pub fn policy_label(p: TermPolicy) -> &'static str {
    match p {
        TermPolicy::Preferred => "preferred",
        TermPolicy::HardLocked => "hard locked",
        TermPolicy::Forbidden => "forbidden",
        TermPolicy::ContextDependent => "context dependent",
    }
}

pub fn at(field: u8) -> Option<&'static Def> {
    ORDER.get(field as usize)
}

#[cfg(test)]
pub fn index_of(f: LexField) -> u8 {
    ORDER.iter().position(|d| d.field == f).unwrap_or(0) as u8
}

/// The entry being edited. The seed is cloned whole and written back in place,
/// which is why a field with no row cannot be lost.
#[derive(Debug, Clone)]
pub enum DraftEntry {
    Glossary(Box<GlossaryTerm>),
    Character(Box<Character>),
    StyleNote(String),
}

impl DraftEntry {
    pub fn kind(&self) -> EntryKind {
        match self {
            DraftEntry::Glossary(_) => EntryKind::Glossary,
            DraftEntry::Character(_) => EntryKind::Character,
            DraftEntry::StyleNote(_) => EntryKind::StyleNote,
        }
    }
}

/// One row's value, in whichever shape that row holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Text(String),
    Flag(bool),
    Choice(usize),
    List(Vec<String>),
    Alts(Vec<AltName>),
}

impl FieldValue {
    pub fn as_text(&self) -> &str {
        match self {
            FieldValue::Text(s) => s,
            _ => "",
        }
    }
}

fn opt_text(v: &Option<String>) -> FieldValue {
    FieldValue::Text(v.clone().unwrap_or_default())
}

fn opt_num(v: &Option<u32>) -> FieldValue {
    FieldValue::Text(v.map(|n| n.to_string()).unwrap_or_default())
}

fn set_opt(slot: &mut Option<String>, v: &FieldValue) {
    // Stored as typed: trimming here would eat a trailing space mid-word and
    // leave the caret past the end. Blank still means unset.
    let text = v.as_text();
    *slot = (!text.trim().is_empty()).then(|| text.to_string());
}

fn set_num(slot: &mut Option<u32>, v: &FieldValue) {
    *slot = v.as_text().trim().parse::<u32>().ok();
}

/// Read one row out of the entry.
pub fn get(entry: &DraftEntry, field: LexField) -> FieldValue {
    use LexField as F;
    match (entry, field) {
        (DraftEntry::Glossary(g), f) => match f {
            F::GJpTerm => FieldValue::Text(g.jp_term.clone()),
            F::GTargetTerm => FieldValue::Text(g.translated_term.clone()),
            F::GRomaji => opt_text(&g.romaji),
            F::GCategory => opt_text(&g.category),
            F::GPolicy => FieldValue::Choice(
                POLICIES
                    .iter()
                    .position(|p| Some(*p) == g.policy)
                    .unwrap_or(0),
            ),
            F::GDoNotTranslate => FieldValue::Flag(g.do_not_translate.unwrap_or(false)),
            F::GForbidden => FieldValue::List(g.forbidden_translations.clone()),
            F::GContextRule => opt_text(&g.context_rule),
            F::GGloss => opt_text(&g.gloss),
            F::GFirstSeen => opt_num(&g.first_seen_chapter),
            _ => FieldValue::Text(String::new()),
        },
        (DraftEntry::Character(c), f) => match f {
            F::CJpName => FieldValue::Text(c.jp_name.clone()),
            F::CTargetName => FieldValue::Text(c.translated_name.clone()),
            F::CRomaji => opt_text(&c.romaji),
            F::CAliases => FieldValue::List(c.aliases.clone()),
            F::CAlsoCalled => FieldValue::Alts(c.also_called.clone()),
            F::CGender => opt_text(&c.gender),
            F::CHonorific => opt_text(&c.honorific),
            F::CSpeechStyle => opt_text(&c.speech_style),
            F::CNotes => opt_text(&c.notes),
            F::CFirstSeen => opt_num(&c.first_seen_chapter),
            _ => FieldValue::Text(String::new()),
        },
        (DraftEntry::StyleNote(_), _) => FieldValue::Text(String::new()),
    }
}

/// Write one row back into the entry.
pub fn set(entry: &mut DraftEntry, field: LexField, value: &FieldValue) {
    use LexField as F;
    match (entry, field) {
        (DraftEntry::Glossary(g), f) => match f {
            F::GJpTerm => g.jp_term = value.as_text().to_string(),
            F::GTargetTerm => g.translated_term = value.as_text().to_string(),
            F::GRomaji => set_opt(&mut g.romaji, value),
            F::GCategory => set_opt(&mut g.category, value),
            F::GPolicy => {
                if let FieldValue::Choice(i) = value {
                    let policy = POLICIES[*i % POLICIES.len()];
                    g.policy = Some(policy);
                    // `protected` has no row of its own: it is what stops an
                    // automatic upsert rewriting a term a human controls, so it
                    // follows the policy that made the term controlled.
                    g.protected = matches!(
                        policy,
                        TermPolicy::HardLocked
                            | TermPolicy::Forbidden
                            | TermPolicy::ContextDependent
                    )
                    .then_some(true);
                }
            }
            F::GDoNotTranslate => {
                if let FieldValue::Flag(on) = value {
                    g.do_not_translate = Some(*on);
                }
            }
            F::GForbidden => {
                if let FieldValue::List(items) = value {
                    g.forbidden_translations = items.clone();
                }
            }
            F::GContextRule => set_opt(&mut g.context_rule, value),
            F::GGloss => set_opt(&mut g.gloss, value),
            F::GFirstSeen => set_num(&mut g.first_seen_chapter, value),
            _ => {}
        },
        (DraftEntry::Character(c), f) => match f {
            F::CJpName => c.jp_name = value.as_text().to_string(),
            F::CTargetName => c.translated_name = value.as_text().to_string(),
            F::CRomaji => set_opt(&mut c.romaji, value),
            F::CAliases => {
                if let FieldValue::List(items) = value {
                    c.aliases = items.clone();
                }
            }
            F::CAlsoCalled => {
                if let FieldValue::Alts(alts) = value {
                    c.also_called = alts.clone();
                }
            }
            F::CGender => set_opt(&mut c.gender, value),
            F::CHonorific => set_opt(&mut c.honorific, value),
            F::CSpeechStyle => set_opt(&mut c.speech_style, value),
            F::CNotes => set_opt(&mut c.notes, value),
            F::CFirstSeen => set_num(&mut c.first_seen_chapter, value),
            _ => {}
        },
        (DraftEntry::StyleNote(_), _) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: Kind) -> FieldValue {
        match kind {
            Kind::Text | Kind::Combo => FieldValue::Text("a value".into()),
            Kind::Numeric => FieldValue::Text("7".into()),
            Kind::Select => FieldValue::Choice(2),
            Kind::Toggle => FieldValue::Flag(true),
            Kind::Chips => FieldValue::List(vec!["one".into(), "two".into()]),
            Kind::Alts => FieldValue::Alts(vec![AltName {
                jp: "ここあ".into(),
                translated_name: "โคโคอะ".into(),
                by: Some("しらい".into()),
            }]),
        }
    }

    fn draft(entry: EntryKind) -> DraftEntry {
        match entry {
            EntryKind::Glossary => DraftEntry::Glossary(Box::default()),
            EntryKind::Character => DraftEntry::Character(Box::default()),
            EntryKind::StyleNote => DraftEntry::StyleNote(String::new()),
        }
    }

    /// The property the positional `get(0)…get(7)` never had: a row is read
    /// back from where it was written, whatever order the rows are declared in.
    #[test]
    fn a_field_is_read_back_from_where_it_was_written() {
        for d in ORDER {
            let mut entry = draft(d.entry);
            let value = sample(d.kind);
            set(&mut entry, d.field, &value);
            assert_eq!(
                get(&entry, d.field),
                value,
                "{} did not round-trip",
                d.label
            );
        }
    }

    /// Writing one row must not disturb another — the failure mode of reading
    /// an entry back out of a flat vector by index.
    #[test]
    fn writing_one_row_leaves_its_neighbours_alone() {
        for entry_kind in [EntryKind::Glossary, EntryKind::Character] {
            let rows: Vec<&Def> = ORDER.iter().filter(|d| d.entry == entry_kind).collect();
            let mut entry = draft(entry_kind);
            for d in &rows {
                set(&mut entry, d.field, &sample(d.kind));
            }
            for d in &rows {
                assert_eq!(
                    get(&entry, d.field),
                    sample(d.kind),
                    "{} was disturbed by a later write",
                    d.label
                );
            }
        }
    }

    /// `form::Opts { id_base }` is the kind's first index, so a kind's rows have
    /// to be contiguous or a zone id decodes to the wrong row.
    #[test]
    fn each_kinds_rows_are_contiguous() {
        for entry in [EntryKind::Glossary, EntryKind::Character] {
            let fields = entry.fields();
            assert!(!fields.is_empty());
            let first = entry.first_field().unwrap();
            for (n, f) in fields.iter().enumerate() {
                assert_eq!(*f, first + n as u8, "{entry:?} rows are not contiguous");
            }
        }
        assert_eq!(EntryKind::StyleNote.first_field(), None, "prose has no rows");
    }

    #[test]
    fn a_row_is_declared_once_and_index_of_agrees() {
        for (i, d) in ORDER.iter().enumerate() {
            assert_eq!(index_of(d.field), i as u8, "{} is declared twice", d.label);
        }
    }

    #[test]
    fn nothing_is_unlabelled_or_unexplained() {
        for d in ORDER {
            assert!(!d.label.trim().is_empty());
            assert!(
                d.help.ends_with('.'),
                "{}'s help should read as a sentence: {:?}",
                d.label,
                d.help
            );
        }
    }

    /// Every model field is either a row or declared as carried. A field that is
    /// neither is one the form would silently drop, which is how the old
    /// `to_character` lost `honorific` and `speech_style`.
    ///
    /// The counts are written out because there is no reflection; the point is
    /// that adding a model field without a row now fails here.
    #[test]
    fn every_model_field_is_a_row_or_declared_carried() {
        // GlossaryTerm: 11 fields — 10 rows, `protected` carried.
        assert_eq!(EntryKind::Glossary.fields().len(), 10);
        assert!(CARRIED.contains(&"protected"));

        // Character: 12 fields — 10 rows, `relationships` and `id` carried.
        assert_eq!(EntryKind::Character.fields().len(), 10);
        assert!(CARRIED.contains(&"relationships"));
        assert!(CARRIED.contains(&"id"));

        assert_eq!(ORDER.len(), 20, "every row belongs to one of the two kinds");
    }

    /// `protected` has no row: it follows the policy, because it is what stops
    /// an automatic upsert rewriting a term a human controls.
    #[test]
    fn a_controlling_policy_sets_protected() {
        let mut entry = DraftEntry::Glossary(Box::default());
        let locked = POLICIES.iter().position(|p| *p == TermPolicy::HardLocked).unwrap();
        set(&mut entry, LexField::GPolicy, &FieldValue::Choice(locked));
        let DraftEntry::Glossary(g) = &entry else { unreachable!() };
        assert_eq!(g.protected, Some(true));

        let preferred = POLICIES.iter().position(|p| *p == TermPolicy::Preferred).unwrap();
        set(&mut entry, LexField::GPolicy, &FieldValue::Choice(preferred));
        let DraftEntry::Glossary(g) = &entry else { unreachable!() };
        assert_eq!(g.protected, None, "a preferred term is not a human control");
    }
}

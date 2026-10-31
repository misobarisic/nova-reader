//! Device-local source shortcuts. Preferences are normal durable metadata;
//! rebuilding the visible source list never changes or prunes stored choices.
use nova_reader_ui::backend_text as text;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const STORAGE_KEY: &str = "reader:source-browser";
const MAX_PINS: usize = 1000;
const MAX_ID_BYTES: usize = 256;
const MAX_STATE_BYTES: usize = 1024 * 1024;

pub struct SourceChoice {
    pub id: String,
    pub name: String,
    pub language: String,
    pub enabled: bool,
    pub latest: bool,
    pub index: i32,
}

#[derive(Clone, Default)]
pub struct SourceBrowser {
    pinned: BTreeSet<String>,
    last_used: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct StoredPreferences {
    pinned: Vec<String>,
    last_used: Option<String>,
}

impl SourceBrowser {
    pub fn load() -> Self {
        let stored = nova_storage::get_str(STORAGE_KEY);
        Self::from_json(stored.as_deref())
    }

    fn from_json(stored: Option<&str>) -> Self {
        let Some(stored) = stored.filter(|value| value.len() <= MAX_STATE_BYTES) else {
            return Self::default();
        };
        let Ok(preferences) = serde_json::from_str::<StoredPreferences>(stored) else {
            return Self::default();
        };
        if preferences.pinned.len() > MAX_PINS
            || preferences.pinned.iter().any(|id| !valid_id(id))
            || preferences
                .last_used
                .as_deref()
                .is_some_and(|id| !valid_id(id))
        {
            return Self::default();
        }
        // No writes on recovery: a broken value survives until a valid edit.
        Self {
            pinned: preferences.pinned.into_iter().collect(),
            last_used: preferences.last_used,
        }
    }

    pub fn toggle_pin(&mut self, id: &str) -> Result<(), String> {
        self.toggle_pin_with(id, persist)
    }

    fn toggle_pin_with(
        &mut self,
        id: &str,
        write: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        if !valid_id(id) {
            return Err(text::tr("Invalid source identity"));
        }
        let mut next = self.clone();
        if !next.pinned.remove(id) {
            if next.pinned.len() >= MAX_PINS {
                return Err(text::tr("You can pin up to 1,000 sources."));
            }
            next.pinned.insert(id.to_owned());
        }
        self.commit(next, write)
    }

    pub fn used(&mut self, id: &str) -> Result<(), String> {
        self.used_with(id, persist)
    }

    fn used_with(
        &mut self,
        id: &str,
        write: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        if !valid_id(id) {
            return Err(text::tr("Invalid source identity"));
        }
        if self.last_used.as_deref() == Some(id) {
            return Ok(());
        }
        let mut next = self.clone();
        next.last_used = Some(id.to_owned());
        self.commit(next, write)
    }

    fn commit(
        &mut self,
        next: Self,
        write: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        let stored = serde_json::to_string(&StoredPreferences {
            pinned: next.pinned.iter().cloned().collect(),
            last_used: next.last_used.clone(),
        })
        .map_err(|error| error.to_string())?;
        write(&stored)?;
        // A failed database transaction must also leave the painted state alone.
        *self = next;
        Ok(())
    }

    pub fn rows(&self, choices: &[SourceChoice]) -> Vec<crate::SourceRow> {
        let mut unique = BTreeMap::<&str, &SourceChoice>::new();
        for choice in choices.iter().filter(|choice| valid_id(&choice.id)) {
            let entry = unique.entry(&choice.id).or_insert(choice);
            // Capability responses may repeat an identity. Prefer the usable
            // variant, then a deterministic caption/index regardless of order.
            if canonical_key(choice) < canonical_key(entry) {
                *entry = choice;
            }
        }
        let mut grouped = unique
            .into_values()
            .map(|choice| {
                let (rank, group) = if self.last_used.as_deref() == Some(&choice.id) {
                    (0, text::tr("Last used"))
                } else if self.pinned.contains(&choice.id) {
                    (1, text::tr("Pinned"))
                } else {
                    (2, language_name(&choice.language))
                };
                (rank, group, choice)
            })
            .collect::<Vec<_>>();
        grouped.sort_by_cached_key(|(rank, group, choice)| {
            (
                *rank,
                group.to_lowercase(),
                choice.name.to_lowercase(),
                choice.id.clone(),
            )
        });
        grouped
            .into_iter()
            .map(|(_, group, choice)| crate::SourceRow {
                id: choice.id.as_str().into(),
                name: if choice.name.trim().is_empty() {
                    choice.id.as_str()
                } else {
                    choice.name.as_str()
                }
                .into(),
                language: language_name(&choice.language).into(),
                group: group.into(),
                pinned: self.pinned.contains(&choice.id),
                enabled: choice.enabled,
                latest: choice.latest,
                index: choice.index,
            })
            .collect()
    }
}

fn persist(stored: &str) -> Result<(), String> {
    nova_storage::try_set_str(STORAGE_KEY, stored).map_err(|error| error.to_string())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && !id
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn canonical_key(choice: &SourceChoice) -> (bool, bool, String, &str, &str, i32) {
    (
        !choice.enabled,
        !choice.latest,
        choice.name.to_lowercase(),
        &choice.name,
        &choice.language,
        choice.index,
    )
}

fn language_name(language: &str) -> String {
    let language = language.trim();
    let normalized = language.to_lowercase().replace('_', "-");
    let name = match normalized.as_str() {
        "all" | "any" | "*" => "All languages",
        "multi" | "multiple" => "Multi",
        "zh-hans" | "zh-cn" => "Chinese (Simplified)",
        "zh-hant" | "zh-tw" => "Chinese (Traditional)",
        "pt-br" => "Portuguese (Brazil)",
        "es-419" => "Spanish (Latin America)",
        _ => match normalized.split('-').next().unwrap_or_default() {
            "en" => "English",
            "ko" => "Korean",
            "ja" => "Japanese",
            "zh" => "Chinese",
            "es" => "Spanish",
            "pt" => "Portuguese",
            "fr" => "French",
            "de" => "German",
            "ru" => "Russian",
            "ar" => "Arabic",
            "id" => "Indonesian",
            "vi" => "Vietnamese",
            "th" => "Thai",
            "it" => "Italian",
            "tr" => "Turkish",
            "pl" => "Polish",
            "uk" => "Ukrainian",
            "nl" => "Dutch",
            "cs" => "Czech",
            "hu" => "Hungarian",
            "ro" => "Romanian",
            "bn" => "Bengali",
            "hi" => "Hindi",
            "fa" => "Persian",
            "he" => "Hebrew",
            "ms" => "Malay",
            "tl" => "Tagalog",
            "fil" => "Filipino",
            "" => "Unknown language",
            _ => return language.to_owned(),
        },
    };
    text::tr(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn choice(id: &str, name: &str, language: &str, index: i32) -> SourceChoice {
        SourceChoice {
            id: id.into(),
            name: name.into(),
            language: language.into(),
            enabled: true,
            latest: true,
            index,
        }
    }

    fn identities(rows: &[crate::SourceRow]) -> Vec<String> {
        rows.iter().map(|row| row.id.to_string()).collect()
    }

    #[test]
    fn last_used_pinned_and_languages_form_one_ordered_list() {
        let mut browser = SourceBrowser::default();
        browser.toggle_pin_with("beta.en", |_| Ok(())).unwrap();
        browser.toggle_pin_with("alpha.ko", |_| Ok(())).unwrap();
        browser.used_with("alpha.ko", |_| Ok(())).unwrap();
        let rows = browser.rows(&[
            choice("zulu.ko", "Zulu", "ko", 0),
            choice("alpha.ko", "Alpha", "ko", 1),
            choice("beta.en", "Beta", "en", 2),
            choice("zulu.en", "Zulu", "en", 3),
            choice("alpha.en", "Alpha", "en", 4),
        ]);
        assert_eq!(
            identities(&rows),
            ["alpha.ko", "beta.en", "alpha.en", "zulu.en", "zulu.ko"]
        );
        assert_eq!(rows[0].group, "Last used");
        assert!(rows[0].pinned);
        assert_eq!(rows[1].group, "Pinned");
        assert_eq!(rows[2].group, "English");
        assert_eq!(rows[4].group, "Korean");
    }

    #[test]
    fn unpinning_moves_a_source_back_and_removed_sources_create_no_orphans() {
        let mut browser = SourceBrowser::default();
        browser.toggle_pin_with("saved.en", |_| Ok(())).unwrap();
        browser.used_with("removed.ko", |_| Ok(())).unwrap();
        let choices = [choice("saved.en", "Saved", "en", 0)];
        assert_eq!(browser.rows(&choices)[0].group, "Pinned");
        assert!(browser.rows(&[]).is_empty());
        browser.toggle_pin_with("saved.en", |_| Ok(())).unwrap();
        let rows = browser.rows(&choices);
        assert_eq!(rows[0].group, "English");
        assert!(!rows[0].pinned);
        // A later reinstall can still recover its last-used shortcut.
        let restored = browser.rows(&[choice("removed.ko", "Returned", "ko", 1)]);
        assert_eq!(restored[0].group, "Last used");
    }

    #[test]
    fn duplicate_identities_collapse_but_language_variants_remain_distinct() {
        let mut duplicate = choice("addon.en", "Example", "en", 7);
        duplicate.enabled = false;
        let choices = [
            duplicate,
            choice("addon.ko", "Example", "ko", 2),
            choice("addon.en", "Example", "en", 1),
        ];
        let rows = SourceBrowser::default().rows(&choices);
        assert_eq!(identities(&rows), ["addon.en", "addon.ko"]);
        assert!(rows[0].enabled);
        assert_eq!(rows[0].index, 1);
        let reversed = choices.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(
            identities(&rows),
            identities(&SourceBrowser::default().rows(&reversed))
        );
    }

    #[test]
    fn corrupt_preferences_recover_in_memory_and_only_valid_edits_write() {
        for stored in [
            "broken JSON".to_owned(),
            json!({"pinned":[""]}).to_string(),
            json!({"last_used":"x".repeat(MAX_ID_BYTES+1)}).to_string(),
            json!({"pinned":vec!["same";MAX_PINS+1]}).to_string(),
        ] {
            let mut browser = SourceBrowser::from_json(Some(&stored));
            assert!(browser.pinned.is_empty());
            assert!(browser.last_used.is_none());
            assert!(
                browser
                    .toggle_pin_with("", |_| panic!("invalid edit wrote state"))
                    .is_err()
            );
            browser
                .toggle_pin_with("valid.en", |written| {
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(written).unwrap(),
                        json!({"pinned":["valid.en"],"last_used":null})
                    );
                    Ok(())
                })
                .unwrap();
        }
    }

    #[test]
    fn failed_persistence_preserves_state_and_pin_bounds() {
        let mut browser = SourceBrowser::default();
        assert!(
            browser
                .toggle_pin_with("valid.en", |_| Err("disk failure".into()))
                .is_err()
        );
        assert!(browser.pinned.is_empty());
        assert!(
            browser
                .used_with("valid.en", |_| Err("disk failure".into()))
                .is_err()
        );
        assert!(browser.last_used.is_none());
        browser.pinned = (0..MAX_PINS)
            .map(|index| format!("source{index}"))
            .collect();
        assert!(
            browser
                .toggle_pin_with("overflow", |_| panic!("overflow wrote state"))
                .is_err()
        );
        browser.toggle_pin_with("source0", |_| Ok(())).unwrap();
        browser.toggle_pin_with("new-source", |_| Ok(())).unwrap();
        assert_eq!(browser.pinned.len(), MAX_PINS);
    }

    #[test]
    fn language_labels_cover_all_multi_and_locale_variants() {
        assert_eq!(language_name("all"), "All languages");
        assert_eq!(language_name("multi"), "Multi");
        assert_eq!(language_name("en_US"), "English");
        assert_eq!(language_name("pt-BR"), "Portuguese (Brazil)");
        assert_eq!(language_name("Klingon"), "Klingon");
    }
}

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
    /// The package is marked adult (shown only with adult sources on).
    pub adult: bool,
    /// The extension (package) that provides the source.
    pub extension: String,
}

#[derive(Clone, Default)]
pub struct SourceBrowser {
    pinned: BTreeSet<String>,
    last_used: Option<String>,
    /// When `last_used` was opened (Unix seconds); unknown for older data.
    last_used_at: Option<i64>,
    /// The All sources language chip, by display name; `None` is All.
    language: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct StoredPreferences {
    pinned: Vec<String>,
    last_used: Option<String>,
    // Optional so preferences saved before these existed still load, and
    // omitted when unset so those keep their stored form.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_used_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

/// Browse's source list, split into its sections.
pub struct Sections {
    /// The last opened source and when, if it is still available.
    pub last_used: Option<(crate::SourceRow, Option<i64>)>,
    /// Pinned sources, in stored order.
    pub pinned: Vec<crate::SourceRow>,
    /// Every visible source under the selected language chip, by name.
    pub all: Vec<crate::SourceRow>,
    /// Language chips after All, most sources first, with their counts.
    pub languages: Vec<(String, usize)>,
    /// Sources under All (chip 0).
    pub total: usize,
    /// The selected chip: 0 All, then `languages`.
    pub language_index: usize,
}

impl SourceBrowser {
    pub fn load() -> Self {
        let stored = reader_domain::profile::kv::get_str(STORAGE_KEY);
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
            last_used_at: preferences.last_used_at,
            language: preferences
                .language
                .filter(|name| !name.is_empty() && name.len() <= MAX_ID_BYTES),
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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        self.used_at(id, now, persist)
    }

    #[cfg(test)]
    fn used_with(
        &mut self,
        id: &str,
        write: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        self.used_at(id, 0, write)
    }

    fn used_at(
        &mut self,
        id: &str,
        at: i64,
        write: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        if !valid_id(id) {
            return Err(text::tr("Invalid source identity"));
        }
        // Opening the same source again refreshes only its time.
        if self.last_used.as_deref() == Some(id) && (at == 0 || self.last_used_at == Some(at)) {
            return Ok(());
        }
        let mut next = self.clone();
        next.last_used = Some(id.to_owned());
        next.last_used_at = (at > 0).then_some(at);
        self.commit(next, write)
    }

    /// Pick the All sources language chip (`None` for All). Device-only.
    pub fn set_language(&mut self, language: Option<String>) -> Result<(), String> {
        if self.language == language {
            return Ok(());
        }
        let mut next = self.clone();
        next.language = language;
        self.commit(next, persist)
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.pinned.contains(id)
    }

    fn commit(
        &mut self,
        next: Self,
        write: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        let stored = serde_json::to_string(&StoredPreferences {
            pinned: next.pinned.iter().cloned().collect(),
            last_used: next.last_used.clone(),
            last_used_at: next.last_used_at,
            language: next.language.clone(),
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
                icon: Default::default(),
                code: language_code(&choice.language).into(),
                extension: choice.extension.as_str().into(),
                adult: choice.adult,
            })
            .collect()
    }

    /// The Refresh layout: Last used, Pinned and All sources with language
    /// chips. The flat `rows` grouping stays for older callers.
    pub fn sections(&self, choices: &[SourceChoice]) -> Sections {
        let rows = self.rows(choices);
        let last_used = self.last_used.as_deref().and_then(|id| {
            rows.iter()
                .find(|row| row.id.as_str() == id)
                .map(|row| (row.clone(), self.last_used_at))
        });
        // Pinned sources in stored order (identities sort as they are stored).
        let pinned: Vec<_> = self
            .pinned
            .iter()
            .filter_map(|id| rows.iter().find(|row| row.id.as_str() == id.as_str()))
            .cloned()
            .collect();
        let mut all = rows;
        all.sort_by_cached_key(|row| (row.name.to_lowercase(), row.id.to_string()));
        for row in &mut all {
            row.group = row.language.clone();
        }
        // Chips by count; "All languages" sources only appear under All.
        let any = text::tr("All languages");
        let mut languages: Vec<(String, usize)> = Vec::new();
        for row in all.iter().filter(|row| row.language.as_str() != any) {
            match languages
                .iter_mut()
                .find(|(name, _)| name == row.language.as_str())
            {
                Some((_, count)) => *count += 1,
                None => languages.push((row.language.to_string(), 1)),
            }
        }
        languages.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let language_index = self
            .language
            .as_ref()
            .and_then(|name| languages.iter().position(|(l, _)| l == name))
            .map_or(0, |i| i + 1);
        let total = all.len();
        if language_index > 0 {
            let name = &languages[language_index - 1].0;
            all.retain(|row| row.language.as_str() == name);
        }
        Sections {
            last_used,
            pinned,
            all,
            languages,
            total,
            language_index,
        }
    }
}

/// "EN", "JA", "ALL": the short tag on a source row.
fn language_code(language: &str) -> String {
    let normalized = language.trim().to_lowercase().replace('_', "-");
    match normalized.as_str() {
        "all" | "any" | "*" | "" => "ALL".into(),
        "multi" | "multiple" => "MULTI".into(),
        _ => normalized
            .split('-')
            .next()
            .unwrap_or_default()
            .chars()
            .take(3)
            .collect::<String>()
            .to_uppercase(),
    }
}

/// "opened 2 h ago" for the Last used card.
pub fn opened_ago(at: i64, now: i64) -> String {
    let seconds = (now - at).max(0);
    let (amount, unit) = match seconds {
        0..60 => return text::tr("opened just now"),
        60..3600 => (seconds / 60, "min"),
        3600..86400 => (seconds / 3600, "h"),
        _ => (seconds / 86400, "d"),
    };
    format!(
        "{} {amount} {} {}",
        text::tr("opened"),
        text::tr(unit),
        text::tr("ago")
    )
}

fn persist(stored: &str) -> Result<(), String> {
    reader_domain::profile::kv::try_set_str(STORAGE_KEY, stored).map_err(|error| error.to_string())
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

/// The display name of a source language ("English", "All languages").
pub fn language_label(language: &str) -> String {
    language_name(language)
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
            adult: false,
            extension: "Example".into(),
        }
    }

    #[test]
    fn sections_split_last_used_pins_and_language_chips() {
        let mut browser = SourceBrowser::default();
        browser.toggle_pin_with("beta.en", |_| Ok(())).unwrap();
        browser.used_at("alpha.ko", 100, |_| Ok(())).unwrap();
        let choices = [
            choice("zulu.ko", "Zulu", "ko", 0),
            choice("alpha.ko", "Alpha", "ko", 1),
            choice("beta.en", "Beta", "en", 2),
            choice("multi.all", "Every", "all", 3),
            choice("gamma.ko", "Gamma", "ko", 4),
        ];
        let sections = browser.sections(&choices);
        let (last, at) = sections.last_used.unwrap();
        assert_eq!(last.id, "alpha.ko");
        assert_eq!(at, Some(100));
        assert_eq!(identities(&sections.pinned), ["beta.en"]);
        // All sources by name; "All languages" has no chip of its own.
        assert_eq!(
            identities(&sections.all),
            ["alpha.ko", "beta.en", "multi.all", "gamma.ko", "zulu.ko"]
        );
        assert_eq!(sections.total, 5);
        assert_eq!(
            sections.languages,
            [("Korean".to_string(), 3), ("English".to_string(), 1)]
        );
        assert_eq!(sections.all[0].code, "KO");
        assert_eq!(sections.all[2].code, "ALL");
        browser.language = Some("Korean".into());
        let korean = browser.sections(&choices);
        assert_eq!(korean.language_index, 1);
        assert_eq!(identities(&korean.all), ["alpha.ko", "gamma.ko", "zulu.ko"]);
        // A chip whose sources vanished falls back to All.
        browser.language = Some("French".into());
        assert_eq!(browser.sections(&choices).language_index, 0);
    }

    #[test]
    fn preferences_without_newer_fields_still_load() {
        let browser = SourceBrowser::from_json(Some(r#"{"pinned":["a.en"],"last_used":"a.en"}"#));
        assert!(browser.pinned.contains("a.en"));
        assert_eq!(browser.last_used.as_deref(), Some("a.en"));
        assert_eq!(browser.last_used_at, None);
        let browser = SourceBrowser::from_json(Some(
            r#"{"pinned":[],"last_used":"a.en","last_used_at":1700000000,"language":"English"}"#,
        ));
        assert_eq!(browser.last_used_at, Some(1_700_000_000));
        assert_eq!(browser.language.as_deref(), Some("English"));
        assert_eq!(opened_ago(1000, 1000 + 2 * 3600), "opened 2 h ago");
        assert_eq!(opened_ago(1000, 1010), "opened just now");
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

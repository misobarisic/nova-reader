//! Device-local entry presentation, independent of strict domain and sync state.
use reader_domain::SeriesId;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Mutex};

const PREFIX: &str = "reader:content-presentation:";
static SNAPSHOTS: Mutex<BTreeMap<String, Settings>> = Mutex::new(BTreeMap::new());

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserPreference {
    #[default]
    Default,
    Reader,
    Gallery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Reader,
    Gallery,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub user: UserPreference,
    pub last_known_addon_hint: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    settings: Settings,
}

fn key(series: &SeriesId) -> String {
    format!(
        "{PREFIX}{}",
        serde_json::to_string(series).expect("SeriesId serializes")
    )
}

fn read(series: &SeriesId) -> Result<Settings, String> {
    let Some(raw) = nova_storage::try_get_str(&key(series)).map_err(|e| e.to_string())? else {
        return Ok(Settings::default());
    };
    match serde_json::from_str::<Record>(&raw) {
        Ok(record) if record.schema_version == 1 => Ok(record.settings),
        _ => {
            eprintln!("Nova Reader: invalid content presentation record; using Default");
            Ok(Settings::default())
        }
    }
}

fn write(series: &SeriesId, settings: &Settings) -> Result<(), String> {
    let raw = serde_json::to_string(&Record {
        schema_version: 1,
        settings: settings.clone(),
    })
    .map_err(|e| e.to_string())?;
    nova_storage::try_set_str(&key(series), &raw).map_err(|e| e.to_string())
}

impl Settings {
    /// Cold reads and mutations share one lock, so a stale load cannot replace
    /// a successfully published write. Hot card projections use memory only.
    pub fn load(series: &SeriesId) -> Result<Self, String> {
        let mut snapshots = SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(settings) = snapshots.get(&key(series)) {
            return Ok(settings.clone());
        }
        let settings = read(series)?;
        snapshots.insert(key(series), settings.clone());
        Ok(settings)
    }

    pub fn set_user(series: &SeriesId, user: UserPreference) -> Result<Self, String> {
        let mut snapshots = SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = match snapshots.get(&key(series)) {
            Some(settings) => settings.clone(),
            None => read(series)?,
        };
        settings.user = user;
        write(series, &settings)?;
        snapshots.insert(key(series), settings.clone());
        Ok(settings)
    }

    /// A missing hint withdraws the suggestion; never changes the user's choice.
    pub fn record_hint(series: &SeriesId, hint: Option<&str>) -> Result<Self, String> {
        let mut snapshots = SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = match snapshots.get(&key(series)) {
            Some(settings) => settings.clone(),
            None => read(series)?,
        };
        // Persist only known bounded tokens, never arbitrary addon payloads.
        let hint = hint.filter(|hint| matches!(*hint, "gallery" | "reader"));
        if settings.last_known_addon_hint.as_deref() != hint {
            settings.last_known_addon_hint = hint.map(str::to_owned);
            write(series, &settings)?;
        }
        snapshots.insert(key(series), settings.clone());
        Ok(settings)
    }

    pub fn effective(&self) -> Presentation {
        match self.user {
            UserPreference::Gallery => Presentation::Gallery,
            UserPreference::Reader => Presentation::Reader,
            UserPreference::Default if self.last_known_addon_hint.as_deref() == Some("gallery") => {
                Presentation::Gallery
            }
            UserPreference::Default => Presentation::Reader,
        }
    }
}

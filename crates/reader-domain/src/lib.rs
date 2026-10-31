//! Reader-owned identities and metadata. Backups deliberately have no path,
//! executable, session, cookie, or credential fields.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub const SCHEMA_VERSION: u32 = 1;
const STATE_KEY: &str = "reader:state";
const MAX_BACKUP_BYTES: usize = 32 * 1024 * 1024;
static STORE_PATH: OnceLock<PathBuf> = OnceLock::new();
static SERIAL: Mutex<()> = Mutex::new(());

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Storage(#[from] nova_storage::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriesId {
    pub source_id: String,
    pub local_id: String,
}
impl SeriesId {
    pub fn new(source_id: impl Into<String>, local_id: impl Into<String>) -> Self {
        Self {
            source_id: source_id.into(),
            local_id: local_id.into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChapterId {
    pub series: SeriesId,
    pub local_id: String,
}
impl ChapterId {
    pub fn new(series: SeriesId, local_id: impl Into<String>) -> Self {
        Self {
            series,
            local_id: local_id.into(),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadingMode {
    #[default]
    Webtoon,
    LeftToRight,
    RightToLeft,
    Vertical,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderPreferences {
    pub mode: ReadingMode,
    pub fit_width: bool,
    pub gap_pixels: u16,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDescriptor {
    pub id: String,
    pub addon_id: String,
    pub name: String,
    pub language: String,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Series {
    pub id: SeriesId,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    pub cover_url: Option<String>,
    pub canonical_url: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chapter {
    pub id: ChapterId,
    pub title: String,
    pub label: String,
    pub number: Option<String>,
    pub volume: Option<String>,
    pub source_order: i64,
    pub language: Option<String>,
    pub scanlator: Option<String>,
    pub uploaded_at: Option<i64>,
}
/// Offset is a normalized fixed point fraction in [0, 1_000_000]. It survives
/// rescaling and tile changes without floating point serialization drift.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeAnchor {
    pub page_id: String,
    pub page_index: usize,
    pub offset_millionths: u32,
}
impl ResumeAnchor {
    pub fn new(page_id: impl Into<String>, page_index: usize, offset: f64) -> Result<Self> {
        if !offset.is_finite() || !(0.0..=1.0).contains(&offset) {
            return Err(Error::Invalid("invalid page offset".into()));
        }
        Ok(Self {
            page_id: page_id.into(),
            page_index,
            offset_millionths: (offset * 1_000_000.0).round() as u32,
        })
    }
    pub fn offset(&self) -> f64 {
        f64::from(self.offset_millionths) / 1_000_000.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadingProgress {
    pub chapter: ChapterId,
    pub anchor: ResumeAnchor,
    pub mode: ReadingMode,
    pub last_read_at: i64,
    pub completed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryEntry {
    pub series: SeriesId,
    pub categories: BTreeSet<String>,
    pub preferences: Option<ReaderPreferences>,
    pub bookmarked: bool,
    pub added_at: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Category {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    pub chapter: ChapterId,
    pub anchor: ResumeAnchor,
    pub note: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub chapter: ChapterId,
    pub read_at: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderState {
    pub schema_version: u32,
    #[serde(default)]
    pub sources: Vec<SourceDescriptor>,
    #[serde(default)]
    pub series: Vec<Series>,
    #[serde(default)]
    pub chapters: Vec<Chapter>,
    #[serde(default)]
    pub library: Vec<LibraryEntry>,
    #[serde(default)]
    pub categories: Vec<Category>,
    #[serde(default)]
    pub progress: Vec<ReadingProgress>,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    #[serde(default)]
    pub preferences: ReaderPreferences,
}
impl Default for ReaderState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            sources: vec![],
            series: vec![],
            chapters: vec![],
            library: vec![],
            categories: vec![],
            progress: vec![],
            history: vec![],
            bookmarks: vec![],
            preferences: ReaderPreferences::default(),
        }
    }
}
fn nonempty(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 8192 && !value.contains('\0')
}
fn valid_series(id: &SeriesId) -> bool {
    nonempty(&id.source_id) && nonempty(&id.local_id)
}
fn valid_chapter(id: &ChapterId) -> bool {
    valid_series(&id.series) && nonempty(&id.local_id)
}
fn decimal(value: &str) -> bool {
    let value = value.strip_prefix('-').unwrap_or(value);
    let mut parts = value.split('.');
    let first = parts.next().unwrap_or("");
    !first.is_empty()
        && first.bytes().all(|b| b.is_ascii_digit())
        && parts
            .next()
            .is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none()
}
fn unique<T: Ord>(items: impl Iterator<Item = T>) -> bool {
    let mut seen = BTreeSet::new();
    items.into_iter().all(|x| seen.insert(x))
}
impl ReaderState {
    pub fn validate(&self) -> Result<()> {
        let invalid = || Error::Invalid("invalid or duplicate metadata record".into());
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::Invalid("unsupported metadata schema".into()));
        }
        if !unique(self.series.iter().map(|s| &s.id))
            || !unique(self.chapters.iter().map(|c| &c.id))
            || !unique(self.library.iter().map(|l| &l.series))
            || !unique(self.progress.iter().map(|p| &p.chapter))
            || !unique(self.categories.iter().map(|c| &c.id))
            || !unique(self.sources.iter().map(|s| &s.id))
        {
            return Err(invalid());
        }
        if self
            .series
            .iter()
            .any(|s| !valid_series(&s.id) || !nonempty(&s.title))
            || self.chapters.iter().any(|c| {
                !valid_chapter(&c.id)
                    || c.number.as_deref().is_some_and(|v| !decimal(v))
                    || c.volume.as_deref().is_some_and(|v| !decimal(v))
            })
        {
            return Err(invalid());
        }
        if self.library.iter().any(|l| {
            !valid_series(&l.series)
                || l.categories
                    .iter()
                    .any(|id| !self.categories.iter().any(|c| &c.id == id))
        }) || self
            .categories
            .iter()
            .any(|c| !nonempty(&c.id) || !nonempty(&c.name))
            || self
                .sources
                .iter()
                .any(|s| !nonempty(&s.id) || !nonempty(&s.addon_id))
        {
            return Err(invalid());
        }
        if self.progress.iter().any(|p| {
            !valid_chapter(&p.chapter)
                || !nonempty(&p.anchor.page_id)
                || p.anchor.offset_millionths > 1_000_000
        }) || self.bookmarks.iter().any(|b| {
            !valid_chapter(&b.chapter)
                || !nonempty(&b.anchor.page_id)
                || b.anchor.offset_millionths > 1_000_000
        }) || self.history.iter().any(|h| !valid_chapter(&h.chapter))
        {
            return Err(invalid());
        }
        let series: BTreeSet<_> = self.series.iter().map(|s| &s.id).collect();
        let chapters: BTreeSet<_> = self.chapters.iter().map(|c| &c.id).collect();
        if self.chapters.iter().any(|c| !series.contains(&c.id.series))
            || self.library.iter().any(|l| !series.contains(&l.series))
            || self.progress.iter().any(|p| !chapters.contains(&p.chapter))
            || self
                .bookmarks
                .iter()
                .any(|b| !chapters.contains(&b.chapter))
            || self.history.iter().any(|h| !chapters.contains(&h.chapter))
        {
            return Err(Error::Invalid("dangling metadata reference".into()));
        }
        Ok(())
    }
    pub fn set_progress(&mut self, progress: ReadingProgress) {
        self.progress.retain(|p| p.chapter != progress.chapter);
        self.history.retain(|h| h.chapter != progress.chapter);
        self.history.push(HistoryEntry {
            chapter: progress.chapter.clone(),
            read_at: progress.last_read_at,
        });
        self.progress.push(progress);
    }
}
fn decode(bytes: &[u8]) -> Result<ReaderState> {
    if bytes.len() > MAX_BACKUP_BYTES {
        return Err(Error::Invalid("metadata exceeds limit".into()));
    }
    // Schema zero used the identical shape before explicit versioning. Migration
    // is prepared and validated fully before the single durable replacement.
    let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
    if value.get("schema_version").and_then(|v| v.as_u64()) == Some(0) {
        value["schema_version"] = SCHEMA_VERSION.into();
    }
    let state: ReaderState = serde_json::from_value(value)?;
    state.validate()?;
    Ok(state)
}
#[derive(Clone, Debug)]
pub struct DomainStore {
    _path: PathBuf,
}
impl DomainStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(path.as_ref())?;
        let path = std::fs::canonicalize(path)?;
        if STORE_PATH.get().is_some_and(|p| p != &path) {
            return Err(Error::Invalid(
                "nova-storage supports one application directory per process".into(),
            ));
        }
        nova_storage::init_at(&path);
        let _ = STORE_PATH.set(path.clone());
        let state = Self::read()?;
        nova_storage::try_set_str(STATE_KEY, &serde_json::to_string(&state)?)?;
        Ok(Self { _path: path })
    }
    fn read() -> Result<ReaderState> {
        nova_storage::try_get_str(STATE_KEY)?
            .map(|s| decode(s.as_bytes()))
            .transpose()
            .map(|s| s.unwrap_or_default())
    }
    pub fn snapshot(&self) -> Result<ReaderState> {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        Self::read()
    }
    pub fn update(&self, update: impl FnOnce(&mut ReaderState) -> Result<()>) -> Result<()> {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = Self::read()?;
        update(&mut state)?;
        state.validate()?;
        let json = serde_json::to_string(&state)?;
        if json.len() > MAX_BACKUP_BYTES {
            return Err(Error::Invalid("metadata exceeds limit".into()));
        }
        nova_storage::try_set_str(STATE_KEY, &json)?;
        Ok(())
    }
    pub fn backup_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(&self.snapshot()?)?)
    }
    pub fn restore_json(&self, bytes: &[u8]) -> Result<()> {
        let state = decode(bytes)?;
        self.update(|current| {
            *current = state;
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_restore_and_atomic_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = DomainStore::open(dir.path()).unwrap();
        let chapter = ChapterId::new(SeriesId::new("fixture", "a"), "special");
        store
            .update(|s| {
                s.series.push(Series {
                    id: chapter.series.clone(),
                    title: "Fixture".into(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                });
                s.chapters.push(Chapter {
                    id: chapter.clone(),
                    title: "Special".into(),
                    label: "Special".into(),
                    number: None,
                    volume: None,
                    source_order: 0,
                    language: None,
                    scanlator: None,
                    uploaded_at: None,
                });
                s.set_progress(ReadingProgress {
                    chapter: chapter.clone(),
                    anchor: ResumeAnchor::new("page", 3, 0.123456).unwrap(),
                    mode: ReadingMode::RightToLeft,
                    last_read_at: 42,
                    completed: false,
                });
                Ok(())
            })
            .unwrap();
        let backup = store.backup_json().unwrap();
        store
            .update(|s| {
                *s = ReaderState::default();
                Ok(())
            })
            .unwrap();
        store.restore_json(&backup).unwrap();
        assert_eq!(
            store.snapshot().unwrap().progress[0]
                .anchor
                .offset_millionths,
            123456
        );
        assert!(store.restore_json(br#"{"schema_version":999}"#).is_err());
        assert!(
            store
                .update(|state| {
                    state.chapters.clear();
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(store.backup_json().unwrap(), backup);
        let backup_path = dir.path().join("metadata-backup.json");
        std::fs::write(&backup_path, &backup).unwrap();
        let clean = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::clean_restore_child")
            .env("NOVA_READER_TEST_BACKUP", &backup_path)
            .output()
            .unwrap();
        assert!(
            clean.status.success(),
            "{}",
            String::from_utf8_lossy(&clean.stderr)
        );
        assert!(!String::from_utf8(backup).unwrap().contains("cookie"));
    }
    #[test]
    fn clean_restore_child() {
        let Some(backup) = std::env::var_os("NOVA_READER_TEST_BACKUP") else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let store = DomainStore::open(directory.path()).unwrap();
        assert_eq!(store.snapshot().unwrap(), ReaderState::default());
        store.restore_json(&std::fs::read(backup).unwrap()).unwrap();
        let restored = store.snapshot().unwrap();
        assert!(restored.sources.is_empty());
        assert_eq!(restored.progress[0].anchor.offset_millionths, 123456);
        assert!(!directory.path().join("downloads").exists());
        let mut legacy = serde_json::to_value(&restored).unwrap();
        legacy["schema_version"] = 0.into();
        store
            .restore_json(&serde_json::to_vec(&legacy).unwrap())
            .unwrap();
        assert_eq!(store.snapshot().unwrap(), restored);
        legacy["cookies"] = "forbidden".into();
        assert!(
            store
                .restore_json(&serde_json::to_vec(&legacy).unwrap())
                .is_err()
        );
    }
    #[test]
    fn identity_decimal_and_anchor_validation() {
        assert_ne!(SeriesId::new("a", "b:c"), SeriesId::new("a:b", "c"));
        assert!(decimal("12.50"));
        assert!(!decimal("1.2.3"));
        assert!(!decimal("NaN"));
        assert!(ResumeAnchor::new("page", 0, f64::NAN).is_err());
    }
}

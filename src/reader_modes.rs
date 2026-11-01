//! Per-series reader selection and the TachiyomiJ2K metadata classifier.
//!
//! Classification adapted from Jays2Kings/tachiyomiJ2K at
//! b83defe42b2ac9f460667bda3a42fdd9d12e767e:
//! app/src/main/java/eu/kanade/tachiyomi/data/database/models/Manga.kt,
//! defaultReaderType and its tag/source helpers (lines 166–239).
//! Copyright 2015 Javier Tomás; Tachiyomi/TachiyomiJ2K contributors.
//! Upstream Apache-2.0; see docs/reader-modes.md and packaged third-party notices.
//! Nova changes: Rust port, explicit Default/Automatic options, composite series
//! keys, fallible persistence, device-default inheritance without progress writes,
//! and explicit RTL for recognized manga tags while retaining unknown fallback.
use reader_domain::{ReadingMode, SeriesId};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

const PREFIX: &str = "reader:mode-choice:";
static SERIAL: Mutex<()> = Mutex::new(());
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModeChoice {
    #[default]
    Default,
    Automatic,
    Manual(ReadingMode),
    LongStripWithGaps,
}
impl ModeChoice {
    pub fn resolve(self, automatic: ReadingMode) -> ReadingMode {
        match self {
            Self::Default => self
                .resolve_with_default(crate::reader_settings::snapshot().default_mode, automatic),
            Self::Automatic => automatic,
            Self::LongStripWithGaps => ReadingMode::Webtoon,
            Self::Manual(mode) => mode,
        }
    }
    /// Pure resolution keeps Automatic independent of the device default.
    /// A defensive Default global value resolves automatically rather than cycling.
    pub fn resolve_with_default(self, global: Self, automatic: ReadingMode) -> ReadingMode {
        match self.inherit(global) {
            Self::Default | Self::Automatic => automatic,
            Self::LongStripWithGaps => ReadingMode::Webtoon,
            Self::Manual(mode) => mode,
        }
    }
    pub fn inherit(self, global: Self) -> Self {
        if self == Self::Default { global } else { self }
    }
    pub fn is_gapped(self) -> bool {
        match self {
            Self::Default => {
                self.is_gapped_with_default(crate::reader_settings::snapshot().default_mode)
            }
            Self::LongStripWithGaps => true,
            _ => false,
        }
    }
    pub fn is_gapped_with_default(self, global: Self) -> bool {
        self.inherit(global) == Self::LongStripWithGaps
    }
    pub fn selection(self) -> i32 {
        match self {
            Self::Default => 6,
            Self::Automatic => 4,
            Self::LongStripWithGaps => 5,
            Self::Manual(ReadingMode::Webtoon) => 0,
            Self::Manual(ReadingMode::LeftToRight) => 1,
            Self::Manual(ReadingMode::RightToLeft) => 2,
            Self::Manual(ReadingMode::Vertical) => 3,
        }
    }
    pub fn from_selection(index: i32) -> Option<Self> {
        Some(match index {
            0 => Self::Manual(ReadingMode::Webtoon),
            1 => Self::Manual(ReadingMode::LeftToRight),
            2 => Self::Manual(ReadingMode::RightToLeft),
            3 => Self::Manual(ReadingMode::Vertical),
            4 => Self::Automatic,
            5 => Self::LongStripWithGaps,
            6 => Self::Default,
            _ => return None,
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    choice: ModeChoice,
}
fn key(series: &SeriesId) -> String {
    format!(
        "{PREFIX}{}",
        serde_json::to_string(series).expect("SeriesId serializes")
    )
}
fn write(series: &SeriesId, choice: ModeChoice) -> Result<(), String> {
    let raw = serde_json::to_string(&Record {
        schema_version: 1,
        choice,
    })
    .map_err(|e| e.to_string())?;
    nova_storage::try_set_str(&key(series), &raw).map_err(|e| e.to_string())
}
pub fn save(series: &SeriesId, choice: ModeChoice) -> Result<(), String> {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    write(series, choice)
}
/// Migrate only an explicitly stored per-library preference. A global default
/// or old chapter progress is never evidence that this series was overridden.
/// Explicit Automatic is a record (not deletion), so legacy data cannot revive.
pub fn load(series: &SeriesId, legacy: Option<ReadingMode>) -> Result<ModeChoice, String> {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(raw) = nova_storage::try_get_str(&key(series)).map_err(|e| e.to_string())? {
        return match serde_json::from_str::<Record>(&raw) {
            Ok(record) if record.schema_version == 1 => Ok(record.choice),
            _ => {
                eprintln!(
                    "Nova Reader: invalid reader mode preference; preserving record and using Default"
                );
                Ok(ModeChoice::Default)
            }
        };
    }
    if let Some(mode) = legacy {
        let choice = ModeChoice::Manual(mode);
        if write(series, choice).is_err() {
            // The old library record is already durable. Failure to migrate
            // its representation must not prevent offline chapter reading.
            eprintln!(
                "Nova Reader: could not migrate reader mode; keeping the saved library preference"
            );
        }
        return Ok(choice);
    }
    Ok(ModeChoice::Default)
}
fn manga_tag(tag: &str) -> bool {
    matches!(tag, "manga" | "манга" | "jp") || tag.starts_with("japanese")
}
fn manhwa_tag(tag: &str) -> bool {
    matches!(tag, "long strip" | "manhwa" | "манхва" | "kr") || tag.starts_with("korean")
}
fn manhua_tag(tag: &str) -> bool {
    // These mixed-case script codes reproduce the pinned upstream list, after
    // its own lowercase normalization; they do not become extra source hints.
    matches!(
        tag,
        "manhua" | "маньхуа" | "cn" | "hk" | "zh-Hans" | "zh-Hant"
    ) || tag.starts_with("chinese")
}
fn comic_tag(tag: &str) -> bool {
    matches!(tag, "comic" | "комикс" | "en" | "gb") || tag.starts_with("english")
}
/// J2K uses metadata, not page dimensions. No aspect-ratio thresholds are
/// implied here. An unrecognized series keeps Reader's existing domain fallback.
/// Upstream leaves manga tags unset and normally inherits its RTL default; Reader
/// makes recognized manga explicit so its legacy Webtoon fallback applies only
/// to unrecognized metadata, independently of a manual device default.
pub fn automatic(genres: &[String], source_name: &str, fallback: ReadingMode) -> ReadingMode {
    let tags = genres
        .iter()
        .flat_map(|g| g.split(','))
        .map(|g| g.trim().to_lowercase())
        .filter(|g| !g.is_empty())
        .collect::<Vec<_>>();
    let source = source_name.to_lowercase();
    let webtoon_source = ["webtoon", "manhwa", "toonily"]
        .iter()
        .any(|name| source.contains(name));
    let comic_source = [
        "gunnerkrigg",
        "dilbert",
        "cyanide",
        "xkcd",
        "tapas",
        "comicextra",
        "read comics online",
        "readcomiconline",
    ]
    .iter()
    .any(|name| source.contains(name));
    if tags
        .iter()
        .any(|tag| manhwa_tag(tag) || tag.contains("webtoon"))
        || (webtoon_source && !tags.iter().any(|tag| manhua_tag(tag) || comic_tag(tag)))
    {
        ReadingMode::Webtoon
    } else if tags.iter().any(|tag| {
        matches!(tag.as_str(), "chinese" | "manhua" | "comic") || tag.starts_with("english")
    }) || (comic_source
        && !source.contains("tapas")
        && !tags.iter().any(|tag| manga_tag(tag)))
        || (source.contains("manhua") && !tags.iter().any(|tag| manga_tag(tag)))
    {
        ReadingMode::LeftToRight
    } else if tags.iter().any(|tag| manga_tag(tag)) {
        ReadingMode::RightToLeft
    } else {
        fallback
    }
}
pub fn legacy_mode(state: &reader_domain::ReaderState, series: &SeriesId) -> Option<ReadingMode> {
    state
        .library
        .iter()
        .find(|entry| &entry.series == series)
        .and_then(|entry| entry.preferences.as_ref())
        .map(|p| p.mode)
}
pub fn automatic_for_state(
    state: &reader_domain::ReaderState,
    series: &SeriesId,
    actual_source_name: Option<&str>,
) -> ReadingMode {
    let genres = state
        .series
        .iter()
        .find(|s| &s.id == series)
        .map(|s| s.genres.as_slice())
        .unwrap_or(&[]);
    let source_name = actual_source_name
        .or_else(|| {
            state
                .sources
                .iter()
                .filter(|source| {
                    source.id == series.source_id
                        || series.source_id.starts_with(&format!("{}.", source.id))
                })
                .max_by_key(|s| s.id.len())
                .map(|s| s.name.as_str())
        })
        .unwrap_or("");
    automatic(genres, source_name, state.preferences.mode)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_automatic_defaults_classify_manga_without_using_legacy_webtoon_fallback() {
        let mut state = reader_domain::ReaderState::default();
        assert_eq!(state.preferences.mode, ReadingMode::Webtoon);
        let global_manual = ModeChoice::Manual(ReadingMode::Vertical);
        for (index, genres, source, expected) in [
            (0, "manga", "Unknown", ReadingMode::RightToLeft),
            (1, "Japanese comics", "Unknown", ReadingMode::RightToLeft),
            (2, "Манга", "Unknown", ReadingMode::RightToLeft),
            (3, "JP", "Unknown", ReadingMode::RightToLeft),
            (4, "manhwa", "Unknown", ReadingMode::Webtoon),
            (5, "manhua", "Unknown", ReadingMode::LeftToRight),
            (6, "action", "Unknown", ReadingMode::Webtoon),
            (7, "Japanese", "ComicExtra", ReadingMode::RightToLeft),
            // Retain the pinned classifier's Webtoon and explicit LTR priority.
            (8, "manga, webtoon", "Unknown", ReadingMode::Webtoon),
            (9, "Japanese, English", "Unknown", ReadingMode::LeftToRight),
        ] {
            let id = SeriesId::new("source", index.to_string());
            state.series.push(reader_domain::Series {
                publication_status: Default::default(),
                id: id.clone(),
                title: format!("Title {index}"),
                description: String::new(),
                authors: vec![],
                genres: vec![genres.into()],
                cover_url: None,
                canonical_url: None,
            });
            let automatic = automatic_for_state(&state, &id, Some(source));
            assert_eq!(automatic, expected, "{genres} / {source}");
            assert_eq!(
                ModeChoice::Default.resolve_with_default(ModeChoice::Automatic, automatic),
                expected
            );
            assert_eq!(
                ModeChoice::Automatic.resolve_with_default(global_manual, automatic),
                expected
            );
            assert_eq!(
                ModeChoice::Default.resolve_with_default(global_manual, automatic),
                ReadingMode::Vertical
            );
        }
        // Changing the legacy fallback still affects unknown metadata only.
        state.preferences.mode = ReadingMode::LeftToRight;
        assert_eq!(
            automatic_for_state(&state, &SeriesId::new("source", "0"), Some("Unknown")),
            ReadingMode::RightToLeft
        );
        assert_eq!(
            automatic_for_state(&state, &SeriesId::new("source", "6"), Some("Unknown")),
            ReadingMode::LeftToRight
        );
    }

    fn detect(tags: &[&str], source: &str) -> ReadingMode {
        automatic(
            &tags.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
            source,
            ReadingMode::RightToLeft,
        )
    }
    #[test]
    fn j2k_tags_sources_exclusions_and_priority_match_pinned_rules() {
        for tag in [
            "long strip",
            " MANHWA ",
            "Korean comics",
            "KR",
            "Манхва",
            "full color, webtoons",
        ] {
            assert_eq!(detect(&[tag], "Unknown"), ReadingMode::Webtoon);
        }
        for source in ["Manhwa18.cc", "WebToon", "TOONILY"] {
            assert_eq!(detect(&[], source), ReadingMode::Webtoon);
        }
        for tag in ["chinese", "manhua", "English publication", "comic"] {
            assert_eq!(detect(&[tag], "Unknown"), ReadingMode::LeftToRight);
        }
        for source in [
            "Gunnerkrigg",
            "Dilbert",
            "Cyanide",
            "xkcd",
            "ComicExtra",
            "Read Comics Online",
            "ReadComicOnline",
            "Manhua Source",
        ] {
            assert_eq!(detect(&[], source), ReadingMode::LeftToRight);
        }
        assert_eq!(
            detect(&["manhua"], "Manhwa Source"),
            ReadingMode::LeftToRight
        );
        assert_eq!(
            detect(&["comic"], "Webtoon Source"),
            ReadingMode::LeftToRight
        );
        assert_eq!(
            detect(&["マンガ", "manga"], "ComicExtra"),
            ReadingMode::RightToLeft
        );
        assert_eq!(
            detect(&["Japanese"], "Manhua Source"),
            ReadingMode::RightToLeft
        );
        assert_eq!(detect(&[], "Tapas"), ReadingMode::RightToLeft);
        assert_eq!(detect(&["en"], "Manhwa Source"), ReadingMode::RightToLeft);
        assert_eq!(
            detect(&["long strip", "manhua"], "ComicExtra"),
            ReadingMode::Webtoon
        );
        assert_eq!(
            automatic(&[], "Unknown", ReadingMode::Vertical),
            ReadingMode::Vertical
        );
    }
    #[test]
    fn manual_modes_override_all_metadata_and_selection_indices_stay_compatible() {
        for index in 0..4 {
            let choice = ModeChoice::from_selection(index).unwrap();
            assert_eq!(choice.selection(), index);
            assert_eq!(
                choice.resolve(ReadingMode::Webtoon),
                match choice {
                    ModeChoice::Manual(mode) => mode,
                    _ => unreachable!(),
                }
            );
        }
        assert_eq!(ModeChoice::from_selection(4), Some(ModeChoice::Automatic));
        assert_eq!(
            ModeChoice::from_selection(5),
            Some(ModeChoice::LongStripWithGaps)
        );
        assert_eq!(ModeChoice::from_selection(6), Some(ModeChoice::Default));
        assert!(ModeChoice::from_selection(7).is_none());
        assert_ne!(
            key(&SeriesId::new("a", "b:c")),
            key(&SeriesId::new("a:b", "c"))
        );
        assert_ne!(
            key(&SeriesId::new("a", "same")),
            key(&SeriesId::new("b", "same"))
        );
    }
    #[test]
    fn default_inherits_but_explicit_automatic_bypasses_the_global_choice() {
        let global = ModeChoice::Manual(ReadingMode::RightToLeft);
        assert_eq!(ModeChoice::default(), ModeChoice::Default);
        assert_eq!(
            ModeChoice::Default.resolve_with_default(global, ReadingMode::Webtoon),
            ReadingMode::RightToLeft
        );
        assert_eq!(
            ModeChoice::Automatic.resolve_with_default(global, ReadingMode::Webtoon),
            ReadingMode::Webtoon
        );
        assert_eq!(
            ModeChoice::Default.resolve_with_default(ModeChoice::Automatic, ReadingMode::Vertical),
            ReadingMode::Vertical
        );
        assert_eq!(
            ModeChoice::Default.resolve_with_default(ModeChoice::Default, ReadingMode::Vertical),
            ReadingMode::Vertical
        );
        assert_eq!(
            ModeChoice::LongStripWithGaps.resolve_with_default(global, ReadingMode::RightToLeft),
            ReadingMode::Webtoon
        );
        assert!(ModeChoice::Default.is_gapped_with_default(ModeChoice::LongStripWithGaps));
        assert!(!ModeChoice::Automatic.is_gapped_with_default(ModeChoice::LongStripWithGaps));
        assert!(ModeChoice::LongStripWithGaps.is_gapped_with_default(global));
        for index in 0..=6 {
            assert_eq!(
                ModeChoice::from_selection(index).unwrap().selection(),
                index
            );
        }
    }
}

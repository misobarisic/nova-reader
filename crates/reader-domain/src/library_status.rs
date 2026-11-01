//! Adapted from Nova b6a35a8, src/app.rs::WatchStatus and
//! src/app/watch_state.rs::activity_bucket (GPL-3.0-or-later).
//! Manual pins override progress; Auto re-derives the bucket without editing it.
use crate::{Chapter, LibraryEntry, ReadingProgress};
use serde::{Deserialize, Serialize};

/// Source publication state is independent of the user's reading status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationStatus {
    #[default]
    Unknown,
    Ongoing,
    Completed,
    OnHiatus,
    Cancelled,
}
impl PublicationStatus {
    pub fn from_source(value: Option<&str>) -> Self {
        match value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "ongoing" | "releasing" => Self::Ongoing,
            "completed" | "finished" | "ended" => Self::Completed,
            "on_hiatus" | "hiatus" => Self::OnHiatus,
            "cancelled" | "canceled" | "discontinued" | "abandoned" => Self::Cancelled,
            _ => Self::Unknown,
        }
    }
    pub fn is_unknown(&self) -> bool {
        *self == Self::Unknown
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadingStatus {
    #[default]
    Auto,
    Reading,
    PlanToRead,
    #[serde(alias = "Read")]
    Completed,
    Dropped,
    OnHold,
}
impl ReadingStatus {
    pub const BUILTINS: [Self; 5] = [
        Self::Reading,
        Self::PlanToRead,
        Self::Completed,
        Self::Dropped,
        Self::OnHold,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Reading => "Reading",
            Self::PlanToRead => "Plan to read",
            Self::Completed => "Completed",
            Self::Dropped => "Dropped",
            Self::OnHold => "On hold",
        }
    }
    pub fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
    // Nova's exact status action codes; video-only New episodes is omitted.
    pub fn from_action(status: i32) -> Option<Self> {
        match status {
            0 => Some(Self::Auto),
            1 => Some(Self::OnHold),
            2 => Some(Self::Dropped),
            5 => Some(Self::PlanToRead),
            6 => Some(Self::Reading),
            7 => Some(Self::Completed),
            _ => None,
        }
    }
}
pub fn library_bucket(
    entry: &LibraryEntry,
    chapters: &[Chapter],
    progress: &[ReadingProgress],
    publication: PublicationStatus,
) -> ReadingStatus {
    if entry.reading_status != ReadingStatus::Auto {
        return entry.reading_status;
    }
    let chapters: Vec<_> = chapters
        .iter()
        .filter(|chapter| chapter.id.series == entry.series)
        .collect();
    if publication == PublicationStatus::Completed
        && !chapters.is_empty()
        && chapters.iter().all(|chapter| {
            progress
                .iter()
                .any(|record| record.chapter == chapter.id && record.completed)
        })
    {
        ReadingStatus::Completed
    } else if progress
        .iter()
        .any(|record| record.chapter.series == entry.series)
    {
        ReadingStatus::Reading
    } else {
        ReadingStatus::PlanToRead
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChapterId, ReadingMode, ResumeAnchor, SeriesId};
    #[test]
    fn auto_buckets_and_all_manual_pins_follow_nova_without_mutating_progress() {
        let id = SeriesId::new("source-a", "same");
        let mut entry = LibraryEntry {
            series: id.clone(),
            categories: Default::default(),
            preferences: None,
            bookmarked: false,
            added_at: 0,
            reading_status: ReadingStatus::Auto,
        };
        let chapter = Chapter {
            id: ChapterId::new(id, "1"),
            title: "1".into(),
            label: "1".into(),
            number: Some("1".into()),
            volume: None,
            source_order: 0,
            language: None,
            scanlator: None,
            uploaded_at: None,
        };
        let chapters = vec![chapter.clone()];
        assert_eq!(
            library_bucket(&entry, &[], &[], PublicationStatus::Completed),
            ReadingStatus::PlanToRead
        );
        assert_eq!(
            library_bucket(&entry, &chapters, &[], PublicationStatus::Completed),
            ReadingStatus::PlanToRead
        );
        let mut progress = vec![ReadingProgress {
            chapter: chapter.id.clone(),
            anchor: ResumeAnchor::new("0", 0, 0.).unwrap(),
            mode: ReadingMode::Webtoon,
            last_read_at: 1,
            completed: false,
        }];
        assert_eq!(
            library_bucket(&entry, &chapters, &progress, PublicationStatus::Completed),
            ReadingStatus::Reading
        );
        progress[0].completed = true;
        for publication in [
            PublicationStatus::Unknown,
            PublicationStatus::Ongoing,
            PublicationStatus::OnHiatus,
            PublicationStatus::Cancelled,
        ] {
            assert_eq!(
                library_bucket(&entry, &chapters, &progress, publication),
                ReadingStatus::Reading,
                "caught up is not a completed publication"
            );
        }
        assert_eq!(
            library_bucket(&entry, &chapters, &progress, PublicationStatus::Completed),
            ReadingStatus::Completed
        );
        let mut next = chapter.clone();
        next.id.local_id = "2".into();
        assert_eq!(
            library_bucket(
                &entry,
                &[chapter.clone(), next],
                &progress,
                PublicationStatus::Completed
            ),
            ReadingStatus::Reading
        );
        let original = progress.clone();
        for pin in ReadingStatus::BUILTINS {
            entry.reading_status = pin;
            assert_eq!(
                library_bucket(&entry, &chapters, &progress, PublicationStatus::Completed),
                pin
            );
            let saved = serde_json::to_string(&entry).unwrap();
            assert_eq!(serde_json::from_str::<LibraryEntry>(&saved).unwrap(), entry);
        }
        assert_eq!(progress, original);
        entry.reading_status = ReadingStatus::Auto;
        assert_eq!(
            library_bucket(&entry, &chapters, &progress, PublicationStatus::Completed),
            ReadingStatus::Completed
        );
        entry.series.source_id = "source-b".into();
        assert_eq!(
            library_bucket(&entry, &chapters, &progress, PublicationStatus::Completed),
            ReadingStatus::PlanToRead
        );
        assert_eq!(
            serde_json::from_str::<ReadingStatus>("\"Read\"").unwrap(),
            ReadingStatus::Completed
        );
        let old: crate::Series = serde_json::from_str(r#"{"id":{"source_id":"a","local_id":"s"},"title":"Old","cover_url":null,"canonical_url":null}"#).unwrap();
        assert_eq!(old.publication_status, PublicationStatus::Unknown);
        for (source, expected) in [
            ("completed", PublicationStatus::Completed),
            (" ongoing ", PublicationStatus::Ongoing),
            ("on_hiatus", PublicationStatus::OnHiatus),
            ("cancelled", PublicationStatus::Cancelled),
            ("future", PublicationStatus::Unknown),
        ] {
            assert_eq!(PublicationStatus::from_source(Some(source)), expected);
        }
        let raw = r#"{"series":{"source_id":"a","local_id":"s"},"categories":[],"preferences":null,"bookmarked":false,"added_at":0}"#;
        assert_eq!(
            serde_json::from_str::<LibraryEntry>(raw)
                .unwrap()
                .reading_status,
            ReadingStatus::Auto
        );
    }
}

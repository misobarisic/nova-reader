//! Which chapters are new since a title was last opened on this profile.
//!
//! A local record per series lists the chapter IDs the detail screen has shown
//! (`reader:seen-chapters:<JSON SeriesId>`). Opening a title compares its
//! chapters with that record once and keeps the answer for the visit, so a
//! chapter stays marked while the reader is on the title (reading or marking
//! it read does not clear it), and chapters that arrive with a refresh during
//! the visit join it. Everything shown is added to the record, so the next
//! visit starts clean. The first visit of a title has no record and marks
//! nothing. The record is a device-local convenience: not synced, not backed
//! up, and existing data needs no migration.
use reader_domain::SeriesId;
use std::collections::BTreeSet;

const PREFIX: &str = "reader:seen-chapters:";

fn key(series: &SeriesId) -> String {
    format!(
        "{PREFIX}{}",
        serde_json::to_string(series).expect("series identities serialize")
    )
}

fn load(series: &SeriesId) -> Option<BTreeSet<String>> {
    reader_domain::profile::kv::try_get_str(&key(series))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// One visit to a title's detail screen.
pub struct Visit {
    series: SeriesId,
    /// What had been seen before this visit; `None` on a title's first visit.
    before: Option<BTreeSet<String>>,
    seen: BTreeSet<String>,
}

impl Visit {
    pub fn open(series: SeriesId) -> Self {
        let before = load(&series);
        Self {
            seen: before.clone().unwrap_or_default(),
            before,
            series,
        }
    }

    pub fn series(&self) -> &SeriesId {
        &self.series
    }

    pub fn is_new(&self, chapter: &str) -> bool {
        self.before
            .as_ref()
            .is_some_and(|before| !before.contains(chapter))
    }

    /// Remember the chapters on screen for the next visit.
    pub fn record<'a>(&mut self, chapters: impl IntoIterator<Item = &'a str>) {
        let before = self.seen.len();
        self.seen.extend(chapters.into_iter().map(str::to_owned));
        if self.seen.len() == before {
            return;
        }
        if let Ok(raw) = serde_json::to_string(&self.seen)
            && let Err(error) = reader_domain::profile::kv::try_set_str(&key(&self.series), &raw)
        {
            eprintln!("Nova Reader: could not remember seen chapters: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visit(before: Option<&[&str]>) -> Visit {
        let before: Option<BTreeSet<String>> =
            before.map(|ids| ids.iter().map(|id| id.to_string()).collect());
        Visit {
            series: SeriesId::new("source", "series"),
            seen: before.clone().unwrap_or_default(),
            before,
        }
    }

    #[test]
    fn a_first_visit_marks_nothing() {
        let visit = visit(None);
        assert!(!visit.is_new("c1"));
    }

    #[test]
    fn chapters_added_since_the_last_visit_stay_new_for_the_visit() {
        let mut visit = visit(Some(&["c1", "c2"]));
        assert!(!visit.is_new("c1"));
        assert!(visit.is_new("c3"));
        // Recording what is on screen (as each refresh does) keeps the answer
        // for this visit: c3 is still new, and a chapter arriving mid-visit is too.
        visit.seen.extend(["c1", "c2", "c3"].map(str::to_owned));
        assert!(visit.is_new("c3"));
        assert!(visit.is_new("c4"));
        // The next visit starts from everything seen.
        let next = Visit {
            series: visit.series.clone(),
            before: Some(visit.seen.clone()),
            seen: visit.seen.clone(),
        };
        assert!(!next.is_new("c3"));
    }
}

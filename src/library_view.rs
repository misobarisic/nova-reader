//! Library screen ordering, category counts and the short lines under each
//! title. Kept apart from rendering so the choices are testable.
use super::*;
use reader_domain::{PublicationStatus, ReaderState, SeriesId};

/// Library sort codes, as stored in `reader:appearance/library_sort`.
pub(super) const RECENTLY_ADDED: i32 = 0;
pub(super) const TITLE: i32 = 1;
pub(super) const UNREAD: i32 = 2;
pub(super) const LAST_READ: i32 = 3;
pub(super) const LATEST_CHAPTER: i32 = 4;

/// Order `series` for the library. Each sort has a natural direction (newest,
/// A→Z or most first); `reversed` flips it. Titles with nothing to sort by
/// (never read, no chapter dates) stay last either way.
pub(super) fn sort(
    series: &mut [Series],
    sort: i32,
    reversed: bool,
    state: &ReaderState,
    counts: &HashMap<SeriesId, (usize, usize)>,
) {
    // A key per title: `None` sorts last in both directions.
    let key = |s: &Series| -> Option<i64> {
        match sort {
            RECENTLY_ADDED => state
                .library
                .iter()
                .find(|l| l.series == s.id)
                .map(|l| l.added_at),
            UNREAD => Some(counts.get(&s.id).map_or(0, |(_, unread)| *unread) as i64),
            LAST_READ => {
                let progress = state
                    .progress
                    .iter()
                    .filter(|p| p.chapter.series == s.id)
                    .map(|p| p.last_read_at);
                let history = state
                    .history
                    .iter()
                    .filter(|h| h.chapter.series == s.id)
                    .map(|h| h.read_at);
                progress.chain(history).max()
            }
            LATEST_CHAPTER => state
                .chapters
                .iter()
                .filter(|c| c.id.series == s.id)
                .filter_map(|c| c.uploaded_at)
                .max(),
            _ => None,
        }
    };
    if sort == TITLE {
        series.sort_by_cached_key(|s| s.title.to_lowercase());
        if reversed {
            series.reverse();
        }
        return;
    }
    // Stable: equal keys keep their library order.
    series.sort_by_cached_key(|s| {
        let key = key(s);
        (
            key.is_none(),
            match key {
                // Natural order is descending; reversing makes it ascending.
                Some(value) if reversed => value,
                Some(value) => -value,
                None => 0,
            },
        )
    });
}

/// Titles per category for the chips, ignoring the search text: All, then the
/// built-in statuses, then the user's categories.
pub(super) fn category_counts(state: &ReaderState) -> Vec<i32> {
    let bucket = |entry: &reader_domain::LibraryEntry| {
        reader_domain::library_bucket(
            entry,
            &state.chapters,
            &state.progress,
            state
                .series
                .iter()
                .find(|s| s.id == entry.series)
                .map(|s| s.publication_status)
                .unwrap_or_default(),
        )
    };
    // Only entries whose series is known show up in the grid.
    let shown: Vec<_> = state
        .library
        .iter()
        .filter(|entry| state.series.iter().any(|s| s.id == entry.series))
        .collect();
    std::iter::once(shown.len())
        .chain(reader_domain::ReadingStatus::BUILTINS.iter().map(|status| {
            shown
                .iter()
                .filter(|entry| bucket(entry) == *status)
                .count()
        }))
        .chain(state.categories.iter().map(|category| {
            shown
                .iter()
                .filter(|entry| entry.categories.contains(&category.id))
                .count()
        }))
        .map(|count| count as i32)
        .collect()
}

/// The line under a grid title, and whether it is the "Completed" success line.
pub(super) fn grid_detail(
    read: usize,
    total: usize,
    publication: PublicationStatus,
) -> (String, bool) {
    if total == 0 {
        return (String::new(), false);
    }
    if read == 0 {
        return (text::chapters_label(total), false);
    }
    if read < total {
        return (
            format!(
                "{} {} {} {total}",
                text::tr("Ch."),
                read + 1,
                text::tr("of")
            ),
            false,
        );
    }
    if publication == PublicationStatus::Completed {
        (text::tr("Completed"), true)
    } else {
        (text::tr("Caught up"), false)
    }
}

/// The detail line of a list row: status, source and downloads.
pub(super) fn list_detail(status: &str, source: &str, downloaded: usize) -> String {
    let mut parts = vec![status.to_owned(), source.to_owned()];
    if downloaded > 0 {
        parts.push(format!("{downloaded} {}", text::tr("downloaded")));
    }
    parts.retain(|part| !part.is_empty());
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(id: &str) -> Series {
        Series {
            publication_status: PublicationStatus::Unknown,
            id: SeriesId::new("source", id),
            title: id.to_uppercase(),
            description: String::new(),
            authors: Vec::new(),
            genres: Vec::new(),
            cover_url: None,
            canonical_url: None,
        }
    }

    fn chapter(series: &str, n: i64, uploaded: Option<i64>) -> Chapter {
        Chapter {
            id: ChapterId::new(SeriesId::new("source", series), format!("{series}-{n}")),
            title: format!("Chapter {n}"),
            label: format!("Chapter {n}"),
            number: Some(n.to_string()),
            volume: None,
            source_order: n,
            language: None,
            scanlator: None,
            uploaded_at: uploaded,
        }
    }

    fn order(list: &[Series]) -> Vec<String> {
        list.iter().map(|s| s.id.local_id.clone()).collect()
    }

    #[test]
    fn each_sort_orders_both_ways_and_keeps_missing_keys_last() {
        let mut state = ReaderState::default();
        state.chapters = vec![
            chapter("a", 1, Some(30)),
            chapter("b", 1, Some(10)),
            chapter("b", 2, None),
            chapter("c", 1, None),
        ];
        state.progress.push(ReadingProgress {
            chapter: state.chapters[0].id.clone(),
            anchor: ResumeAnchor::cleared(),
            mode: state.preferences.mode,
            last_read_at: 5,
            completed: true,
        });
        state.history.push(reader_domain::HistoryEntry {
            chapter: state.chapters[1].id.clone(),
            read_at: 9,
        });
        let counts = series_chapter_counts(&state.chapters, &state.progress);
        let all = vec![series("c"), series("a"), series("b")];
        let run = |sort_code: i32, reversed: bool| {
            let mut list = all.clone();
            sort(&mut list, sort_code, reversed, &state, &counts);
            order(&list)
        };
        assert_eq!(run(TITLE, false), ["a", "b", "c"]);
        assert_eq!(run(TITLE, true), ["c", "b", "a"]);
        // b has 2 unread, c 1, a 0.
        assert_eq!(run(UNREAD, false), ["b", "c", "a"]);
        assert_eq!(run(UNREAD, true), ["a", "c", "b"]);
        // Never read (c) stays last either way.
        assert_eq!(run(LAST_READ, false), ["b", "a", "c"]);
        assert_eq!(run(LAST_READ, true), ["a", "b", "c"]);
        // No dated chapter (c) stays last either way.
        assert_eq!(run(LATEST_CHAPTER, false), ["a", "b", "c"]);
        assert_eq!(run(LATEST_CHAPTER, true), ["b", "a", "c"]);
    }

    #[test]
    fn detail_lines_describe_reading_state() {
        assert_eq!(
            grid_detail(4, 12, PublicationStatus::Ongoing),
            ("Ch. 5 of 12".into(), false)
        );
        assert_eq!(
            grid_detail(12, 12, PublicationStatus::Ongoing),
            ("Caught up".into(), false)
        );
        assert_eq!(
            grid_detail(12, 12, PublicationStatus::Completed),
            ("Completed".into(), true)
        );
        assert_eq!(
            grid_detail(0, 94, PublicationStatus::Ongoing),
            ("94 chapters".into(), false)
        );
        assert_eq!(
            grid_detail(0, 0, PublicationStatus::Ongoing),
            (String::new(), false)
        );
        assert_eq!(
            list_detail("Reading", "mangadex", 3),
            "Reading · mangadex · 3 downloaded"
        );
        assert_eq!(list_detail("Reading", "mangadex", 0), "Reading · mangadex");
    }
}

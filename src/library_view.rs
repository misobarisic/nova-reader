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
    // One pass over the big lists instead of a scan per title.
    let mut added: HashMap<&SeriesId, i64> = HashMap::new();
    let mut latest: HashMap<&SeriesId, i64> = HashMap::new();
    match sort {
        RECENTLY_ADDED => {
            for l in &state.library {
                added.entry(&l.series).or_insert(l.added_at);
            }
        }
        LAST_READ => {
            let progress = state
                .progress
                .iter()
                .map(|p| (&p.chapter.series, p.last_read_at));
            let history = state.history.iter().map(|h| (&h.chapter.series, h.read_at));
            for (series, at) in progress.chain(history) {
                let slot = latest.entry(series).or_insert(at);
                *slot = (*slot).max(at);
            }
        }
        LATEST_CHAPTER => {
            for c in &state.chapters {
                if let Some(at) = c.uploaded_at {
                    let slot = latest.entry(&c.id.series).or_insert(at);
                    *slot = (*slot).max(at);
                }
            }
        }
        _ => {}
    }
    // A key per title: `None` sorts last in both directions.
    let key = |s: &Series| -> Option<i64> {
        match sort {
            RECENTLY_ADDED => added.get(&s.id).copied(),
            UNREAD => Some(counts.get(&s.id).map_or(0, |(_, unread)| *unread) as i64),
            LAST_READ | LATEST_CHAPTER => latest.get(&s.id).copied(),
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

/// Every library entry's status bucket, computed in one pass. Equivalent to
/// `reader_domain::library_bucket` per entry (which rescans all chapters and
/// progress each call), from the per-series `(total, unread)` counts.
pub(super) fn buckets(
    state: &ReaderState,
    counts: &HashMap<SeriesId, (usize, usize)>,
) -> HashMap<SeriesId, reader_domain::ReadingStatus> {
    use reader_domain::ReadingStatus;
    let publication: HashMap<&SeriesId, PublicationStatus> = state
        .series
        .iter()
        .map(|s| (&s.id, s.publication_status))
        .collect();
    let started: HashSet<&SeriesId> = state.progress.iter().map(|p| &p.chapter.series).collect();
    state
        .library
        .iter()
        .map(|entry| {
            let bucket = if entry.reading_status != ReadingStatus::Auto {
                entry.reading_status
            } else {
                let (total, unread) = counts.get(&entry.series).copied().unwrap_or_default();
                let publication = publication.get(&entry.series).copied().unwrap_or_default();
                if publication == PublicationStatus::Completed && total > 0 && unread == 0 {
                    ReadingStatus::Completed
                } else if started.contains(&entry.series) {
                    ReadingStatus::Reading
                } else {
                    ReadingStatus::PlanToRead
                }
            };
            (entry.series.clone(), bucket)
        })
        .collect()
}

/// Titles per category for the chips, ignoring the search text: All, then the
/// built-in statuses, then the user's categories.
pub(super) fn category_counts(
    state: &ReaderState,
    buckets: &HashMap<SeriesId, reader_domain::ReadingStatus>,
) -> Vec<i32> {
    // Only entries whose series is known show up in the grid.
    let known: HashSet<&SeriesId> = state.series.iter().map(|s| &s.id).collect();
    let shown: Vec<_> = state
        .library
        .iter()
        .filter(|entry| known.contains(&entry.series))
        .collect();
    std::iter::once(shown.len())
        .chain(reader_domain::ReadingStatus::BUILTINS.iter().map(|status| {
            shown
                .iter()
                .filter(|entry| buckets.get(&entry.series) == Some(status))
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

/// Everything a library or catalogue card shows except its poster, as plain
/// data. `slint::Image` stays on the UI thread, so workers build these and the
/// event loop turns them into `ItemRow`s with the cached cover.
pub(super) struct SeriesFields {
    series_key: String,
    title: String,
    subtitle: String,
    badge: String,
    bookmarked: bool,
    new_count: i32,
    downloaded_count: i32,
    detail: String,
    detail_success: bool,
    list_detail: String,
    artwork_key: String,
    completed: i32,
    total: i32,
}

impl SeriesFields {
    pub(super) fn into_row(self, poster: slint::Image) -> ItemRow {
        ItemRow {
            series_key: self.series_key.into(),
            title: self.title.into(),
            subtitle: self.subtitle.into(),
            badge: self.badge.into(),
            bookmarked: self.bookmarked,
            new_count: self.new_count,
            downloaded_count: self.downloaded_count,
            detail: self.detail.into(),
            detail_success: self.detail_success,
            list_detail: self.list_detail.into(),
            poster,
            artwork_key: self.artwork_key.into(),
            completed: self.completed,
            total: self.total,
            ..ItemRow::default()
        }
    }
}

/// The per-render indexes a card is built from.
pub(super) struct SeriesContext<'a> {
    pub section: i32,
    pub counts: &'a HashMap<SeriesId, (usize, usize)>,
    pub buckets: &'a HashMap<SeriesId, reader_domain::ReadingStatus>,
    pub library: &'a HashMap<&'a SeriesId, &'a reader_domain::LibraryEntry>,
    pub downloaded: &'a HashMap<&'a SeriesId, usize>,
    pub chapter_ids: &'a HashMap<&'a SeriesId, Vec<&'a str>>,
}

pub(super) fn series_fields(ctx: &SeriesContext, s: &Series) -> SeriesFields {
    let gallery = crate::content_presentation::Settings::load(&s.id)
        .unwrap_or_default()
        .effective()
        == crate::content_presentation::Presentation::Gallery;
    let (total, unread) = ctx.counts.get(&s.id).copied().unwrap_or_default();
    let downloaded = ctx.downloaded.get(&s.id).copied().unwrap_or(0);
    let bucket = ctx.buckets.get(&s.id).copied();
    let status_label = bucket
        .map(|bucket| text::tr(bucket.label()))
        .unwrap_or_default();
    let library = ctx.section == 0;
    let grid = grid_detail(total - unread, total, s.publication_status);
    SeriesFields {
        series_key: serde_json::to_string(&s.id).unwrap_or_default(),
        title: s.title.clone(),
        subtitle: bucket
            .map(|bucket| {
                format!(
                    "{} · {}",
                    text::tr(bucket.label()),
                    text::chapter_count(unread, total)
                )
            })
            .unwrap_or_else(|| text::chapter_count(unread, total)),
        badge: s.id.source_id.clone(),
        bookmarked: ctx.library.contains_key(&s.id),
        new_count: if library {
            crate::chapter_news::unseen_count(
                &s.id,
                ctx.chapter_ids.get(&s.id).into_iter().flatten().copied(),
            ) as i32
        } else {
            0
        },
        downloaded_count: if library { downloaded as i32 } else { 0 },
        detail: if library && !gallery {
            grid.0.clone()
        } else if ctx.section == 3 {
            browse_page::catalogue_detail(s.publication_status, total)
        } else {
            String::new()
        },
        detail_success: library && !gallery && grid.1,
        list_detail: if library {
            list_detail(&status_label, &s.id.source_id, downloaded)
        } else if ctx.section == 3 {
            browse_page::catalogue_detail(s.publication_status, total)
        } else {
            String::new()
        },
        artwork_key: crate::artwork::key(s),
        // A last-viewed gallery image does not imply that all preceding
        // images were viewed. Keep manual status intact.
        completed: if gallery { 0 } else { (total - unread) as i32 },
        total: if gallery { 0 } else { total as i32 },
    }
}

/// The built-in status and the user category a chip index selects (0 is All,
/// then the built-in statuses, then the user's categories).
pub(super) fn filter_for(
    category: usize,
    categories: &[reader_domain::Category],
) -> (Option<String>, Option<reader_domain::ReadingStatus>) {
    let builtins = reader_domain::ReadingStatus::BUILTINS;
    let id = category
        .checked_sub(builtins.len() + 1)
        .and_then(|index| categories.get(index))
        .map(|category| category.id.clone());
    let status = category
        .checked_sub(1)
        .and_then(|index| builtins.get(index))
        .copied();
    (id, status)
}

/// A filtered, sorted Library with its card fields, built off the UI thread.
pub(super) struct Projection {
    pub rows: Vec<Series>,
    pub items: Vec<SeriesFields>,
}

pub(super) fn project(
    snapshot: &ReaderState,
    query: &str,
    category_id: Option<&str>,
    status: Option<reader_domain::ReadingStatus>,
    sort_code: i32,
    reversed: bool,
    download_states: &BTreeMap<ChapterId, reader_downloads::JobStatus>,
) -> Projection {
    let counts = series_chapter_counts(&snapshot.chapters, &snapshot.progress);
    let buckets = buckets(snapshot, &counts);
    let series_by_id: HashMap<&SeriesId, &Series> =
        snapshot.series.iter().map(|s| (&s.id, s)).collect();
    let library_by_id: HashMap<&SeriesId, &reader_domain::LibraryEntry> =
        snapshot.library.iter().map(|l| (&l.series, l)).collect();
    let mut rows: Vec<Series> = snapshot
        .library
        .iter()
        .filter(|l| {
            category_id.is_none_or(|id| l.categories.contains(id))
                && status.is_none_or(|status| buckets.get(&l.series) == Some(&status))
        })
        .filter_map(|l| series_by_id.get(&l.series).copied())
        .filter(|s| query.is_empty() || s.title.to_lowercase().contains(query))
        .cloned()
        .collect();
    sort(&mut rows, sort_code, reversed, snapshot, &counts);
    let mut downloaded: HashMap<&SeriesId, usize> = HashMap::new();
    for (chapter, status) in download_states {
        if *status == reader_downloads::JobStatus::Complete {
            *downloaded.entry(&chapter.series).or_default() += 1;
        }
    }
    let mut chapter_ids: HashMap<&SeriesId, Vec<&str>> = HashMap::new();
    for chapter in &snapshot.chapters {
        chapter_ids
            .entry(&chapter.id.series)
            .or_default()
            .push(chapter.id.local_id.as_str());
    }
    let context = SeriesContext {
        section: 0,
        counts: &counts,
        buckets: &buckets,
        library: &library_by_id,
        downloaded: &downloaded,
        chapter_ids: &chapter_ids,
    };
    let items = rows.iter().map(|s| series_fields(&context, s)).collect();
    Projection { rows, items }
}

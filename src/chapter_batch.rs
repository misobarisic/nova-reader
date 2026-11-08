//! Chapter actions over several chapters at once, chosen in natural reading
//! order (never the list's display order, which the user can reverse): mark
//! earlier chapters read, and pick which chapters to download next.
use super::*;
use reader_downloads::JobStatus;

/// A series' chapters in ascending reading order.
pub(super) fn ordered<'a>(
    state: &'a reader_domain::ReaderState,
    series: &reader_domain::SeriesId,
) -> Vec<&'a Chapter> {
    let mut chapters: Vec<_> = state
        .chapters
        .iter()
        .filter(|chapter| &chapter.id.series == series)
        .collect();
    chapters.sort_by(|a, b| reader_open::reading_order(a, b));
    chapters
}

/// Every chapter before `chapter` in reading order.
pub(super) fn before(state: &reader_domain::ReaderState, chapter: &ChapterId) -> Vec<ChapterId> {
    ordered(state, &chapter.series)
        .into_iter()
        .take_while(|c| &c.id != chapter)
        .map(|c| c.id.clone())
        .collect()
}

/// Mark chapters read as an explicit read-state edit (no saved page), leaving
/// chapters that are already read untouched. Returns the newly read ones.
pub(super) fn mark_read(
    state: &mut reader_domain::ReaderState,
    chapters: &[ChapterId],
    at: i64,
) -> Vec<ChapterId> {
    let mut newly_read = Vec::new();
    for chapter in chapters {
        if state
            .progress
            .iter()
            .any(|p| &p.chapter == chapter && p.completed)
        {
            continue;
        }
        state.progress.retain(|p| &p.chapter != chapter);
        state.progress.push(ReadingProgress {
            chapter: chapter.clone(),
            anchor: ResumeAnchor::cleared(),
            mode: state.preferences.mode,
            last_read_at: at,
            completed: true,
        });
        newly_read.push(chapter.clone());
    }
    newly_read
}

/// Whether a chapter still needs downloading: nothing on disk and nothing
/// already waiting or transferring. Failed and cancelled jobs can be retried.
fn wants_download(jobs: &BTreeMap<ChapterId, JobStatus>, chapter: &ChapterId) -> bool {
    !matches!(
        jobs.get(chapter),
        Some(JobStatus::Complete | JobStatus::Queued | JobStatus::Running | JobStatus::Paused)
    )
}

fn is_read(state: &reader_domain::ReaderState, chapter: &ChapterId) -> bool {
    state
        .progress
        .iter()
        .any(|p| &p.chapter == chapter && p.completed)
}

/// Which chapters to download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pick {
    /// From this chapter on, up to `n` that still need downloading.
    From(usize),
    /// The first `n` unread chapters that still need downloading.
    NextUnread(usize),
    /// Every unread chapter that still needs downloading.
    Unread,
    /// Every chapter that still needs downloading.
    All,
}

pub(super) fn to_download(
    state: &reader_domain::ReaderState,
    series: &reader_domain::SeriesId,
    start: Option<&ChapterId>,
    pick: Pick,
    jobs: &BTreeMap<ChapterId, JobStatus>,
) -> Vec<ChapterId> {
    let chapters = ordered(state, series);
    let from = start
        .and_then(|start| chapters.iter().position(|c| &c.id == start))
        .unwrap_or(0);
    let candidates = chapters[from..]
        .iter()
        .map(|c| &c.id)
        .filter(|id| id.series.source_id != "local" && wants_download(jobs, id));
    match pick {
        Pick::From(n) => candidates.take(n).cloned().collect(),
        Pick::NextUnread(n) => candidates
            .filter(|id| !is_read(state, id))
            .take(n)
            .cloned()
            .collect(),
        Pick::Unread => candidates.filter(|id| !is_read(state, id)).cloned().collect(),
        Pick::All => candidates.cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (reader_domain::ReaderState, reader_domain::SeriesId) {
        let series = reader_domain::SeriesId::new("source", "series");
        let mut state = reader_domain::ReaderState::default();
        // Listed newest first, as many sources do; reading order follows numbers.
        for number in (1..=8).rev() {
            state.chapters.push(Chapter {
                id: ChapterId::new(series.clone(), format!("c{number}")),
                title: format!("Chapter {number}"),
                label: format!("Chapter {number}"),
                number: Some(number.to_string()),
                volume: None,
                source_order: 8 - number,
                language: None,
                scanlator: None,
                uploaded_at: None,
            });
        }
        (state, series)
    }

    fn id(series: &reader_domain::SeriesId, number: i64) -> ChapterId {
        ChapterId::new(series.clone(), format!("c{number}"))
    }

    #[test]
    fn earlier_chapters_follow_reading_order_not_listing_order() {
        let (mut state, series) = state();
        let earlier = before(&state, &id(&series, 4));
        assert_eq!(earlier, (1..=3).map(|n| id(&series, n)).collect::<Vec<_>>());
        // Chapter 2 is already read: it keeps its existing record.
        let newly = mark_read(&mut state, &[id(&series, 2)], 10);
        assert_eq!(newly, vec![id(&series, 2)]);
        let newly = mark_read(&mut state, &earlier, 20);
        assert_eq!(newly, vec![id(&series, 1), id(&series, 3)]);
        assert!(state.progress.iter().all(|p| p.completed));
        assert_eq!(
            state
                .progress
                .iter()
                .find(|p| p.chapter == id(&series, 2))
                .unwrap()
                .last_read_at,
            10
        );
    }

    #[test]
    fn download_picks_skip_chapters_on_disk_or_already_queued() {
        let (mut state, series) = state();
        let jobs = BTreeMap::from([
            (id(&series, 4), JobStatus::Complete),
            (id(&series, 5), JobStatus::Queued),
            (id(&series, 6), JobStatus::Failed),
        ]);
        assert_eq!(
            to_download(&state, &series, Some(&id(&series, 3)), Pick::From(5), &jobs),
            [3, 6, 7, 8].map(|n| id(&series, n)).to_vec()
        );
        mark_read(&mut state, &[id(&series, 1), id(&series, 2)], 1);
        assert_eq!(
            to_download(&state, &series, None, Pick::NextUnread(2), &jobs),
            [3, 6].map(|n| id(&series, n)).to_vec()
        );
        assert_eq!(
            to_download(&state, &series, None, Pick::Unread, &jobs),
            [3, 6, 7, 8].map(|n| id(&series, n)).to_vec()
        );
        assert_eq!(
            to_download(&state, &series, None, Pick::All, &jobs),
            [1, 2, 3, 6, 7, 8].map(|n| id(&series, n)).to_vec()
        );
    }
}

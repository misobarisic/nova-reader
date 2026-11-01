//! Foreground chapter inventory refresh, adapted from Nova's
//! src/app/catalog.rs::reserve_saved_episode_refresh and run_saved_episode_refresh.
//! Keep the same small batches, recent/old cadence and starvation protection.
use crate::services::{Services, now};
use reader_domain::{Chapter, ReaderState, ReadingStatus, Series, SeriesId};
use reader_sources::CancelToken;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const BATCH: usize = 4;
const BATCH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const RECENT_INTERVAL: i64 = 30 * 60;
const OLD_INTERVAL: i64 = 6 * 3600;
const FAILURE_INTERVAL: i64 = 2 * 60;
const DEADLINE_PREFIX: &str = "reader:metadata-refresh:";
static FOREGROUND: AtomicBool = AtomicBool::new(true);
static DEADLINE_WRITES: Mutex<()> = Mutex::new(());

#[cfg(target_os = "android")]
pub(crate) fn set_foreground(active: bool) {
    FOREGROUND.store(active, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::{ChapterId, LibraryEntry, ReadingMode, ReadingProgress, ResumeAnchor};

    fn fixture(id: &str, last_read: i64) -> ReaderState {
        let id = SeriesId::new("refresh-fixture", id);
        let series = Series {
            publication_status: Default::default(),
            id: id.clone(),
            title: "Cached title".into(),
            description: "Cached".into(),
            authors: vec![],
            genres: vec![],
            cover_url: None,
            canonical_url: None,
        };
        let chapter = Chapter {
            id: ChapterId::new(id.clone(), "1"),
            title: "Chapter 1".into(),
            label: "1".into(),
            number: Some("1".into()),
            volume: None,
            source_order: 0,
            language: None,
            scanlator: None,
            uploaded_at: None,
        };
        let mut state = ReaderState::default();
        state.library.push(LibraryEntry {
            series: id,
            categories: Default::default(),
            preferences: None,
            bookmarked: false,
            added_at: last_read,
            reading_status: ReadingStatus::Auto,
        });
        state.series.push(series);
        state.chapters.push(chapter.clone());
        state.set_progress(ReadingProgress {
            chapter: chapter.id,
            anchor: ResumeAnchor::new("0", 0, 0.5).unwrap(),
            mode: ReadingMode::Webtoon,
            last_read_at: last_read,
            completed: true,
        });
        state
    }

    #[test]
    fn nova_cadence_reserves_bounded_fair_batches_and_never_confuses_caught_up_with_ended() {
        let wall_now = 100 * 24 * 3600;
        let mut state = ReaderState::default();
        for i in 0..12 {
            let mut one = fixture(&format!("{i:02}"), if i < 6 { wall_now - 60 } else { 1 });
            state.series.append(&mut one.series);
            state.library.append(&mut one.library);
            state.chapters.append(&mut one.chapters);
            state.progress.append(&mut one.progress);
        }
        let overdue = BTreeMap::from([
            (state.library[0].series.clone(), wall_now - 1),
            (state.library[1].series.clone(), wall_now - 2),
        ]);
        let work = plan(&state, &overdue, wall_now);
        assert_eq!(work.len(), BATCH);
        assert!(work.iter().take(2).all(|job| overdue.contains_key(&job.id)));
        // Every known chapter is read, but these titles remain eligible.
        assert_eq!(work[0].interval, RECENT_INTERVAL);
        assert_eq!(
            interval(&state, &state.library[10].series, wall_now),
            Some(OLD_INTERVAL)
        );
        let future = state
            .library
            .iter()
            .map(|entry| (entry.series.clone(), wall_now + RECENT_INTERVAL))
            .collect();
        assert!(plan(&state, &future, wall_now).is_empty());
        assert_eq!(plan(&state, &future, wall_now + OLD_INTERVAL).len(), BATCH);
        let mut visited = BTreeMap::new();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..3 {
            for job in plan(&state, &visited, wall_now) {
                seen.insert(job.id.clone());
                visited.insert(job.id, wall_now + job.interval);
            }
        }
        assert_eq!(
            seen.len(),
            state.library.len(),
            "large old libraries get their turn"
        );
        for pin in ReadingStatus::BUILTINS {
            state.library[0].reading_status = pin;
            assert_eq!(
                interval(&state, &state.library[0].series, wall_now).is_some(),
                pin == ReadingStatus::Reading
            );
        }
        state.library[0].reading_status = ReadingStatus::Reading;
        state.library[0].series.source_id = "local".into();
        assert!(interval(&state, &state.library[0].series, wall_now).is_none());
        for section in 0..15 {
            assert_eq!(surface_allowed(section), section < 3);
        }
    }

    #[test]
    fn refresh_rejects_empty_superseded_removed_or_pinned_targets_and_preserves_user_state() {
        let baseline = fixture("same", 1);
        let series = baseline.series[0].clone();
        let chapters = baseline.chapters.clone();
        let mut fresh = series.clone();
        fresh.title = "Updated title".into();
        let mut new = chapters[0].clone();
        new.id.local_id = "2".into();
        let mut state = baseline.clone();
        assert!(!publish_if_current(
            &mut state,
            &series,
            &chapters,
            fresh.clone(),
            vec![],
            2
        ));
        assert_eq!(state, baseline);
        state.series[0].title = "Newer foreground result".into();
        assert!(!publish_if_current(
            &mut state,
            &series,
            &chapters,
            fresh.clone(),
            vec![new.clone()],
            2
        ));
        state = baseline.clone();
        state.chapters.push(new.clone());
        assert!(!publish_if_current(
            &mut state,
            &series,
            &chapters,
            fresh.clone(),
            vec![new.clone()],
            2
        ));
        state = baseline.clone();
        state.library.clear();
        assert!(!publish_if_current(
            &mut state,
            &series,
            &chapters,
            fresh.clone(),
            vec![new.clone()],
            2
        ));
        state = baseline.clone();
        state.library[0].reading_status = ReadingStatus::OnHold;
        assert!(!publish_if_current(
            &mut state,
            &series,
            &chapters,
            fresh.clone(),
            vec![new.clone()],
            2
        ));
        state = baseline.clone();
        // Progress and library status remain untouched by successful publication.
        assert!(publish_if_current(
            &mut state,
            &series,
            &chapters,
            fresh,
            vec![new],
            2
        ));
        assert_eq!(state.progress, baseline.progress);
        assert_eq!(state.history, baseline.history);
        assert_eq!(state.library, baseline.library);
        assert_eq!(
            state.chapters.len(),
            2,
            "vanished chapters remain available offline"
        );
    }

    #[test]
    fn actual_source_refresh_persists_deadlines_across_restarts_and_pauses_cancel_work() {
        const MODE: &str = "READER_METADATA_REFRESH_CHILD";
        if std::env::var_os(MODE).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "metadata_refresh::tests::actual_source_refresh_persists_deadlines_across_restarts_and_pauses_cancel_work"])
                .env(MODE, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let services = Services::open(directory.path()).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../examples/addons/neutral-fixture/manifest.json"
        ))
        .unwrap();
        manifest["id"] = serde_json::json!("refresh-fixture");
        manifest["assets"] = serde_json::json!([]);
        let script = r#"globalThis.novaProvider = {handle(r) {
          if (r.op === 'details') return {id:'same', title:'Fresh title', status:'completed'};
          if (r.op === 'chapters') return {chapters:[{id:'2',seriesId:'same',title:'New chapter',sourceOrder:1}]};
          return {sources:[]};
        }};"#;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (path, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("main.js", script.as_bytes().to_vec()),
        ] {
            zip.start_file(path, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        services
            .install(&zip.finish().unwrap().into_inner())
            .unwrap();
        let state = fixture("same", now() - 60);
        services
            .domain
            .update(|current| {
                *current = state.clone();
                Ok(())
            })
            .unwrap();
        let scheduler = Scheduler::default();
        scheduler.allowed.store(true, Ordering::Release);
        assert!(scheduler.run(&services, &CancelToken::default()));
        let saved = services.domain.snapshot().unwrap();
        assert_eq!(saved.series[0].title, "Fresh title");
        assert_eq!(
            saved.series[0].publication_status,
            reader_domain::PublicationStatus::Completed
        );
        assert_eq!(saved.chapters.len(), 2);
        assert_eq!(saved.progress, state.progress);
        assert_eq!(saved.history, state.history);
        let deadline = deadline(&state.series[0].id).unwrap();
        assert!(deadline >= now() + RECENT_INTERVAL - 2);
        let batch_deadline = *scheduler.next_batch.lock().unwrap();
        scheduler.pause();
        assert_eq!(
            *scheduler.next_batch.lock().unwrap(),
            batch_deadline,
            "rapid page changes cannot bypass the five-minute batch limit"
        );
        let restarted = Scheduler::default();
        restarted.allowed.store(true, Ordering::Release);
        assert!(!restarted.run(&services, &CancelToken::default()));
        defer(&state.series[0].id, 0);
        let cancel = CancelToken::default();
        *restarted.cancel.lock().unwrap() = cancel.clone();
        restarted.pause();
        assert!(!restarted.run(&services, &cancel));
        assert_eq!(services.domain.snapshot().unwrap(), saved);
        let unrelated = SeriesId::new("another-source", "same");
        assert!(super::deadline(&unrelated).is_none());
        note_manual_success(&services, &state.series[0].id);
        assert!(super::deadline(&state.series[0].id).unwrap() > now());
    }
}
pub(crate) fn foreground() -> bool {
    FOREGROUND.load(Ordering::Acquire)
}
pub(crate) fn surface_allowed(section: i32) -> bool {
    matches!(section, 0..=2)
}
fn interval(state: &ReaderState, id: &SeriesId, wall_now: i64) -> Option<i64> {
    // Imported folders/CBZs have no remote metadata provider.
    if id.source_id == "local" {
        return None;
    }
    let entry = state.library.iter().find(|entry| &entry.series == id)?;
    if !matches!(
        entry.reading_status,
        ReadingStatus::Auto | ReadingStatus::Reading
    ) {
        return None;
    }
    let last_read = state
        .progress
        .iter()
        .filter(|p| &p.chapter.series == id)
        .map(|p| p.last_read_at)
        .max()
        .or_else(|| (entry.reading_status == ReadingStatus::Reading).then_some(entry.added_at))?;
    Some(if wall_now.saturating_sub(last_read) < 30 * 24 * 3600 {
        RECENT_INTERVAL
    } else {
        OLD_INTERVAL
    })
}
fn key(id: &SeriesId) -> String {
    format!(
        "{DEADLINE_PREFIX}{}",
        serde_json::to_string(id).expect("series identity")
    )
}
fn deadline(id: &SeriesId) -> Option<i64> {
    nova_storage::try_get_str(&key(id))
        .ok()
        .flatten()?
        .parse()
        .ok()
}
fn defer(id: &SeriesId, until: i64) {
    let _guard = DEADLINE_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(error) = nova_storage::try_set_str(&key(id), &until.to_string()) {
        eprintln!("Nova Reader: could not save metadata refresh deadline: {error}");
    }
}
fn reserve_retry(id: &SeriesId, wall_now: i64) -> bool {
    // A manual refresh may have completed since the queue was planned. Never
    // shorten its cooldown with this older request's retry reservation.
    let _guard = DEADLINE_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    if deadline(id).is_some_and(|at| at > wall_now && at.saturating_sub(wall_now) <= OLD_INTERVAL) {
        return false;
    }
    if let Err(error) = nova_storage::try_set_str(
        &key(id),
        &wall_now.saturating_add(FAILURE_INTERVAL).to_string(),
    ) {
        eprintln!("Nova Reader: could not reserve metadata refresh: {error}");
        return false;
    }
    true
}
pub(crate) fn note_manual_success(services: &Services, id: &SeriesId) {
    let wall_now = now();
    if let Ok(state) = services.domain.snapshot()
        && let Some(interval) = interval(&state, id, wall_now)
    {
        defer(id, wall_now.saturating_add(interval));
    }
}
#[derive(Debug)]
struct Job {
    id: SeriesId,
    interval: i64,
}

fn plan(state: &ReaderState, deadlines: &BTreeMap<SeriesId, i64>, wall_now: i64) -> Vec<Job> {
    let mut candidates = state
        .library
        .iter()
        .filter_map(|entry| {
            let interval = interval(state, &entry.series, wall_now)?;
            let previous = deadlines.get(&entry.series).copied();
            // Do not let a clock correction or corrupt future stamp freeze refresh.
            if previous
                .is_some_and(|at| at > wall_now && at.saturating_sub(wall_now) <= OLD_INTERVAL)
            {
                return None;
            }
            let last_read = state
                .progress
                .iter()
                .filter(|p| p.chapter.series == entry.series)
                .map(|p| p.last_read_at)
                .max()
                .unwrap_or(entry.added_at);
            Some((
                previous,
                std::cmp::Reverse(last_read),
                Job {
                    id: entry.series.clone(),
                    interval,
                },
            ))
        })
        .collect::<Vec<_>>();
    // Copied from Nova: reserve half the budget for overdue recently read
    // titles; rotate the rest through unvisited/oldest deadlines.
    candidates.sort_by_key(|(deadline, read, job)| (*deadline, *read, job.id.clone()));
    let mut urgent = Vec::new();
    let mut index = 0;
    while index < candidates.len() && urgent.len() < BATCH / 2 {
        if candidates[index].0.is_some() && candidates[index].2.interval == RECENT_INTERVAL {
            urgent.push(candidates.remove(index));
        } else {
            index += 1;
        }
    }
    urgent.extend(candidates);
    urgent
        .into_iter()
        .take(BATCH)
        .map(|(_, _, job)| job)
        .collect()
}

fn publish_if_current(
    state: &mut ReaderState,
    baseline: &Series,
    chapters: &[Chapter],
    fresh: Series,
    fresh_chapters: Vec<Chapter>,
    wall_now: i64,
) -> bool {
    if fresh_chapters.is_empty()
        || fresh.id != baseline.id
        || interval(state, &baseline.id, wall_now).is_none()
        || state.series.iter().find(|saved| saved.id == baseline.id) != Some(baseline)
        || state
            .chapters
            .iter()
            .filter(|c| c.id.series == baseline.id)
            .ne(chapters.iter())
    {
        return false;
    }
    crate::services::merge_details(state, fresh, fresh_chapters);
    true
}

#[derive(Default)]
pub(crate) struct Scheduler {
    busy: AtomicBool,
    allowed: AtomicBool,
    next_batch: Mutex<Option<Instant>>,
    cancel: Mutex<CancelToken>,
}
impl Scheduler {
    pub(crate) fn pause(&self) {
        self.allowed.store(false, Ordering::Release);
        self.cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel();
        // Preserve batch spacing across rapid tab changes and reader visits.
    }
    pub(crate) fn tick(
        self: &Arc<Self>,
        services: Services,
        complete: impl FnOnce() + Send + 'static,
    ) {
        self.allowed.store(true, Ordering::Release);
        if !foreground()
            || self
                .next_batch
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|at| Instant::now() < at)
            || self.busy.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let cancel = CancelToken::default();
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) = cancel.clone();
        let scheduler = self.clone();
        std::thread::spawn(move || {
            let changed = scheduler.run(&services, &cancel);
            scheduler.busy.store(false, Ordering::Release);
            if changed {
                complete();
            }
        });
    }
    fn current(&self, cancel: &CancelToken) -> bool {
        foreground() && self.allowed.load(Ordering::Acquire) && cancel.check().is_ok()
    }
    fn run(&self, services: &Services, cancel: &CancelToken) -> bool {
        if !self.current(cancel) {
            return false;
        }
        let Ok(state) = services.domain.snapshot() else {
            return false;
        };
        let deadlines = state
            .library
            .iter()
            .filter_map(|entry| deadline(&entry.series).map(|at| (entry.series.clone(), at)))
            .collect();
        let work = plan(&state, &deadlines, now());
        if !work.is_empty() {
            *self.next_batch.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Instant::now() + BATCH_INTERVAL);
        }
        let mut changed = false;
        for job in work {
            if !self.current(cancel) {
                break;
            }
            // A manual refresh can finish while this bounded queue is waiting.
            if deadline(&job.id)
                .is_some_and(|at| at > now() && at.saturating_sub(now()) <= OLD_INTERVAL)
            {
                continue;
            }
            let Ok(baseline) = services.domain.snapshot() else {
                continue;
            };
            if interval(&baseline, &job.id, now()).is_none() {
                continue;
            }
            let Some(series) = baseline.series.iter().find(|s| s.id == job.id) else {
                continue;
            };
            if !self.current(cancel) || !reserve_retry(&job.id, now()) {
                continue;
            }
            let Ok(source) = services.source(&job.id.source_id) else {
                continue;
            };
            let owner = source.availability();
            let chapters: Vec<_> = baseline
                .chapters
                .iter()
                .filter(|c| c.id.series == job.id)
                .cloned()
                .collect();
            // The retry was persisted before work, so restart/failure cannot
            // hammer a source. Source availability guards revoke old installs.
            let Ok((fresh, fresh_chapters)) = services.fetch_details_from(&source, series, cancel)
            else {
                continue;
            };
            let mut published = false;
            let result = services.domain.update(|state| {
                if self.current(cancel) && owner.is_available() {
                    published =
                        publish_if_current(state, series, &chapters, fresh, fresh_chapters, now());
                }
                Ok(())
            });
            if result.is_ok() && published {
                changed = true;
                defer(&job.id, now().saturating_add(job.interval));
            }
        }
        if changed {
            let _ = nova_storage::try_set_str("reader:last-update", &now().to_string());
        }
        changed
    }
}

//! Download presentation and stable-identity actions. Poll manifests, never pages.
use super::*;
use crate::DownloadEntry;
use reader_domain::{ReaderState, SeriesId};
use reader_downloads::{ChapterDownload, JobStatus};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct DownloadView {
    pub jobs: Vec<ChapterDownload>,
    pub catalogue: Option<ReaderState>,
    pub pending: BTreeMap<ChapterId, Option<JobStatus>>,
    pub prepare_errors: BTreeMap<ChapterId, String>,
    pub selected: BTreeSet<ChapterId>,
    pub expanded: BTreeSet<SeriesId>,
    pub eligible: Vec<ChapterId>,
    initialized: bool,
    samples: BTreeMap<ChapterId, (u64, Instant, String)>,
    /// Delete awaiting the reader's confirmation: (row key, action).
    pub confirm: Option<(String, String)>,
    pub busy: bool,
}
fn key<T: serde::Serialize>(id: &T) -> String {
    serde_json::to_string(id).expect("download identities serialize")
}
pub(super) fn state(status: &JobStatus) -> i32 {
    match status {
        JobStatus::Complete => 0,
        JobStatus::Queued => 1,
        JobStatus::Running => 2,
        JobStatus::Paused => 3,
        JobStatus::Failed => 4,
        JobStatus::Cancelled => 5,
    }
}
fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MiB", bytes as f64 / (1u64 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}
fn model(ui: &AppWindow, rows: Vec<DownloadEntry>, queue: bool) {
    let current = if queue {
        ui.get_download_queue()
    } else {
        ui.get_downloaded_rows()
    };
    if current.row_count() == rows.len() && current.iter().zip(&rows).all(|(a, b)| a.key == b.key) {
        for (index, row) in rows.into_iter().enumerate() {
            if current.row_data(index).as_ref() != Some(&row) {
                current.set_row_data(index, row);
            }
        }
    } else {
        let rows = ModelRc::new(VecModel::from(rows));
        if queue {
            ui.set_download_queue(rows);
        } else {
            ui.set_downloaded_rows(rows);
        }
    }
}
impl DownloadView {
    /// Keep only selections still listed under the current search/filter.
    pub fn trim_selection(selected: &mut BTreeSet<ChapterId>, eligible: &[ChapterId]) {
        selected.retain(|id| eligible.contains(id));
    }
    /// Cancelling a row whose metadata preparation failed has no job to
    /// cancel; dropping its bookkeeping is what dismisses it.
    pub fn dismiss_prepare_failure(&mut self, id: &ChapterId) -> bool {
        let failed = self.prepare_errors.remove(id).is_some();
        if failed {
            self.pending.remove(id);
        }
        failed
    }
    /// Resume on a row still being prepared clears its queued pause so the
    /// worker leaves it queued instead of pausing it after preparation.
    pub fn resume_pending(&mut self, id: &ChapterId) -> bool {
        match self.pending.get_mut(id) {
            Some(desired) => {
                *desired = None;
                true
            }
            None => false,
        }
    }
}
/// "Resume all" revives paused and failed jobs; a cancelled job was dismissed
/// on purpose and stays cancelled until the reader retries it individually.
fn resume_all_targets(jobs: &[ChapterDownload]) -> Vec<ChapterId> {
    jobs.iter()
        .filter(|j| matches!(j.status, JobStatus::Paused | JobStatus::Failed))
        .map(|j| j.chapter.clone())
        .collect()
}
/// The open title's unfinished downloads, shown in a card above its chapters.
pub(super) fn publish_detail_queue(view: &View, ui: &AppWindow) {
    let series = view
        .series
        .as_ref()
        .map(|s| &s.id)
        .filter(|_| view.section == 5);
    let jobs: Vec<&ChapterDownload> = view
        .downloads
        .jobs
        .iter()
        .filter(|j| Some(&j.chapter.series) == series)
        .filter(|j| {
            matches!(
                j.status,
                JobStatus::Queued | JobStatus::Running | JobStatus::Paused | JobStatus::Failed
            )
        })
        .collect();
    let count = |status: JobStatus| jobs.iter().filter(|j| j.status == status).count();
    let (queued, paused, failed) = (
        count(JobStatus::Queued),
        count(JobStatus::Paused),
        count(JobStatus::Failed),
    );
    let running = jobs.iter().find(|j| j.status == JobStatus::Running);
    let fraction = running
        .filter(|j| !j.pages.is_empty())
        .map(|j| j.completed_pages() as f32 / j.pages.len() as f32)
        .unwrap_or(0.);
    let title = match running {
        Some(job) => {
            let label = view
                .rows
                .iter()
                .find_map(|row| match row {
                    Row::Chapter(c) if c.id == job.chapter => {
                        Some(c.number.clone().unwrap_or_else(|| c.label.clone()))
                    }
                    _ => None,
                })
                .unwrap_or_default();
            format!(
                "{} {label} · {}%",
                text::tr("Downloading chapter"),
                (fraction * 100.).round() as u32
            )
        }
        None if queued > 0 => text::tr("Waiting to download"),
        None if paused > 0 => text::tr("Downloads paused"),
        None => text::tr("Downloads failed"),
    };
    let mut counts = Vec::new();
    if queued > 0 {
        counts.push(format!("{queued} {}", text::tr("queued")));
    }
    if paused > 0 {
        counts.push(format!("{paused} {}", text::tr("paused")));
    }
    ui.set_detail_queue_visible(!jobs.is_empty());
    ui.set_detail_queue_title(title.into());
    ui.set_detail_queue_counts(counts.join(" · ").into());
    ui.set_detail_queue_failed(if failed > 0 {
        format!("{failed} {}", text::tr("failed")).into()
    } else {
        Default::default()
    });
    ui.set_detail_queue_progress(fraction);
    ui.set_detail_queue_running(running.is_some());
    // Pause while anything runs or waits; otherwise offer to resume.
    ui.set_detail_queue_pausable(running.is_some() || queued > 0);
}

impl Controller {
    /// Pause or resume only the open title's jobs.
    pub(super) fn detail_queue_action(self: &Arc<Self>, ui: &AppWindow, action: &str) {
        let jobs = {
            let view = self.view();
            let Some(series) = view.series.as_ref().map(|s| s.id.clone()) else {
                return;
            };
            let jobs: Vec<ChapterDownload> = view
                .downloads
                .jobs
                .iter()
                .filter(|j| j.chapter.series == series)
                .cloned()
                .collect::<Vec<_>>();
            jobs
        };
        if action == "pause" {
            for job in jobs
                .iter()
                .filter(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running))
            {
                if let Err(e) = self.services.downloads.pause(&job.chapter) {
                    ui.set_status(error(e).into());
                }
            }
            self.refresh_download_states(ui);
        } else if action == "resume" {
            self.download_many(ui, resume_all_targets(&jobs));
        }
    }

    pub(super) fn patch_chapter_downloads(&self, ui: &AppWindow) {
        let view = self.view();
        let model = ui.get_items();
        for (index, entry) in view.rows.iter().enumerate() {
            let Row::Chapter(chapter) = entry else {
                continue;
            };
            let Some(mut row) = model.row_data(index) else {
                continue;
            };
            if row.local_import {
                continue;
            }
            let status = view.download_states.get(&chapter.id);
            let next_state = status
                .map(|status| match status {
                    JobStatus::Complete => 5,
                    JobStatus::Queued => 1,
                    JobStatus::Running => 2,
                    JobStatus::Paused => 3,
                    JobStatus::Failed => 4,
                    JobStatus::Cancelled => 6,
                })
                .unwrap_or(0);
            let progress = view
                .downloads
                .jobs
                .iter()
                .find(|j| j.chapter == chapter.id)
                .filter(|j| !j.pages.is_empty())
                .map(|j| j.completed_pages() as f32 / j.pages.len() as f32)
                .unwrap_or(0.);
            if row.download_state != next_state
                || row.download_progress != progress
                || row.has_download != status.is_some()
            {
                row.download_state = next_state;
                row.download_progress = progress;
                row.has_download = status.is_some();
                row.downloaded = next_state == 5;
                // The details line leads with the transfer while there is one,
                // and falls back to the reading position when it ends.
                let status = crate::chapter_list::status(next_state, progress, None, false)
                    .filter(|_| (1..=4).contains(&next_state));
                row.status_danger = status.as_ref().is_some_and(|(_, danger)| *danger);
                row.status = status
                    .map(|(status, _)| status.into())
                    .unwrap_or_else(|| row.reading_status.clone());
                model.set_row_data(index, row);
            }
        }
        super::publish_chapter_filter_counts(ui);
        publish_detail_queue(&view, ui);
    }
    pub(super) fn publish_downloads(&self, ui: &AppWindow) {
        let mut view = self.view();
        if view.section != 7 {
            return;
        }
        let downloads = &mut view.downloads;
        let Some(catalogue) = &downloads.catalogue else {
            return;
        };
        let chapters: BTreeMap<_, _> = catalogue.chapters.iter().map(|c| (&c.id, c)).collect();
        let series: BTreeMap<_, _> = catalogue.series.iter().map(|s| (&s.id, s)).collect();
        let progress: BTreeMap<_, _> = catalogue.progress.iter().map(|p| (&p.chapter, p)).collect();
        let query = ui.get_download_query().to_lowercase();
        let unread = ui.get_download_unread_only();
        let sort = ui.get_download_sort();
        // Chapter files are shared by every profile on this device, but each
        // profile lists only the downloads of series it knows.
        let completed: Vec<_> = downloads
            .jobs
            .iter()
            .filter(|j| {
                j.status == JobStatus::Complete
                    && j.chapter.series.source_id != "local"
                    && series.contains_key(&j.chapter.series)
            })
            .collect();
        let saved_bytes: u64 = completed.iter().map(|j| j.downloaded_bytes()).sum();
        let titles = completed
            .iter()
            .map(|j| &j.chapter.series)
            .collect::<BTreeSet<_>>()
            .len();
        let mut groups: BTreeMap<&SeriesId, Vec<&ChapterDownload>> = BTreeMap::new();
        for job in &completed {
            let title = series
                .get(&job.chapter.series)
                .map(|s| s.title.as_str())
                .unwrap_or(&job.chapter.series.local_id);
            let chapter = chapters
                .get(&job.chapter)
                .map(|c| c.title.as_str())
                .unwrap_or(&job.chapter.local_id);
            if unread && progress.get(&job.chapter).is_some_and(|p| p.completed) {
                continue;
            }
            if !query.is_empty()
                && !format!("{title} {chapter} {}", job.chapter.series.source_id)
                    .to_lowercase()
                    .contains(&query)
            {
                continue;
            }
            groups.entry(&job.chapter.series).or_default().push(job);
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by(|(a, aj), (b, bj)| {
            let title = |id: &SeriesId| {
                series
                    .get(id)
                    .map(|s| s.title.to_lowercase())
                    .unwrap_or_else(|| id.local_id.to_lowercase())
            };
            if sort == 1 {
                bj.iter()
                    .map(|j| j.downloaded_bytes())
                    .sum::<u64>()
                    .cmp(&aj.iter().map(|j| j.downloaded_bytes()).sum::<u64>())
                    .then_with(|| a.cmp(b))
            } else {
                title(a).cmp(&title(b)).then_with(|| a.cmp(b))
            }
        });
        if !downloads.initialized
            && let Some((first, _)) = groups.first()
        {
            downloads.expanded.insert((*first).clone());
            downloads.initialized = true;
        }
        let valid: BTreeSet<_> = completed.iter().map(|j| j.chapter.clone()).collect();
        downloads.selected.retain(|id| valid.contains(id));
        downloads.eligible.clear();
        let mut rows = Vec::new();
        for (id, mut jobs) in groups {
            jobs.sort_by(|a, b| {
                if sort == 1 {
                    b.downloaded_bytes()
                        .cmp(&a.downloaded_bytes())
                        .then_with(|| a.chapter.cmp(&b.chapter))
                } else {
                    chapters
                        .get(&b.chapter)
                        .map(|c| c.source_order)
                        .cmp(&chapters.get(&a.chapter).map(|c| c.source_order))
                        .then_with(|| b.chapter.cmp(&a.chapter))
                }
            });
            let title = series
                .get(id)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| id.local_id.clone());
            let source = catalogue
                .sources
                .iter()
                .find(|s| s.id == id.source_id || id.source_id.starts_with(&format!("{}.", s.id)))
                .map(|s| s.name.clone())
                .unwrap_or_else(|| id.source_id.clone());
            let art = series
                .get(id)
                .map(|s| crate::artwork::key(s))
                .unwrap_or_default();
            let poster = series
                .get(id)
                .and_then(|s| self.artwork.cached_image(s))
                .unwrap_or_default();
            let expanded = downloads.expanded.contains(id);
            let total = jobs.iter().map(|j| j.downloaded_bytes()).sum();
            rows.push(DownloadEntry {
                key: key(id).into(),
                series_key: key(id).into(),
                series_title: title.clone().into(),
                source: source.clone().into(),
                meta: text::chapters_label(jobs.len()).into(),
                size_label: size(total).into(),
                artwork_key: art.clone().into(),
                poster: poster.clone(),
                expanded,
                ..Default::default()
            });
            for job in jobs {
                downloads.eligible.push(job.chapter.clone());
                if !expanded {
                    continue;
                }
                let p = progress.get(&job.chapter);
                let read_meta = if p.is_some_and(|p| p.completed) {
                    text::tr("Read")
                } else if let Some(p) = p.filter(|p| {
                    p.last_read_at > 0 && catalogue.history.iter().any(|h| h.chapter == job.chapter)
                }) {
                    format!(
                        "{} {} / {}",
                        text::tr("Page"),
                        p.anchor.page_index + 1,
                        job.pages.len()
                    )
                } else {
                    text::tr("Unread")
                };
                rows.push(DownloadEntry {
                    key: key(&job.chapter).into(),
                    series_key: key(id).into(),
                    kind: 1,
                    title: chapters
                        .get(&job.chapter)
                        .map(|c| c.title.clone())
                        .unwrap_or_else(|| job.chapter.local_id.clone())
                        .into(),
                    series_title: title.clone().into(),
                    source: source.clone().into(),
                    meta: text::pages_label(job.pages.len()).into(),
                    read_meta: read_meta.into(),
                    size_label: size(job.downloaded_bytes()).into(),
                    selected: downloads.selected.contains(&job.chapter),
                    ..Default::default()
                });
            }
        }
        // Count, size and bulk delete must describe only what the reader can see.
        DownloadView::trim_selection(&mut downloads.selected, &downloads.eligible);
        let mut queued: Vec<_> = downloads
            .jobs
            .iter()
            .filter(|j| {
                j.status != JobStatus::Complete
                    && j.chapter.series.source_id != "local"
                    && series.contains_key(&j.chapter.series)
            })
            .cloned()
            .collect();
        for (id, desired) in &downloads.pending {
            if !queued.iter().any(|j| &j.chapter == id)
                && !completed.iter().any(|j| &j.chapter == id)
            {
                queued.push(ChapterDownload {
                    schema_version: 1,
                    queue_order: u64::MAX,
                    chapter: id.clone(),
                    pages: vec![],
                    status: desired.clone().unwrap_or(JobStatus::Queued),
                    error: None,
                });
            }
        }
        // An explicit resume waits behind the serial worker; show it as queued
        // rather than the stale Paused/Failed/Cancelled manifest status.
        for job in &mut queued {
            if matches!(downloads.pending.get(&job.chapter), Some(None))
                && matches!(
                    job.status,
                    JobStatus::Paused | JobStatus::Failed | JobStatus::Cancelled
                )
            {
                job.status = JobStatus::Queued;
                job.error = None;
            }
        }
        for (id, failure) in &downloads.prepare_errors {
            if let Some(job) = queued.iter_mut().find(|job| &job.chapter == id) {
                job.status = JobStatus::Failed;
                job.error = Some(failure.clone());
            } else {
                queued.push(ChapterDownload {
                    schema_version: 1,
                    queue_order: u64::MAX,
                    chapter: id.clone(),
                    pages: vec![],
                    status: JobStatus::Failed,
                    error: Some(failure.clone()),
                });
            }
        }
        queued.sort_by_key(|job| {
            (
                if job.status == JobStatus::Running {
                    0
                } else {
                    1
                },
                job.queue_order,
                job.chapter.clone(),
            )
        });
        let mut queue = Vec::new();
        let mut running = 0;
        let mut waiting = 0;
        let mut attention = 0;
        let now = Instant::now();
        for job in &queued {
            match job.status {
                JobStatus::Running => running += 1,
                JobStatus::Queued => waiting += 1,
                JobStatus::Failed => attention += 1,
                _ => {}
            }
            let id = &job.chapter.series;
            let rate = downloads.samples.entry(job.chapter.clone()).or_insert((
                job.downloaded_bytes(),
                now,
                String::new(),
            ));
            if job.status != JobStatus::Running {
                rate.0 = job.downloaded_bytes();
                rate.1 = now;
                rate.2.clear();
            } else if job.downloaded_bytes() > rate.0
                && now.duration_since(rate.1) >= Duration::from_millis(500)
            {
                let bytes_per_second = (job.downloaded_bytes() - rate.0) as f64
                    / now.duration_since(rate.1).as_secs_f64();
                rate.2 = format!("~{}/s", size(bytes_per_second as u64));
                rate.0 = job.downloaded_bytes();
                rate.1 = now;
            }
            queue.push(DownloadEntry {
                key: key(&job.chapter).into(),
                series_key: key(id).into(),
                kind: 1,
                state: state(&job.status),
                title: chapters
                    .get(&job.chapter)
                    .map(|c| c.title.clone())
                    .unwrap_or_else(|| job.chapter.local_id.clone())
                    .into(),
                series_title: series
                    .get(id)
                    .map(|s| s.title.clone())
                    .unwrap_or_else(|| id.local_id.clone())
                    .into(),
                source: id.source_id.clone().into(),
                progress: if job.pages.is_empty() {
                    0.
                } else {
                    job.completed_pages() as f32 / job.pages.len() as f32
                },
                done_pages: job.completed_pages() as i32,
                total_pages: job.pages.len() as i32,
                rate_label: rate.2.clone().into(),
                error: job
                    .error
                    .as_deref()
                    .map(text::download_error)
                    .unwrap_or_default()
                    .into(),
                size_label: size(job.downloaded_bytes()).into(),
                poster: series
                    .get(id)
                    .and_then(|s| self.artwork.cached_image(s))
                    .unwrap_or_default(),
                artwork_key: series
                    .get(id)
                    .map(|s| crate::artwork::key(s))
                    .unwrap_or_default()
                    .into(),
                ..Default::default()
            });
        }
        downloads
            .samples
            .retain(|id, _| queued.iter().any(|j| &j.chapter == id));
        let selected_bytes: u64 = completed
            .iter()
            .filter(|j| downloads.selected.contains(&j.chapter))
            .map(|j| j.downloaded_bytes())
            .sum();
        ui.set_downloaded_count(completed.len() as i32);
        ui.set_download_queue_count(queue.len() as i32);
        ui.set_download_summary(
            format!(
                "{} · {} · {}",
                text::titles_label(titles),
                text::chapters_label(completed.len()),
                size(saved_bytes)
            )
            .into(),
        );
        ui.set_download_queue_summary(
            format!(
                "{running} {} · {waiting} {} · {attention} {}",
                text::tr("downloading"),
                text::tr("waiting"),
                text::tr("need attention")
            )
            .into(),
        );
        ui.set_download_selected_count(downloads.selected.len() as i32);
        ui.set_download_selected_size(size(selected_bytes).into());
        ui.set_download_busy(downloads.busy);
        drop(view);
        model(ui, rows, false);
        model(ui, queue, true);
    }
    pub(super) fn download_artwork(&self, key: &str) {
        let Ok(id) = serde_json::from_str::<SeriesId>(key) else {
            return;
        };
        let Ok(view) = self.view.try_lock() else {
            return;
        };
        if view.section != 7 {
            return;
        }
        let series = view
            .downloads
            .catalogue
            .as_ref()
            .and_then(|s| s.series.iter().find(|s| s.id == id))
            .cloned();
        drop(view);
        if let Some(series) = series {
            self.artwork.request(&series);
        }
    }
    /// Destructive removals ask first, stating the count and size.
    fn confirm_download_delete(&self, ui: &AppWindow, key: &str, action: &str) {
        let mut view = self.view();
        if view.downloads.busy {
            return;
        }
        let ids: Vec<ChapterId> = if action == "delete-selected" {
            view.downloads
                .selected
                .iter()
                .filter(|id| view.downloads.eligible.contains(id))
                .cloned()
                .collect()
        } else {
            serde_json::from_str::<ChapterId>(key).into_iter().collect()
        };
        if ids.is_empty() {
            return;
        }
        let bytes: u64 = view
            .downloads
            .jobs
            .iter()
            .filter(|j| ids.contains(&j.chapter))
            .map(|j| j.downloaded_bytes())
            .sum();
        view.downloads.confirm = Some((key.to_owned(), action.to_owned()));
        drop(view);
        ui.set_download_confirm_message(
            text::download_delete_prompt(ids.len(), &size(bytes)).into(),
        );
        ui.set_download_confirm_open(true);
    }
    pub(super) fn resolve_download_delete(self: &Arc<Self>, ui: &AppWindow, confirmed: bool) {
        ui.set_download_confirm_open(false);
        let pending = self.view().downloads.confirm.take();
        if let (true, Some((key, action))) = (confirmed, pending) {
            self.downloads_action(ui, &key, &format!("confirmed-{action}"));
        }
    }
    pub(super) fn downloads_action(self: &Arc<Self>, ui: &AppWindow, key: &str, action: &str) {
        if matches!(action, "delete" | "delete-selected") {
            self.confirm_download_delete(ui, key, action);
            return;
        }
        let action = action.strip_prefix("confirmed-").unwrap_or(action);
        match action {
            "toggle-series" => {
                if let Ok(id) = serde_json::from_str::<SeriesId>(key) {
                    let mut v = self.view();
                    if !v.downloads.expanded.remove(&id) {
                        v.downloads.expanded.insert(id);
                    }
                }
                self.publish_downloads(ui);
                return;
            }
            "select" => {
                if let Ok(id) = serde_json::from_str::<ChapterId>(key) {
                    let mut v = self.view();
                    if !v.downloads.selected.remove(&id) && v.downloads.eligible.contains(&id) {
                        v.downloads.selected.insert(id);
                    }
                }
                self.publish_downloads(ui);
                return;
            }
            "select-all" => {
                let mut v = self.view();
                let ids = v.downloads.eligible.clone();
                v.downloads.selected.extend(ids);
                drop(v);
                self.publish_downloads(ui);
                return;
            }
            "clear-selection" => {
                self.view().downloads.selected.clear();
                self.publish_downloads(ui);
                return;
            }
            _ => {}
        }
        let chapter = serde_json::from_str::<ChapterId>(key).ok();
        if action == "open" {
            if let Some(id) = chapter {
                self.open_chapter(ui, id);
            }
            return;
        }
        if matches!(action, "resume" | "retry") {
            if let Some(id) = chapter {
                // A row already being prepared cannot start a second worker.
                if self.view().downloads.resume_pending(&id) {
                    self.publish_downloads(ui);
                } else {
                    self.download(ui, id);
                }
            }
            return;
        }
        let mut ids = if action == "delete-selected" {
            let v = self.view();
            v.downloads
                .selected
                .iter()
                .filter(|id| v.downloads.eligible.contains(id))
                .cloned()
                .collect()
        } else if matches!(action, "pause-all" | "resume-all") {
            let v = self.view();
            if action == "pause-all" {
                v.downloads
                    .jobs
                    .iter()
                    .filter(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running))
                    .map(|j| j.chapter.clone())
                    .collect()
            } else {
                resume_all_targets(&v.downloads.jobs)
            }
        } else {
            chapter.into_iter().collect::<Vec<_>>()
        };
        if action == "resume-all" {
            {
                let mut view = self.view();
                ids.extend(view.downloads.prepare_errors.keys().cloned());
                for desired in view.downloads.pending.values_mut() {
                    *desired = None;
                }
            }
            ids.sort();
            ids.dedup();
            self.download_many(ui, ids);
            return;
        }
        {
            let mut v = self.view();
            if v.downloads.busy {
                return;
            }
            if action == "pause-all" {
                for value in v.downloads.pending.values_mut() {
                    *value = Some(JobStatus::Paused);
                }
            }
            if let Some(id) = serde_json::from_str::<ChapterId>(key).ok()
                && let Some(value) = v.downloads.pending.get_mut(&id)
            {
                if action == "pause" {
                    *value = Some(JobStatus::Paused);
                } else if action == "cancel" {
                    *value = Some(JobStatus::Cancelled);
                }
            }
            if action == "cancel"
                && let Ok(id) = serde_json::from_str::<ChapterId>(key)
            {
                v.downloads.dismiss_prepare_failure(&id);
            }
            v.downloads.busy = true;
        }
        ui.set_download_busy(true);
        let app = self.clone();
        let weak = ui.as_weak();
        let action = action.to_owned();
        crate::profiles::spawn(move || {
            let result = (|| -> Result<()> {
                for id in ids {
                    if id.series.source_id == "local" {
                        continue;
                    }
                    match action.as_str() {
                        "delete" | "delete-selected" => {
                            let metadata_failure =
                                app.view().downloads.prepare_errors.remove(&id).is_some();
                            if !metadata_failure
                                || app
                                    .services
                                    .downloads
                                    .snapshot()
                                    .map_err(error)?
                                    .iter()
                                    .any(|j| j.chapter == id)
                            {
                                app.remove_download(&id)?;
                            }
                        }
                        "pause" | "pause-all" => {
                            if app.services.downloads.job(&id).is_ok() {
                                app.services.downloads.pause(&id).map_err(error)?;
                            }
                        }
                        "cancel" => {
                            if app.services.downloads.job(&id).is_ok() {
                                app.services.downloads.cancel(&id).map_err(error)?;
                            }
                        }
                        "move-up" | "move-down" => {
                            let jobs = app.services.downloads.snapshot().map_err(error)?;
                            let queue: Vec<_> = jobs
                                .iter()
                                .filter(|j| j.status == JobStatus::Queued)
                                .map(|j| &j.chapter)
                                .collect();
                            if let Some(index) = queue.iter().position(|other| **other == id) {
                                let target = if action == "move-up" {
                                    if index == 0 {
                                        continue;
                                    }
                                    queue.get(index - 1).copied()
                                } else {
                                    if index + 1 >= queue.len() {
                                        continue;
                                    }
                                    queue.get(index + 2).copied()
                                };
                                app.services
                                    .downloads
                                    .move_before(&id, target)
                                    .map_err(error)?;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(())
            })();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                app.view().downloads.busy = false;
                if let Err(e) = result {
                    ui.set_status(e.into());
                }
                app.refresh_download_states(&ui);
            });
        });
    }
    pub(super) fn downloads_reorder(self: &Arc<Self>, ui: &AppWindow, key: &str, before: &str) {
        let Ok(id) = serde_json::from_str::<ChapterId>(key) else {
            return;
        };
        let before = if before.is_empty() {
            None
        } else {
            match serde_json::from_str::<ChapterId>(before) {
                Ok(id) => Some(id),
                Err(_) => return,
            }
        };
        let app = self.clone();
        let weak = ui.as_weak();
        crate::profiles::spawn(move || {
            // Dropping onto a row means "take its place", so moving down lands
            // after it; the empty target is the end-of-queue drop zone.
            let result = match before.as_ref() {
                Some(onto) => app.services.downloads.move_onto(&id, onto),
                None => app.services.downloads.move_before(&id, None),
            };
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if let Err(e) = result {
                    ui.set_status(error(e).into());
                }
                app.refresh_download_states(&ui);
            });
        });
    }
}
pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) {
    let owner = app.clone();
    let weak = ui.as_weak();
    ui.on_download_action(move |key, action| {
        if let Some(ui) = weak.upgrade() {
            owner.downloads_action(&ui, &key, &action);
        }
    });
    let owner = app.clone();
    let weak = ui.as_weak();
    ui.on_download_reorder(move |key, before| {
        if let Some(ui) = weak.upgrade() {
            owner.downloads_reorder(&ui, &key, &before);
        }
    });
    let owner = app.clone();
    let weak = ui.as_weak();
    ui.on_download_search(move |query| {
        if let Some(ui) = weak.upgrade() {
            ui.set_download_query(query);
            owner.publish_downloads(&ui);
        }
    });
    let owner = app.clone();
    let weak = ui.as_weak();
    ui.on_download_filter(move |unread, sort| {
        if let Some(ui) = weak.upgrade() {
            ui.set_download_unread_only(unread);
            ui.set_download_sort(sort);
            owner.publish_downloads(&ui);
        }
    });
    let owner = app.clone();
    let weak = ui.as_weak();
    ui.on_download_confirm_choice(move |choice| {
        if let Some(ui) = weak.upgrade() {
            owner.resolve_download_delete(&ui, choice == 0);
        }
    });
    let owner = app.clone();
    ui.on_download_artwork(move |key| owner.download_artwork(&key));
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    use reader_downloads::PageResource;

    fn chapter_id(n: &str) -> ChapterId {
        ChapterId::new(SeriesId::new("fixture.en", "lantern"), n)
    }
    fn job(n: &str, status: JobStatus) -> ChapterDownload {
        ChapterDownload {
            schema_version: 1,
            queue_order: 0,
            chapter: chapter_id(n),
            pages: vec![],
            status,
            error: None,
        }
    }

    #[test]
    fn cancelling_a_prepare_failed_row_dismisses_it() {
        let mut view = DownloadView::default();
        let id = chapter_id("1");
        view.prepare_errors.insert(id.clone(), "offline".into());
        view.pending.insert(id.clone(), Some(JobStatus::Cancelled));
        assert!(view.dismiss_prepare_failure(&id));
        assert!(view.prepare_errors.is_empty() && view.pending.is_empty());
        assert!(!view.dismiss_prepare_failure(&id));
    }

    #[test]
    fn resume_clears_a_pause_requested_while_preparing() {
        let mut view = DownloadView::default();
        let id = chapter_id("1");
        view.pending.insert(id.clone(), Some(JobStatus::Paused));
        assert!(view.resume_pending(&id));
        assert_eq!(view.pending[&id], None);
        assert!(!view.resume_pending(&chapter_id("2")));
    }

    #[test]
    fn bulk_delete_selection_is_trimmed_to_the_visible_rows() {
        let mut view = DownloadView::default();
        view.selected = BTreeSet::from([chapter_id("1"), chapter_id("2")]);
        view.eligible = vec![chapter_id("2"), chapter_id("3")];
        DownloadView::trim_selection(&mut view.selected, &view.eligible);
        assert_eq!(view.selected, BTreeSet::from([chapter_id("2")]));
    }

    #[test]
    fn resume_all_skips_cancelled_jobs() {
        let jobs = vec![
            job("paused", JobStatus::Paused),
            job("cancelled", JobStatus::Cancelled),
            job("failed", JobStatus::Failed),
            job("queued", JobStatus::Queued),
        ];
        assert_eq!(
            resume_all_targets(&jobs),
            vec![chapter_id("paused"), chapter_id("failed")]
        );
    }

    #[test]
    fn sizes_use_binary_unit_labels() {
        assert_eq!(size(2048), "2 KiB");
        assert_eq!(size(3 << 20), "3.0 MiB");
        assert_eq!(size(5 << 30), "5.0 GiB");
    }

    #[test]
    fn download_models_keep_partial_jobs_separate_and_actions_use_stable_ids() {
        if std::env::var_os("NOVA_DOWNLOAD_MODELS_CHILD").is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","app::downloads::tests::download_models_keep_partial_jobs_separate_and_actions_use_stable_ids"])
                .env("NOVA_DOWNLOAD_MODELS_CHILD","1").status().unwrap().success());
            return;
        }
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let ui = AppWindow::new().unwrap();
        ui.set_section(7);
        let services = Services::open(&dir.path().join("data")).unwrap();
        let sid = SeriesId::new("fixture.en", "lantern");
        let ids: Vec<_> = (1..=3)
            .map(|n| ChapterId::new(sid.clone(), n.to_string()))
            .collect();
        services
            .domain
            .update(|state| {
                state.series.push(Series {
                    id: sid.clone(),
                    title: "The Last Lantern".into(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                    publication_status: Default::default(),
                });
                for (index, id) in ids.iter().enumerate() {
                    state.chapters.push(Chapter {
                        id: id.clone(),
                        title: format!("Chapter {}", index + 1),
                        label: String::new(),
                        number: Some((index + 1).to_string()),
                        volume: None,
                        source_order: index as i64,
                        language: None,
                        scanlator: None,
                        uploaded_at: None,
                    });
                }
                Ok(())
            })
            .unwrap();
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                &[80, 120, 180, 255].repeat(32 * 48),
                32,
                48,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        for id in &ids {
            services
                .downloads
                .enqueue(
                    id.clone(),
                    vec![PageResource {
                        id: "page".into(),
                        url: "https://example.test/page.png".into(),
                        headers: BTreeMap::new(),
                    }],
                )
                .unwrap();
        }
        for id in &ids[..2] {
            services.downloads.run(id, |_| Ok(png.clone())).unwrap();
        }
        services.downloads.pause(&ids[2]).unwrap();
        services
            .domain
            .update_manual_progress(|state| toggle_chapter_read(state, ids[0].clone(), 50))
            .unwrap();
        let (images, _receiver) = mpsc::channel();
        let app = Arc::new(Controller {
            progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
                services.domain.clone(),
            )
            .unwrap(),
            artwork: crate::artwork::ArtworkLoader::new(
                services.clone(),
                ui.as_weak(),
                dir.path().join("posters"),
            ),
            services,
            chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
                dir.path().join("viewed"),
            )),
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::new(View {
                section: 7,
                ..Default::default()
            }),
            generation: AtomicU64::new(0),
            cancel: Mutex::new(CancelToken::default()),
            images,
            download_serial: Mutex::new(()),
            active_image: Mutex::new(None),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        {
            let mut view = app.view();
            view.downloads.jobs = app.services.downloads.snapshot().unwrap();
            view.downloads.catalogue = Some(app.services.domain.snapshot().unwrap());
        }
        app.publish_downloads(&ui);
        assert_eq!(ui.get_downloaded_count(), 2);
        assert_eq!(ui.get_download_queue_count(), 1);
        assert_eq!(ui.get_download_queue().row_data(0).unwrap().state, 3);
        assert_eq!(
            ui.get_downloaded_rows().row_count(),
            3,
            "one group and two complete chapters"
        );
        // One-page chapters and a single title must not read "1 pages".
        let listed = ui.get_downloaded_rows();
        assert_eq!(listed.row_data(0).unwrap().meta, "2 chapters");
        assert_eq!(listed.row_data(1).unwrap().meta, "1 page");
        assert!(
            ui.get_download_summary()
                .starts_with("1 title · 2 chapters")
        );
        let existing = ui.get_downloaded_rows();
        app.publish_downloads(&ui);
        assert_eq!(existing.row_count(), ui.get_downloaded_rows().row_count());
        app.downloads_action(&ui, &key(&sid), "toggle-series");
        assert_eq!(ui.get_downloaded_rows().row_count(), 1);
        app.downloads_action(&ui, "", "select-all");
        assert_eq!(
            ui.get_download_selected_count(),
            2,
            "selects matching chapters even in collapsed groups"
        );
        app.downloads_action(&ui, "", "clear-selection");
        ui.set_download_query("Chapter 2".into());
        app.publish_downloads(&ui);
        app.downloads_action(&ui, "", "select-all");
        assert_eq!(
            app.view().downloads.selected,
            BTreeSet::from([ids[1].clone()])
        );
        ui.set_download_query("".into());
        ui.set_download_unread_only(true);
        app.publish_downloads(&ui);
        assert_eq!(app.view().downloads.eligible, vec![ids[1].clone()]);
        // Detail uses completion/read colors and icons, never download labels or sizes.
        {
            let mut view = app.view();
            view.section = 5;
            view.series = app
                .services
                .domain
                .snapshot()
                .unwrap()
                .series
                .first()
                .cloned();
            view.chapters = app.services.domain.snapshot().unwrap().chapters;
            view.download_states = app.services.downloads.statuses().unwrap();
        }
        app.render(&ui);
        let rows = ui.get_items();
        assert!(rows.iter().all(
            |row| !row.chapter_meta.contains("Downloaded") && !row.chapter_meta.contains(" MB")
        ));
        assert!(rows.iter().any(|row| row.read));
        assert!(rows.iter().any(|row| row.download_state == 5));
        assert!(rows.iter().any(|row| row.download_state == 3));
        let index = rows.iter().position(|row| row.download_state == 5).unwrap();
        let before = rows.row_data(index).unwrap();
        let chapter = match &app.view().rows[index] {
            Row::Chapter(c) => c.id.clone(),
            _ => unreachable!(),
        };
        app.view()
            .download_states
            .insert(chapter, JobStatus::Paused);
        app.patch_chapter_downloads(&ui);
        let after = rows.row_data(index).unwrap();
        assert_eq!(
            after.download_state, 3,
            "existing model handles see progress patches"
        );
        assert_eq!(after.read, before.read);
        assert_eq!(after.chapter_meta, before.chapter_meta);
    }
}

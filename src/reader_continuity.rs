//! Chapter metadata prefetch and bounded, in-place reader page-model growth.
//! Encoded pixels still flow through the existing image worker and its budgets.
use super::*;

const MAX_SEGMENTS: usize = 3;
/// How close to a chapter edge its neighbour's metadata starts loading. Pixel
/// read-ahead wants the neighbour's first pages two viewport heights (or two
/// pages) before the edge, and the inventory is a network round trip, so the
/// metadata starts two more viewports (or one more page) earlier.
const NEAR_VIEWPORTS: f32 = 4.;
const NEAR_PAGES: usize = 3;

type MetadataWork = Box<dyn FnOnce() + Send>;
#[derive(Default)]
struct MetadataQueue {
    running: bool,
    active: Option<CancelToken>,
    pending: Option<(CancelToken, MetadataWork)>,
}

/// One actual metadata worker and one latest pending request. A source that
/// ignores cancellation can delay the next request, but cannot create an
/// unbounded number of threads or concurrent provider operations.
#[derive(Default)]
pub(super) struct ContinuationWork {
    queue: Arc<Mutex<MetadataQueue>>,
}
impl ContinuationWork {
    pub(super) fn cancel(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cancel) = &queue.active {
            cancel.cancel();
        }
        if let Some((cancel, _)) = queue.pending.take() {
            cancel.cancel();
        }
    }

    fn submit(&self, cancel: CancelToken, work: MetadataWork) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(active) = &queue.active {
            active.cancel();
        }
        if let Some((old, _)) = queue.pending.replace((cancel, work)) {
            old.cancel();
        }
        if queue.running {
            return;
        }
        queue.running = true;
        let shared = self.queue.clone();
        drop(queue);
        crate::profiles::spawn(move || {
            loop {
                let work = {
                    let mut queue = shared.lock().unwrap_or_else(|e| e.into_inner());
                    match queue.pending.take() {
                        Some((cancel, work)) => {
                            queue.active = Some(cancel.clone());
                            Some((cancel, work))
                        }
                        None => {
                            queue.running = false;
                            queue.active = None;
                            None
                        }
                    }
                };
                let Some((cancel, work)) = work else {
                    break;
                };
                if !cancel.is_cancelled() {
                    work();
                }
                shared.lock().unwrap_or_else(|e| e.into_inner()).active = None;
            }
        });
    }
}

#[derive(Clone)]
pub(super) struct ChapterSegment {
    pub chapter: ChapterId,
    pub title: String,
    pub start: usize,
    /// Exclusive source-page index in the combined page list.
    pub end: usize,
    pub source: Option<Source>,
    pub completed: bool,
}

impl Reader {
    fn sync_continuation_epoch(&mut self, generation: u64) -> bool {
        if self.preload_epoch != generation {
            self.preloading = None;
            self.preload_nonce = self.preload_nonce.wrapping_add(1);
            self.continuity_error = None;
            self.preload_epoch = generation;
            true
        } else {
            false
        }
    }
    pub(super) fn initialise_segments(&mut self, title: String) {
        if self.segments.is_empty() && !self.pages.is_empty() {
            self.segments.push(ChapterSegment {
                chapter: self.chapter.clone(),
                title,
                start: 0,
                end: self.pages.len(),
                source: self.source.clone(),
                completed: self.completed,
            });
        }
    }
    pub(super) fn chapter_segment(&self, index: usize) -> Option<&ChapterSegment> {
        self.segments
            .iter()
            .find(|segment| (segment.start..segment.end).contains(&index))
    }
    pub(super) fn owner(&self, index: usize) -> &ChapterId {
        self.chapter_segment(index)
            .map(|segment| &segment.chapter)
            .unwrap_or(&self.chapter)
    }
    pub(super) fn source_for(&self, index: usize) -> Option<Source> {
        self.chapter_segment(index)
            .map(|segment| segment.source.clone())
            .unwrap_or_else(|| self.source.clone())
    }
    pub(super) fn local_index(&self, index: usize) -> usize {
        self.chapter_segment(index)
            .map_or(index, |segment| index - segment.start)
    }
    pub(super) fn active_count(&self) -> usize {
        self.chapter_segment(self.anchor.page_index)
            .map_or(self.pages.len(), |segment| segment.end - segment.start)
    }
    /// Save completion before changing the anchor, then restore the target
    /// chapter's completion. Backward continuation must not mark read art unread.
    pub(super) fn continuity_completion(
        &mut self,
        next_index: usize,
        previous_completed: bool,
    ) -> bool {
        if let Some(previous) = self
            .segments
            .iter_mut()
            .find(|segment| (segment.start..segment.end).contains(&self.anchor.page_index))
        {
            previous.completed = previous_completed;
        }
        self.chapter_segment(next_index)
            .is_some_and(|segment| segment.completed)
    }
    /// UI identity includes the owning chapter; progress keeps canonical page IDs.
    pub(super) fn row_page_id(&self, index: usize) -> String {
        serde_json::to_string(&(self.owner(index), self.page_id(index)))
            .expect("page identity serializes")
    }
    fn near_continuation(&self, segment: &ChapterSegment) -> bool {
        if self.visible_end().saturating_add(NEAR_PAGES) >= segment.end {
            return true;
        }
        if self.mode != ReadingMode::Webtoon {
            return false;
        }
        let last = segment.end - 1;
        let remaining = self.page_position(last) + self.ratios[last]
            - self.page_position(self.anchor.page_index)
            - self.ratios[self.anchor.page_index] * self.anchor.offset() as f32;
        self.viewport_width > 0.
            && remaining * self.viewport_width <= self.viewport_height.max(0.) * NEAR_VIEWPORTS
    }
    fn near_previous(&self, segment: &ChapterSegment) -> bool {
        if self.anchor.page_index < segment.start.saturating_add(NEAR_PAGES) {
            return true;
        }
        let distance = self.page_position(self.anchor.page_index)
            + self.ratios[self.anchor.page_index] * self.anchor.offset() as f32
            - self.page_position(segment.start);
        self.mode == ReadingMode::Webtoon
            && self.viewport_width > 0.
            && distance * self.viewport_width <= self.viewport_height.max(0.) * NEAR_VIEWPORTS
    }

    fn continuation_candidates(
        &self,
        chapters: &[Chapter],
    ) -> Vec<(ChapterId, ChapterId, String, bool)> {
        let tail = self.segments.last().filter(|tail| {
            (self.mode == ReadingMode::Webtoon
                || (tail.start..tail.end).contains(&self.anchor.page_index))
                && self.near_continuation(tail)
                && self.prune_for_append().is_some()
        });
        let head = self.segments.first().filter(|head| {
            (head.start..head.end).contains(&self.anchor.page_index)
                && self.near_previous(head)
                && self.prune_for_prepend().is_some()
        });
        let edges = if self.continuity_backwards {
            [(head, true), (tail, false)]
        } else {
            [(tail, false), (head, true)]
        };
        let mut candidates = edges
            .into_iter()
            .filter_map(|(edge, backwards)| {
                let edge = edge?;
                let position = chapters.iter().position(|c| c.id == edge.chapter)?;
                let adjacent = if backwards {
                    position.checked_sub(1)?
                } else {
                    position + 1
                };
                let chapter = chapters.get(adjacent)?;
                (!self.segments.iter().any(|s| s.chapter == chapter.id)).then(|| {
                    (
                        edge.chapter.clone(),
                        chapter.id.clone(),
                        chapter.title.clone(),
                        backwards,
                    )
                })
            })
            .collect::<Vec<_>>();
        if let Some((chapter, backwards)) = self.navigation_metadata_target() {
            let edge = if backwards {
                self.segments.first()
            } else {
                self.segments.last()
            };
            if let Some(edge) = edge
                && let Some(target) = chapters.iter().find(|c| &c.id == chapter)
            {
                candidates.retain(|(_, id, _, _)| id != chapter);
                candidates.insert(
                    0,
                    (
                        edge.chapter.clone(),
                        chapter.clone(),
                        target.title.clone(),
                        backwards,
                    ),
                );
            }
        }
        candidates
    }
    /// Return the retained prefix length, never dropping visible artwork.
    fn prune_for_prepend(&self) -> Option<usize> {
        if self.segments.len() < MAX_SEGMENTS {
            return Some(self.pages.len());
        }
        let visible_segment = self
            .segments
            .iter()
            .position(|s| (s.start..s.end).contains(&self.visible_end()))?;
        let count = (visible_segment + 2)
            .max(MAX_SEGMENTS - 1)
            .min(self.segments.len());
        Some(
            self.segments
                .get(count)
                .map_or(self.pages.len(), |s| s.start),
        )
    }
    fn prune_for_append(&self) -> Option<usize> {
        if self.segments.len() < MAX_SEGMENTS {
            return Some(0);
        }
        let current = self
            .segments
            .iter()
            .position(|s| (s.start..s.end).contains(&self.anchor.page_index))?;
        let count = self
            .segments
            .len()
            .saturating_sub(MAX_SEGMENTS - 1)
            .min(current.saturating_sub(1));
        if count > 0 {
            Some(self.segments[count - 1].end)
        } else {
            // More than three segments are justified only by visible content;
            // stop speculative expansion once the tail leaves the viewport.
            self.segments
                .last()
                .filter(|tail| tail.start <= self.visible_end())
                .map(|_| 0)
        }
    }
    /// Rebase metadata by moving entries, preserving image/model allocations.
    /// Requested tasks are cleared: their generation is cancelled by the caller.
    fn prune_prefix(&mut self, remove: usize) -> f32 {
        let distance = self.page_position(remove);
        self.pages.drain(..remove);
        self.ratios.drain(..remove);
        self.segments.retain(|segment| segment.end > remove);
        for segment in &mut self.segments {
            segment.start -= remove;
            segment.end -= remove;
        }
        self.anchor.page_index -= remove;
        self.desired_anchor = self
            .desired_anchor
            .checked_sub(remove)
            .unwrap_or(usize::MAX);
        self.requested.clear();
        rebase_set(&mut self.retained, remove);
        rebase_set(&mut self.desired, remove);
        rebase_set(&mut self.failed, remove);
        rebase_set(&mut self.budget_blocked, remove);
        rebase_map(&mut self.decoded_bytes, remove);
        rebase_map(&mut self.decoded_widths, remove);
        rebase_map(&mut self.decoded_windows, remove);
        rebase_map(&mut self.decoded_responses, remove);
        rebase_map(&mut self.native_dimensions, remove);
        rebase_map(&mut self.presentations, remove);
        rebase_map(&mut self.image_retries, remove);
        distance
    }
}
fn rebase_set(set: &mut BTreeSet<usize>, remove: usize) {
    *set = std::mem::take(set)
        .into_iter()
        .filter_map(|index| index.checked_sub(remove))
        .collect();
}
fn rebase_map<T>(map: &mut BTreeMap<usize, T>, remove: usize) {
    *map = std::mem::take(map)
        .into_iter()
        .filter_map(|(index, value)| index.checked_sub(remove).map(|index| (index, value)))
        .collect();
}

struct PreparedChapter {
    chapter: ChapterId,
    title: String,
    source: Option<Source>,
    pages: Vec<PageInput>,
    ratios: Vec<f32>,
    completed: bool,
}
fn prepare_chapter(
    services: &Services,
    chapter: ChapterId,
    title: String,
    cancel: &CancelToken,
) -> Result<PreparedChapter> {
    cancel.check().map_err(error)?;
    let completed = services
        .domain
        .snapshot()
        .map_err(error)?
        .progress
        .iter()
        .find(|progress| progress.chapter == chapter)
        .is_some_and(|progress| progress.completed);
    let inventory = crate::page_inventory::load(services, &chapter, cancel).map_err(error)?;
    let (source, pages, ratios) = reader_open::inventory_inputs(&inventory.pages);
    cancel.check().map_err(error)?;
    if source
        .as_ref()
        .is_some_and(|source| !source.availability().is_available())
    {
        return Err(text::tr("Source unavailable"));
    }
    Ok(PreparedChapter {
        chapter,
        title,
        source,
        pages,
        ratios,
        completed,
    })
}

fn append_rows(
    reader: &mut Reader,
    rows: &VecModel<PageRow>,
    prepared: PreparedChapter,
    remove: usize,
) -> f32 {
    let prepared_chapter = prepared.chapter.clone();
    let shift = if remove > 0 {
        reader.prune_prefix(remove)
    } else {
        0.
    };
    let start = reader.pages.len();
    reader.pages.extend(prepared.pages);
    reader.ratios.extend(prepared.ratios);
    reader.segments.push(ChapterSegment {
        chapter: prepared.chapter,
        title: prepared.title,
        start,
        end: reader.pages.len(),
        source: prepared.source,
        completed: prepared.completed,
    });
    reader.continuity_failures.remove(&prepared_chapter);
    if reader
        .continuity_error
        .as_ref()
        .is_some_and(|(chapter, _)| chapter == &prepared_chapter)
    {
        reader.continuity_error = None;
    }
    reader.desired_anchor = usize::MAX;
    reader.prune_retry_deadlines();
    for index in (0..remove).rev() {
        rows.remove(index);
    }
    if remove > 0 {
        for index in 0..start {
            if let Some(mut row) = rows.row_data(index) {
                row.position = reader.page_position(index);
                rows.set_row_data(index, row);
            }
        }
    }
    rows.extend((start..reader.pages.len()).map(|index| PageRow {
        page_id: reader.row_page_id(index).into(),
        ratio: reader.ratios[index],
        position: reader.page_position(index),
        ..Default::default()
    }));
    shift
}

/// Prepending shifts indices, but retains existing tile handles and logical
/// source offsets. Drop only an offscreen suffix to keep the three-chapter cap.
fn prepend_rows(
    reader: &mut Reader,
    rows: &VecModel<PageRow>,
    prepared: PreparedChapter,
    keep: usize,
) -> f32 {
    let prepared_chapter = prepared.chapter.clone();
    reader.pages.truncate(keep);
    reader.ratios.truncate(keep);
    reader.segments.retain(|segment| segment.start < keep);
    for index in (keep..rows.row_count()).rev() {
        rows.remove(index);
    }
    let count = prepared.pages.len();
    reader.pages.splice(0..0, prepared.pages);
    reader.ratios.splice(0..0, prepared.ratios);
    for segment in &mut reader.segments {
        segment.start += count;
        segment.end += count;
    }
    reader.segments.insert(
        0,
        ChapterSegment {
            chapter: prepared.chapter,
            title: prepared.title,
            start: 0,
            end: count,
            source: prepared.source,
            completed: prepared.completed,
        },
    );
    reader.anchor.page_index += count;
    reader.desired_anchor = usize::MAX;
    reader.requested.clear();
    reader.progressive_job = None;
    macro_rules! shift_set { ($($field:ident),*) => { $(reader.$field = std::mem::take(&mut reader.$field).into_iter().filter(|i| *i < keep).map(|i| i + count).collect();)* }; }
    macro_rules! shift_map { ($($field:ident),*) => { $(reader.$field = std::mem::take(&mut reader.$field).into_iter().filter(|(i, _)| *i < keep).map(|(i, value)| (i + count, value)).collect();)* }; }
    shift_set!(retained, desired, failed, budget_blocked);
    shift_map!(
        decoded_bytes,
        decoded_widths,
        decoded_windows,
        decoded_responses,
        native_dimensions,
        presentations,
        image_retries
    );
    reader.prune_retry_deadlines();
    reader.continuity_failures.remove(&prepared_chapter);
    if reader
        .continuity_error
        .as_ref()
        .is_some_and(|(chapter, _)| chapter == &prepared_chapter)
    {
        reader.continuity_error = None;
    }
    for index in 0..count {
        rows.insert(
            index,
            PageRow {
                page_id: reader.row_page_id(index).into(),
                ratio: reader.ratios[index],
                position: reader.page_position(index),
                ..Default::default()
            },
        );
    }
    for index in count..reader.pages.len() {
        if let Some(mut row) = rows.row_data(index) {
            row.position = reader.page_position(index);
            rows.set_row_data(index, row);
        }
    }
    -reader.page_position(count)
}

impl Controller {
    /// Prefetch chapter metadata near either boundary in every reading mode.
    /// Pixel prefetch continues
    /// to obey the existing final-window and 32 MiB display admission policy.
    pub(super) fn maybe_preload_next(self: &Arc<Self>, ui: &AppWindow) {
        let generation = self.generation.load(Ordering::Acquire);
        let request = {
            let mut view = self.view();
            if view.section != 6 {
                return;
            }
            let Some(reader) = view.reader.as_mut() else {
                return;
            };
            if reader.sync_continuation_epoch(generation) {
                ui.set_reader_continuation_error("".into());
            }
            if reader.collection || reader.mode_switching || reader.pages.is_empty() {
                return;
            }
            let candidates = view
                .reader
                .as_ref()
                .unwrap()
                .continuation_candidates(&view.chapters);
            let reader = view.reader.as_mut().unwrap();
            let mut failure = None;
            let candidate = candidates.into_iter().find(|(_, chapter, _, _)| {
                if let Some(message) = reader.continuity_failures.get(chapter) {
                    if failure.is_none() {
                        failure = Some((chapter.clone(), message.clone()));
                    }
                    false
                } else {
                    true
                }
            });
            let Some((previous, chapter, title, backwards)) = candidate else {
                reader.continuity_error = failure.clone();
                ui.set_reader_continuation_error(
                    failure
                        .map(|(_, message)| message)
                        .unwrap_or_default()
                        .into(),
                );
                return;
            };
            if let Some(active) = reader.preloading.as_ref() {
                let explicit = reader
                    .navigation_metadata_target()
                    .is_some_and(|(id, _)| id == &chapter);
                if active == &chapter || (backwards != reader.continuity_backwards && !explicit) {
                    return;
                }
                // One logical metadata slot: a newly demanded edge supersedes
                // an opposite speculative job. Cooperative source cancellation
                // and the nonce reject its late completion independently.
                self.continuation_work.cancel();
            }
            reader.preload_nonce = reader.preload_nonce.wrapping_add(1);
            let nonce = reader.preload_nonce;
            reader.preloading = Some(chapter.clone());
            let cancel = CancelToken::default();
            (
                previous, chapter, title, generation, backwards, nonce, cancel,
            )
        };
        let (previous, chapter, title, generation, backwards, nonce, cancel) = request;
        let services = self.services.clone();
        let app = Arc::downgrade(self);
        let weak = ui.as_weak();
        self.continuation_work.submit(
            cancel.clone(),
            Box::new(move || {
                let result = prepare_chapter(&services, chapter.clone(), title, &cancel);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let Some(app) = app.upgrade() else {
                        return;
                    };
                    if app.generation.load(Ordering::Acquire) != generation {
                        return;
                    }
                    let mut view = app.view();
                    if view.section != 6 {
                        return;
                    }
                    let Some(reader) = view.reader.as_mut() else {
                        return;
                    };
                    if reader.preloading.as_ref() != Some(&chapter)
                        || reader.preload_nonce != nonce
                        || (if backwards {
                            reader.segments.first()
                        } else {
                            reader.segments.last()
                        })
                        .is_none_or(|segment| segment.chapter != previous)
                    {
                        return;
                    }
                    reader.preloading = None;
                    let result = result.and_then(|prepared| {
                        if prepared
                            .source
                            .as_ref()
                            .is_none_or(|source| source.availability().is_available())
                        {
                            Ok(prepared)
                        } else {
                            Err(text::tr("Source unavailable"))
                        }
                    });
                    let prepared = match result {
                        Ok(prepared) => prepared,
                        Err(message) => {
                            let message = format!(
                                "{}: {message}",
                                text::tr(if backwards {
                                    "Could not load previous chapter"
                                } else {
                                    "Could not load next chapter"
                                })
                            );
                            reader
                                .continuity_failures
                                .insert(chapter.clone(), message.clone());
                            reader.continuity_error = Some((chapter, message.clone()));
                            ui.set_reader_continuation_error(message.into());
                            drop(view);
                            app.apply_chapter_navigation(&ui);
                            // A failed speculative direction cannot leave the other
                            // eligible edge waiting for a gesture or image event.
                            app.maybe_preload_next(&ui);
                            return;
                        }
                    };
                    let Some(remove) = (if backwards {
                        reader.prune_for_prepend()
                    } else {
                        reader.prune_for_append()
                    }) else {
                        return;
                    };
                    let model = ui.get_pages();
                    let Some(rows) = model.as_any().downcast_ref::<VecModel<PageRow>>() else {
                        return;
                    };
                    if rows.row_count() != reader.pages.len() {
                        return;
                    }
                    let old_mode_switching = reader.mode_switching;
                    reader.mode_switching = true;
                    if backwards || remove > 0 {
                        reader.progressive_job = None;
                        // Old image tasks carry absolute list indices. Change their
                        // generation before rebasing, then retain surviving pixels.
                        app.invalidate();
                        reader.preload_epoch = app.generation.load(Ordering::Acquire);
                        app.active_image
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .take();
                    }
                    let shift = if backwards {
                        prepend_rows(reader, rows, prepared, remove)
                    } else {
                        append_rows(reader, rows, prepared, remove)
                    };
                    ui.set_total_ratio(reader.total_extent());
                    ui.set_current_page(reader.anchor.page_index as i32);
                    let webtoon = reader.mode == ReadingMode::Webtoon;
                    let anchor = reader.anchor.page_index;
                    if webtoon && shift != 0. {
                        ui.set_scroll_y(ui.get_scroll_y() + shift * ui.get_reader_width());
                    }
                    reader.mode_switching = old_mode_switching;
                    drop(view);
                    ui.set_reader_continuation_error("".into());
                    // The scroll shift above fires Slint's position callback while mode
                    // switching still suppressed it. Re-derive the anchor from the
                    // adjusted scroll so the visual page cannot drift into a
                    // neighbouring chapter (especially a backward prepend).
                    if webtoon {
                        app.position_internal(
                            &ui,
                            -ui.get_scroll_y(),
                            ui.get_reader_width(),
                            ui.get_reader_viewport_height(),
                            PositionOrigin::Restoration,
                        );
                    }
                    app.apply_chapter_navigation(&ui);
                    app.publish_reader_controls(&ui);
                    let anchor = app
                        .view()
                        .reader
                        .as_ref()
                        .map_or(anchor, |r| r.anchor.page_index);
                    app.request_images(&ui, anchor);
                    // The opposite neighbor may also be needed on initial open;
                    // do not require a second scroll just to start its preparation.
                    app.maybe_preload_next(&ui);
                });
            }),
        );
    }
    pub(super) fn retry_continuation(self: &Arc<Self>, ui: &AppWindow) {
        let mut view = self.view();
        if let Some(reader) = view.reader.as_mut()
            && let Some((chapter, _)) = reader.continuity_error.take()
        {
            reader.continuity_failures.remove(&chapter);
        }
        drop(view);
        ui.set_reader_continuation_error("".into());
        self.maybe_preload_next(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_core::model::{ModelChangeListener, ModelChangeListenerContainer};
    use std::{cell::Cell, pin::Pin, time::Duration};

    fn reader() -> Reader {
        let mut reader = super::super::tests::tall_reader();
        reader.pages = (0..6)
            .map(|index| PageInput::Local {
                path: PathBuf::new(),
                id: format!("canonical-{}", index % 2),
            })
            .collect();
        reader.ratios = vec![1.; 6];
        reader.viewport_width = 400.;
        reader.viewport_height = 600.;
        reader.anchor = ResumeAnchor::new("canonical-0", 4, 0.375).unwrap();
        reader.segments = (0..3)
            .map(|index| ChapterSegment {
                chapter: ChapterId::new(reader.chapter.series.clone(), format!("chapter-{index}")),
                title: format!("Chapter {index}"),
                start: index * 2,
                end: index * 2 + 2,
                source: None,
                completed: false,
            })
            .collect();
        reader.chapter = reader.segments[2].chapter.clone();
        reader
    }
    fn next(reader: &Reader) -> PreparedChapter {
        PreparedChapter {
            chapter: ChapterId::new(reader.chapter.series.clone(), "chapter-3"),
            title: "Chapter 3".into(),
            source: None,
            pages: (0..2)
                .map(|index| PageInput::Local {
                    path: PathBuf::new(),
                    id: format!("canonical-{index}"),
                })
                .collect(),
            ratios: vec![1.; 2],
            completed: false,
        }
    }
    #[derive(Default)]
    struct Changes {
        added: Cell<usize>,
        removed: Cell<usize>,
        resets: Cell<usize>,
    }
    impl ModelChangeListener for Changes {
        fn row_changed(self: Pin<&Self>, _: usize) {}
        fn row_added(self: Pin<&Self>, _: usize, count: usize) {
            self.added.set(self.added.get() + count);
        }
        fn row_removed(self: Pin<&Self>, _: usize, count: usize) {
            self.removed.set(self.removed.get() + count);
        }
        fn reset(self: Pin<&Self>) {
            self.resets.set(self.resets.get() + 1);
        }
    }
    #[test]
    fn scrolling_back_into_a_read_chapter_keeps_its_place_for_history() {
        const CHILD: &str = "READER_READ_CHAPTER_KEEPS_PLACE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_continuity::tests::scrolling_back_into_a_read_chapter_keeps_its_place_for_history", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        crate::reader_settings::init().unwrap();
        crate::reader_settings::update("fullscreen", 0).unwrap();
        let series = reader_domain::SeriesId::new("local", "natural-order");
        let chapters = [1, 2, 3, 10].map(|number| reader_domain::Chapter {
            id: ChapterId::new(series.clone(), format!("opaque-{number}")),
            title: format!("Chapter {number}"),
            number: Some(number.to_string()),
            source_order: number,
            label: String::new(),
            volume: None,
            uploaded_at: Some(100 - number),
            language: None,
            scanlator: None,
        });
        let folder = dir.path().join("pages");
        std::fs::create_dir(&folder).unwrap();
        for page in [1, 2] {
            image::RgbaImage::from_pixel(10, 20, image::Rgba([30, 80, 210, 255]))
                .save(folder.join(format!("{page}.png")))
                .unwrap();
        }
        for chapter in &chapters {
            services
                .downloads
                .import_folder(chapter.id.clone(), &folder)
                .unwrap();
        }
        services
            .domain
            .update(|state| {
                state.series.push(reader_domain::Series {
                    id: series.clone(),
                    title: "Natural order".into(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                    publication_status: Default::default(),
                });
                state.chapters = chapters.iter().rev().cloned().collect();
                Ok(())
            })
            .unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(1000., 600.));
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
                dir.path().join("cache"),
            )),
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::new(View::default()),
            generation: AtomicU64::new(0),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        fn pump_until(mut ready: impl FnMut() -> bool + 'static) {
            let timer = slint::Timer::default();
            let start = std::time::Instant::now();
            timer.start(
                slint::TimerMode::Repeated,
                Duration::from_millis(5),
                move || {
                    if ready() || start.elapsed() > Duration::from_secs(5) {
                        slint::quit_event_loop().unwrap();
                    }
                },
            );
            slint::run_event_loop().unwrap();
            timer.stop();
        }
        crate::reader_modes::save(&series, ModeChoice::Manual(ReadingMode::Webtoon)).unwrap();
        // Chapters 1 and 2 were read before; the user now re-reads 2 and scrolls
        // up into 1, then later forward into 3 (never read).
        app.services
            .domain
            .update(|state| {
                for chapter in &chapters[..2] {
                    state.set_progress(ReadingProgress {
                        chapter: chapter.id.clone(),
                        anchor: ResumeAnchor::cleared(),
                        mode: ReadingMode::Webtoon,
                        last_read_at: now() - 100,
                        completed: true,
                    });
                }
                Ok(())
            })
            .unwrap();
        app.open_chapter(&ui, chapters[1].id.clone());
        let wait = app.clone();
        pump_until(move || {
            wait.view()
                .reader
                .as_ref()
                .is_some_and(|reader| reader.segments.len() == 3 && reader.preloading.is_none())
        });
        let rows = ui.get_pages();
        for index in 0..rows.row_count() {
            let mut row = rows.row_data(index).unwrap();
            row.loaded = true;
            rows.set_row_data(index, row);
        }
        let width = ui.get_reader_width();
        // Half way down chapter 1's first image, from inside chapter 2.
        app.position(&ui, 0.5 * 2. * width, width, 600.);
        {
            let view = app.view();
            let r = view.reader.as_ref().unwrap();
            assert_eq!(r.chapter, chapters[0].id);
            assert_eq!(r.local_index(r.anchor.page_index), 0);
        }
        app.flush();
        let stored = app
            .services
            .domain
            .snapshot()
            .unwrap()
            .progress
            .into_iter()
            .find(|p| p.chapter == chapters[0].id)
            .unwrap();
        assert!(stored.completed, "reading again keeps the read flag");
        assert!(
            !stored.anchor.is_cleared()
                && stored.anchor.page_index == 0
                && stored.anchor.offset() > 0.2,
            "the place reached in a read chapter is stored: {:?}",
            stored.anchor
        );
        // Opening it the History way restores that place.
        app.open_chapter(&ui, chapters[0].id.clone());
        let wait = app.clone();
        let target = chapters[0].id.clone();
        pump_until(move || {
            wait.view()
                .reader
                .as_ref()
                .is_some_and(|r| r.chapter == target && r.preloading.is_none())
        });
        {
            let view = app.view();
            let r = view.reader.as_ref().unwrap();
            assert_eq!(r.local_index(r.anchor.page_index), stored.anchor.page_index);
            assert!((r.anchor.offset() - stored.anchor.offset()).abs() < 0.01);
        }
        // Symmetry: an unread chapter entered going forward stores its place.
        app.open_chapter(&ui, chapters[1].id.clone());
        let wait = app.clone();
        pump_until(move || {
            wait.view()
                .reader
                .as_ref()
                .is_some_and(|reader| reader.segments.len() == 3 && reader.preloading.is_none())
        });
        let rows = ui.get_pages();
        for index in 0..rows.row_count() {
            let mut row = rows.row_data(index).unwrap();
            row.loaded = true;
            rows.set_row_data(index, row);
        }
        let width = ui.get_reader_width();
        let into_next = {
            let view = app.view();
            let r = view.reader.as_ref().unwrap();
            let start = r.segments[2].start;
            (r.page_position(start) + 0.5 * r.ratios[start]) * width
        };
        app.position(&ui, into_next, width, 600.);
        app.flush();
        let next = app
            .services
            .domain
            .snapshot()
            .unwrap()
            .progress
            .into_iter()
            .find(|p| p.chapter == chapters[2].id)
            .unwrap();
        assert!(!next.completed);
        assert!(!next.anchor.is_cleared() && next.anchor.offset() > 0.2);
        // Detail's "Read again" still starts the read chapter over.
        app.view().reread = Some(chapters[0].id.clone());
        app.open_chapter(&ui, chapters[0].id.clone());
        let wait = app.clone();
        let target = chapters[0].id.clone();
        pump_until(move || {
            wait.view()
                .reader
                .as_ref()
                .is_some_and(|r| r.chapter == target && r.preloading.is_none())
        });
        {
            let view = app.view();
            let r = view.reader.as_ref().unwrap();
            assert_eq!(r.local_index(r.anchor.page_index), 0);
            assert_eq!(r.anchor.offset(), 0.);
        }
    }

    #[test]
    fn direct_middle_chapter_opens_prepare_both_directions_in_every_reader_mode() {
        const CHILD: &str = "READER_BIDIRECTIONAL_CHAPTER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_continuity::tests::direct_middle_chapter_opens_prepare_both_directions_in_every_reader_mode", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        crate::reader_settings::init().unwrap();
        crate::reader_settings::update("fullscreen", 0).unwrap();
        let series = reader_domain::SeriesId::new("local", "natural-order");
        let chapters = [1, 2, 3, 10].map(|number| reader_domain::Chapter {
            id: ChapterId::new(series.clone(), format!("opaque-{number}")),
            title: format!("Chapter {number}"),
            number: Some(number.to_string()),
            source_order: number,
            label: String::new(),
            volume: None,
            uploaded_at: Some(100 - number),
            language: None,
            scanlator: None,
        });
        let folder = dir.path().join("pages");
        std::fs::create_dir(&folder).unwrap();
        for page in [1, 2] {
            image::RgbaImage::from_pixel(10, 20, image::Rgba([30, 80, 210, 255]))
                .save(folder.join(format!("{page}.png")))
                .unwrap();
        }
        for chapter in &chapters {
            services
                .downloads
                .import_folder(chapter.id.clone(), &folder)
                .unwrap();
        }
        services
            .domain
            .update(|state| {
                state.series.push(reader_domain::Series {
                    id: series.clone(),
                    title: "Natural order".into(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                    publication_status: Default::default(),
                });
                state.chapters = chapters.iter().rev().cloned().collect();
                Ok(())
            })
            .unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(1000., 600.));
        let (images, receiver) = mpsc::channel();
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
                dir.path().join("cache"),
            )),
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::new(View::default()),
            generation: AtomicU64::new(0),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        fn pump_until(mut ready: impl FnMut() -> bool + 'static) {
            let timer = slint::Timer::default();
            let start = std::time::Instant::now();
            timer.start(
                slint::TimerMode::Repeated,
                Duration::from_millis(5),
                move || {
                    if ready() || start.elapsed() > Duration::from_secs(5) {
                        slint::quit_event_loop().unwrap();
                    }
                },
            );
            slint::run_event_loop().unwrap();
            timer.stop();
        }
        for mode in [
            ReadingMode::Webtoon,
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            for descending in [true, false] {
                crate::reading_preferences::save_chapter_order(&ui, &series, descending).unwrap();
                crate::reader_modes::save(&series, ModeChoice::Manual(mode)).unwrap();
                app.flush();
                app.services
                    .domain
                    .update(|state| {
                        state.set_progress(ReadingProgress {
                            chapter: chapters[2].id.clone(),
                            anchor: ResumeAnchor::new("1", 1, 0.75)?,
                            mode,
                            last_read_at: now() - 100,
                            completed: false,
                        });
                        Ok(())
                    })
                    .unwrap();
                app.open_chapter(&ui, chapters[1].id.clone());
                let wait = app.clone();
                pump_until(move || {
                    wait.view().reader.as_ref().is_some_and(|reader| {
                        reader.segments.len() == 3 && reader.preloading.is_none()
                    })
                });
                let current = app.view().reader.as_ref().unwrap().anchor.page_index;
                let generation = app.generation.load(Ordering::Acquire);
                assert!(
                    receiver
                        .try_iter()
                        .any(|task| task.generation == generation && task.index == current),
                    "attachment must explicitly restart current-generation pixels in {mode:?}"
                );
                {
                    let view = app.view();
                    let reader = view.reader.as_ref().expect("opened chapter 2");
                    assert_eq!(
                        reader
                            .segments
                            .iter()
                            .map(|s| s.chapter.clone())
                            .collect::<Vec<_>>(),
                        chapters[..3]
                            .iter()
                            .map(|c| c.id.clone())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(reader.chapter, chapters[1].id);
                    assert_eq!(reader.mode, mode);
                }
                if mode == ReadingMode::Webtoon {
                    app.reader_seek(&ui, 0.);
                    let before = app.view().reader.as_ref().unwrap().progress_record();
                    app.position(&ui, 90000., ui.get_reader_width(), 600.);
                    let view = app.view();
                    let r = view.reader.as_ref().unwrap();
                    assert_eq!(r.progress_record().chapter, before.chapter);
                    assert_eq!(
                        r.progress_record().anchor,
                        before.anchor,
                        "scrolling through blank estimates must not skip unread content"
                    );
                    assert!(
                        (-ui.get_scroll_y()
                            - r.page_position(r.anchor.page_index) * ui.get_reader_width())
                        .abs()
                            < 1.
                    );
                }
                let rows = ui.get_pages();
                for index in 0..rows.row_count() {
                    let mut row = rows.row_data(index).unwrap();
                    row.loaded = true;
                    rows.set_row_data(index, row);
                }
                // Back across chapter 2's first page reaches chapter 1's last
                // page, never chapter 3 or chapter 10 from display/text sorting.
                app.reader_seek(&ui, 0.);
                if mode == ReadingMode::Webtoon {
                    let previous = {
                        let view = app.view();
                        let r = view.reader.as_ref().unwrap();
                        (r.page_position(1) + 0.5) * ui.get_reader_width()
                    };
                    app.position(&ui, previous, ui.get_reader_width(), 600.);
                } else {
                    app.step_page(&ui, -1);
                }
                {
                    let view = app.view();
                    let r = view.reader.as_ref().unwrap();
                    assert_eq!(r.chapter, chapters[0].id);
                    assert_eq!(r.local_index(r.anchor.page_index), 1);
                }
                // Return to chapter 2, then cross its bottom/last page to 3.
                let start = app.view().reader.as_ref().unwrap().segments[1].start;
                if mode == ReadingMode::Webtoon {
                    let offset = app.view().reader.as_ref().unwrap().page_position(start)
                        * ui.get_reader_width();
                    app.position(&ui, offset, ui.get_reader_width(), 600.);
                    let offset = app.view().reader.as_ref().unwrap().page_position(start + 2)
                        * ui.get_reader_width();
                    app.position(&ui, offset + 1., ui.get_reader_width(), 600.);
                } else {
                    app.step_page(&ui, 1);
                    app.step_page(&ui, 1);
                    app.step_page(&ui, 1);
                }
                {
                    let view = app.view();
                    let r = view.reader.as_ref().unwrap();
                    assert_eq!(
                        r.chapter, chapters[2].id,
                        "mode={mode:?}, descending={descending}"
                    );
                    assert_eq!(r.local_index(r.anchor.page_index), 0);
                }
                app.flush();
                let saved = app.services.domain.snapshot().unwrap();
                let destination = saved
                    .progress
                    .iter()
                    .filter(|p| p.chapter == chapters[2].id)
                    .collect::<Vec<_>>();
                assert_eq!(destination.len(), 1);
                assert_eq!(
                    destination[0].anchor.page_index, 0,
                    "this visit replaces the old later resume, not a maximum/farthest position"
                );
                assert!(destination[0].anchor.offset() < 0.01);
            }
        }
        app.flush();
        let old_anchor = ResumeAnchor::new("1", 1, 0.5).unwrap();
        app.services
            .domain
            .update(|state| {
                state.set_progress(ReadingProgress {
                    chapter: chapters[1].id.clone(),
                    anchor: old_anchor.clone(),
                    mode: ReadingMode::Webtoon,
                    last_read_at: now(),
                    completed: true,
                });
                Ok(())
            })
            .unwrap();
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
        }
        ui.set_section(5);
        app.render(&ui);
        let row = app
            .view()
            .rows
            .iter()
            .position(|row| matches!(row, Row::Chapter(chapter) if chapter.id == chapters[1].id))
            .unwrap();
        let history = app.services.domain.snapshot().unwrap().history;
        app.item_action(&ui, row, "clear-progress");
        let cleared = app.services.domain.snapshot().unwrap();
        let progress = cleared
            .progress
            .iter()
            .find(|p| p.chapter == chapters[1].id)
            .unwrap();
        assert_eq!(progress.anchor.page_index, 0);
        assert_eq!(progress.anchor.offset(), 0.);
        assert!(
            progress.completed,
            "clearing resume is separate from marking unread"
        );
        assert_eq!(cleared.history, history);
        {
            let owner = nova_sync::local_store().unwrap();
            let store = owner.lock().unwrap();
            let key = serde_json::to_string(&chapters[1].id).unwrap();
            let record = store.record(&crate::sync::domain("resume"), &key).unwrap();
            let shared: ReadingProgress =
                serde_json::from_str(record.value.as_deref().unwrap()).unwrap();
            assert_eq!(shared.anchor.page_index, 0);
            assert!(
                shared.completed,
                "synced resume reset must preserve completion too"
            );
        }
        app.open_chapter(&ui, chapters[1].id.clone());
        let wait = app.clone();
        let chapter = chapters[1].id.clone();
        pump_until(move || {
            wait.view()
                .reader
                .as_ref()
                .is_some_and(|r| r.chapter == chapter && r.preloading.is_none())
        });
        {
            let view = app.view();
            let r = view.reader.as_ref().unwrap();
            assert_eq!(r.local_index(r.anchor.page_index), 0);
        }
        app.flush();
        app.view().section = 5;
        ui.set_section(5);
        let history = app.services.domain.snapshot().unwrap().history;
        let key = serde_json::to_string(&series).unwrap();
        // Give several chapters distinct saved pages, then clear them through
        // the entry menu in one edit.
        app.services
            .domain
            .update(|state| {
                for (index, chapter) in chapters.iter().enumerate() {
                    if let Some(progress) =
                        state.progress.iter_mut().find(|p| p.chapter == chapter.id)
                    {
                        progress.anchor = ResumeAnchor::new("1", 1, 0.5 * index as f64)?;
                    }
                }
                Ok(())
            })
            .unwrap();
        let completed_before = app
            .services
            .domain
            .snapshot()
            .unwrap()
            .progress
            .iter()
            .filter(|p| p.chapter.series == series)
            .map(|p| (p.chapter.clone(), p.completed))
            .collect::<Vec<_>>();
        app.entry_action(&ui, &key, "clear-all-positions");
        let cleared = app.services.domain.snapshot().unwrap();
        for progress in cleared
            .progress
            .iter()
            .filter(|p| p.chapter.series == series)
        {
            assert!(
                progress.anchor.is_cleared(),
                "cleared chapters have no saved page"
            );
        }
        assert_eq!(
            cleared
                .progress
                .iter()
                .filter(|p| p.chapter.series == series)
                .map(|p| (p.chapter.clone(), p.completed))
                .collect::<Vec<_>>(),
            completed_before,
            "clearing positions never changes read/unread state"
        );
        assert_eq!(cleared.history, history);
        {
            let owner = nova_sync::local_store().unwrap();
            let store = owner.lock().unwrap();
            for chapter in &chapters[..3] {
                let identity = serde_json::to_string(&chapter.id).unwrap();
                let record = store
                    .record(&crate::sync::domain("resume"), &identity)
                    .unwrap();
                let shared: ReadingProgress =
                    serde_json::from_str(record.value.as_deref().unwrap()).unwrap();
                assert!(shared.anchor.is_cleared());
            }
        }
        let anchor = cleared
            .progress
            .iter()
            .find(|p| p.chapter == chapters[1].id)
            .unwrap()
            .anchor
            .clone();
        for (action, completed) in [("mark-all-read", true), ("mark-all-unread", false)] {
            app.entry_action(&ui, &key, action);
            let state = app.services.domain.snapshot().unwrap();
            assert!(
                state
                    .progress
                    .iter()
                    .filter(|p| p.chapter.series == series)
                    .all(|p| p.completed == completed)
            );
            assert_eq!(
                state
                    .progress
                    .iter()
                    .filter(|p| p.chapter.series == series)
                    .count(),
                chapters.len()
            );
            assert_eq!(
                state.history, history,
                "manual batch actions do not invent history"
            );
            assert_eq!(
                state
                    .progress
                    .iter()
                    .find(|p| p.chapter == chapters[1].id)
                    .unwrap()
                    .anchor,
                anchor
            );
        }
    }

    #[test]
    fn source_page_ownership_is_chapter_local_and_ui_ids_do_not_collide() {
        let mut reader = reader();
        assert_eq!(reader.page_id(0), reader.page_id(2));
        assert_ne!(reader.row_page_id(0), reader.row_page_id(2));
        assert_eq!(reader.owner(2), &reader.segments[1].chapter);
        assert_eq!(reader.local_index(4), 0);
        assert_eq!(reader.local_index(5), 1);
        assert_eq!(reader.active_count(), 2);
        assert!(reader.source_for(4).is_none());
        reader.segments.clear();
        assert_eq!(reader.owner(4), &reader.chapter);
        assert_eq!(reader.local_index(4), 4);
        assert_eq!(reader.active_count(), 6);
        reader.initialise_segments("Initial".into());
        reader.initialise_segments("Must not duplicate".into());
        assert_eq!(reader.segments.len(), 1);
        assert_eq!(reader.segments[0].title, "Initial");
    }
    #[test]
    fn segment_sources_keep_downloaded_pages_local_and_share_remote_revocation() {
        use std::io::{Cursor, Write};
        let directory = tempfile::tempdir().unwrap();
        let registry = reader_sources::SourceRegistry::open(directory.path()).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../examples/addons/neutral-fixture/manifest.json"
        ))
        .unwrap();
        manifest["assets"] = json!([]);
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            (
                "main.js",
                include_bytes!("../examples/addons/neutral-fixture/main.js").to_vec(),
            ),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&bytes).unwrap();
        }
        registry
            .install_zip(&archive.finish().unwrap().into_inner())
            .unwrap();
        registry.set_enabled("neutral-fixture", true).unwrap();
        let remote = registry.source("neutral-fixture").unwrap();
        let mut reader = reader();
        reader.source = Some(remote.clone());
        reader.segments[1].source = Some(remote);
        assert!(
            reader.source_for(0).is_none(),
            "downloaded segment must not inherit the active remote source"
        );
        assert_eq!(
            reader.source_for(2).unwrap().descriptor().id,
            "neutral-fixture"
        );
        reader.chapter = reader.segments[0].chapter.clone();
        reader.source = None;
        reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.75).unwrap();
        reader.activate_page(2).unwrap();
        assert!(
            reader.source.is_some(),
            "forward transition activates the remote segment source"
        );
        reader.anchor = ResumeAnchor::new(reader.page_id(2), 2, 0.).unwrap();
        reader.activate_page(1).unwrap();
        assert!(
            reader.source.is_none(),
            "reverse transition restores the downloaded segment source"
        );
        registry.set_enabled("neutral-fixture", false).unwrap();
        assert!(!reader.source_for(2).unwrap().availability().is_available());
        assert!(reader.source_for(0).is_none());
    }
    #[test]
    fn forward_and_reverse_continuation_preserve_canonical_progress_and_completion() {
        let mut reader = reader();
        reader.chapter = reader.segments[0].chapter.clone();
        reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.875).unwrap();
        reader.completed = false;
        let previous = reader.activate_page(2).unwrap();
        assert_eq!(previous.chapter, reader.segments[0].chapter);
        assert!(
            previous.completed,
            "crossing forward finishes the chapter left behind"
        );
        assert!(
            previous.anchor.is_cleared(),
            "a finished chapter keeps no resume page"
        );
        reader.anchor = ResumeAnchor::new(reader.page_id(2), 2, 0.125).unwrap();
        let next = reader.progress_record();
        assert_eq!(next.chapter, reader.segments[1].chapter);
        assert_eq!(next.anchor.page_id, "canonical-0");
        assert_eq!(next.anchor.page_index, 0, "saved indices are chapter local");
        assert_eq!(next.anchor.offset(), 0.125);
        assert!(!next.completed);
        let departing = reader.activate_page(1).unwrap();
        assert_eq!(departing.chapter, reader.segments[1].chapter);
        assert_eq!(departing.anchor.page_index, 0);
        assert_eq!(departing.anchor.page_id, "canonical-0");
        assert!(
            !departing.completed,
            "backtracking must not complete the new chapter"
        );
        reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.75).unwrap();
        let back = reader.progress_record();
        assert_eq!(back.chapter, reader.segments[0].chapter);
        assert!(
            back.completed,
            "already completed retained chapters stay completed when revisited"
        );
        assert!(
            back.anchor.is_cleared(),
            "revisiting a completed chapter still has no resume page"
        );
        assert!(
            reader.activate_page(0).is_none(),
            "same-chapter movement does not manufacture a chapter transition"
        );
    }
    #[test]
    fn prepending_preserves_source_anchor_tiles_and_bounds_reverse_retention() {
        let mut reader = reader();
        reader.chapter = reader.segments[0].chapter.clone();
        reader.anchor = ResumeAnchor::new(reader.page_id(0), 0, 0.25).unwrap();
        let art = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(2, 2));
        let tiles = ModelRc::new(VecModel::from(vec![TileRow {
            image: art.clone(),
            ..Default::default()
        }]));
        let rows = VecModel::from(
            (0..6)
                .map(|index| PageRow {
                    page_id: reader.row_page_id(index).into(),
                    ratio: 1.,
                    position: reader.page_position(index),
                    loaded: true,
                    tiles: tiles.clone(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        );
        let changes = Box::pin(ModelChangeListenerContainer::<Changes>::default());
        rows.model_tracker()
            .attach_peer(changes.as_ref().model_peer());
        reader.retained = BTreeSet::from([0, 1, 5]);
        reader.decoded_bytes = BTreeMap::from([(0, 16), (5, 32)]);
        let mut previous = next(&reader);
        previous.chapter.local_id = "previous".into();
        let keep = reader.prune_for_prepend().unwrap();
        assert_eq!(keep, 4);
        let before = reader.progress_record();
        let shift = prepend_rows(&mut reader, &rows, previous, keep);
        assert_eq!(reader.progress_record().chapter, before.chapter);
        assert_eq!(reader.progress_record().anchor, before.anchor);
        assert_eq!(reader.anchor.page_index, 2);
        assert_eq!(shift, -reader.page_position(2));
        assert_eq!(reader.segments.len(), 3);
        assert_eq!(reader.segments[0].chapter.local_id, "previous");
        assert_eq!(reader.retained, BTreeSet::from([2, 3]));
        assert_eq!(reader.decoded_bytes, BTreeMap::from([(2, 16)]));
        assert_eq!(rows.row_count(), 6);
        assert!(rows.row_data(2).unwrap().tiles == tiles);
        assert_eq!(
            rows.row_data(2).unwrap().tiles.row_data(0).unwrap().image,
            art
        );
        assert_eq!(changes.added.get(), 2);
        assert_eq!(changes.removed.get(), 2);
        assert_eq!(changes.resets.get(), 0);
        // A tall viewport reaching the last chapter prevents its eviction.
        reader.viewport_height = 5000.;
        assert_eq!(reader.prune_for_prepend(), Some(reader.pages.len()));
    }

    #[test]
    fn stable_retry_deadlines_survive_rebase_with_attempts_and_drop_pruned_pages() {
        let mut reader = reader();
        let due = std::time::Instant::now() + Duration::from_secs(5);
        let identity = reader.row_page_id(4);
        let pruned = reader.row_page_id(0);
        reader.failed.extend([0, 3, 4]);
        reader.image_retries.insert(4, 2);
        reader.image_retry_due.insert(identity.clone(), due);
        reader.image_retry_due.insert(pruned.clone(), due);
        reader.prune_prefix(2);
        reader.prune_retry_deadlines();
        assert_eq!(reader.retry_index(&identity, due), Some(2));
        assert_eq!(reader.image_retries[&2], 2);
        assert_eq!(reader.image_retry_due[&identity], due);
        assert!(!reader.image_retry_due.contains_key(&pruned));
        assert!(
            reader.failed.contains(&1),
            "terminal failures survive without a retry ticket"
        );
        let rows = VecModel::from(
            (0..reader.pages.len())
                .map(|_| PageRow::default())
                .collect::<Vec<_>>(),
        );
        let previous = next(&reader);
        let keep = reader.pages.len();
        prepend_rows(&mut reader, &rows, previous, keep);
        assert_eq!(reader.retry_index(&identity, due), Some(4));
        assert_eq!(reader.image_retries[&4], 2);
        assert_eq!(reader.image_retry_due[&identity], due);
        assert!(
            reader
                .retry_index(&identity, due + Duration::from_millis(1))
                .is_none()
        );
    }

    #[test]
    fn pending_retry_timer_delivers_once_after_rebase_and_overview_with_original_deadline() {
        const CHILD: &str = "READER_REBOUND_RETRY_TIMER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_continuity::tests::pending_retry_timer_delivers_once_after_rebase_and_overview_with_original_deadline", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let ui = AppWindow::new().unwrap();
        ui.set_section(6);
        ui.window().set_size(slint::LogicalSize::new(400., 600.));
        let dir = tempfile::tempdir().unwrap();
        let mut reader = reader();
        reader.failed = (0..6).collect();
        reader.image_retries.insert(4, 2);
        let identity = reader.row_page_id(4);
        let due = std::time::Instant::now() + Duration::from_millis(300);
        reader.image_retry_due.insert(identity.clone(), due);
        let (app, receiver) =
            super::super::reader_handoff_tests::fixture_app(&ui, dir.path(), reader, false);
        app.schedule_image_retry(&ui, identity.clone(), due);
        let generation = app.invalidate().0;
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.prune_prefix(2);
            reader.prune_retry_deadlines();
            assert_eq!(reader.retry_index(&identity, due), Some(2));
            assert_eq!(reader.image_retries[&2], 2);
            view.section = 23;
        }
        ui.set_section(23);
        app.mount_pages(&ui);
        let owner = app.clone();
        let weak = ui.as_weak();
        let resume = slint::Timer::default();
        resume.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(30),
            move || {
                let ui = weak.upgrade().unwrap();
                owner.view().section = 6;
                ui.set_section(6);
                owner.resume_image_retries(&ui);
                owner.resume_image_retries(&ui);
            },
        );
        let delivered = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = delivered.clone();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(5),
            move || {
                for task in receiver.try_iter() {
                    assert!(
                        std::time::Instant::now() >= due,
                        "rebase/return cannot restart or shorten remaining backoff"
                    );
                    observed.borrow_mut().push((task.generation, task.index));
                }
                if std::time::Instant::now() >= due + Duration::from_millis(100) {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        slint::run_event_loop().unwrap();
        timer.stop();
        resume.stop();
        assert_eq!(*delivered.borrow(), vec![(generation, 2)]);
        let view = app.view();
        let reader = view.reader.as_ref().unwrap();
        assert_eq!(reader.image_retries[&2], 2);
        assert!(!reader.image_retry_due.contains_key(&identity));
        assert!(!reader.failed.contains(&2));
    }

    #[test]
    fn generation_changes_clear_old_metadata_markers_and_allow_retry() {
        let mut reader = reader();
        reader.sync_continuation_epoch(7);
        reader.preloading = Some(reader.chapter.clone());
        reader.continuity_error = Some((reader.chapter.clone(), "failed".into()));
        reader.sync_continuation_epoch(7);
        assert!(reader.preloading.is_some());
        reader.mode = ReadingMode::LeftToRight;
        reader.sync_continuation_epoch(8);
        assert!(
            reader.preloading.is_none(),
            "mode changes with a new image epoch must not leave a blocked metadata marker"
        );
        assert!(reader.continuity_error.is_none());
        assert_eq!(reader.preload_epoch, 8);
    }
    #[test]
    fn append_and_bounded_prune_preserve_visible_tiles_pixels_and_anchor() {
        let mut reader = reader();
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(2, 2);
        pixels.make_mut_bytes().fill(117);
        let original_pixels = pixels.as_bytes().as_ptr();
        let art = slint::Image::from_rgba8(pixels);
        let tiles = ModelRc::new(VecModel::from(vec![TileRow {
            image: art.clone(),
            ..Default::default()
        }]));
        let rows = VecModel::from(
            (0..6)
                .map(|index| PageRow {
                    page_id: reader.row_page_id(index).into(),
                    ratio: 1.,
                    position: reader.page_position(index),
                    tiles: tiles.clone(),
                    loaded: true,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        );
        let changes = Box::pin(ModelChangeListenerContainer::<Changes>::default());
        rows.model_tracker()
            .attach_peer(changes.as_ref().model_peer());
        let encoded = crate::page_store::PageImage::from_encoded(vec![7_u8; 16]);
        reader.requested = BTreeSet::from([0, 4, 5]);
        reader.retained = BTreeSet::from([0, 4, 5]);
        reader.desired = BTreeSet::from([3, 4, 5]);
        reader.failed = BTreeSet::from([1, 3]);
        reader.budget_blocked = BTreeSet::from([0, 5]);
        reader.desired_anchor = 4;
        reader.decoded_bytes = BTreeMap::from([(0, 16), (4, 32)]);
        reader.decoded_widths = BTreeMap::from([(4, 400)]);
        reader.decoded_windows = BTreeMap::from([(
            4,
            reader_engine::DisplayWindowPlan {
                width: 4,
                full_height: 8,
                first_row: 0,
                rows: 8,
            },
        )]);
        reader.decoded_responses = BTreeMap::from([(4, encoded.downgrade())]);
        reader.native_dimensions = BTreeMap::from([(
            4,
            reader_engine::PageDimensions {
                width: 4,
                height: 8,
            },
        )]);
        let presentation = reader_engine::DisplayDecoder::default()
            .presentation(
                &Arc::new(
                    include_bytes!("../crates/reader-engine/tests/fixtures/tiny.avif").to_vec(),
                ),
                reader_engine::DecodeLimits::default(),
                reader_engine::DisplayOptions::default(),
            )
            .unwrap();
        reader.presentations.insert(4, presentation);
        let before = reader.page_position(4) + reader.ratios[4] * reader.anchor.offset() as f32;
        let anchor_id = reader.anchor.page_id.clone();
        let prepared = next(&reader);
        let remove = reader.prune_for_append().unwrap();
        assert_eq!(remove, 2);
        let shift = append_rows(&mut reader, &rows, prepared, remove);
        assert_eq!(reader.segments.len(), 3);
        assert_eq!(reader.segments[0].chapter.local_id, "chapter-1");
        assert_eq!(reader.segments[2].chapter.local_id, "chapter-3");
        assert_eq!(reader.anchor.page_index, 2);
        assert_eq!(reader.anchor.page_id, anchor_id);
        assert_eq!(reader.anchor.offset(), 0.375);
        assert!((before - shift - (reader.page_position(2) + 0.375)).abs() < 0.0001);
        assert!(
            reader.requested.is_empty(),
            "old-generation requests must be rescheduled"
        );
        assert_eq!(reader.retained, BTreeSet::from([2, 3]));
        assert_eq!(reader.desired, BTreeSet::from([1, 2, 3]));
        assert_eq!(reader.failed, BTreeSet::from([1]));
        assert_eq!(reader.budget_blocked, BTreeSet::from([3]));
        assert_eq!(reader.decoded_bytes, BTreeMap::from([(2, 32)]));
        assert!(reader.decoded_widths.contains_key(&2));
        assert!(reader.decoded_windows.contains_key(&2));
        assert!(reader.native_dimensions.contains_key(&2));
        assert_eq!(reader.presentations.get(&2), Some(&presentation));
        assert!(
            reader.decoded_responses[&2]
                .upgrade()
                .unwrap()
                .ptr_eq(&encoded)
        );
        assert_eq!(rows.row_count(), 6);
        for index in 0..4 {
            assert!(rows.row_data(index).unwrap().tiles == tiles);
            assert!(rows.row_data(index).unwrap().loaded);
            let retained_image = rows
                .row_data(index)
                .unwrap()
                .tiles
                .row_data(0)
                .unwrap()
                .image;
            assert_eq!(retained_image, art);
            assert_eq!(
                retained_image.to_rgba8().unwrap().as_bytes().as_ptr(),
                original_pixels,
                "pruning must retain the same pixel allocation"
            );
        }
        for index in 4..6 {
            assert!(!rows.row_data(index).unwrap().loaded);
        }
        assert_eq!(changes.added.get(), 2);
        assert_eq!(changes.removed.get(), 2);
        assert_eq!(
            changes.resets.get(),
            0,
            "continuous growth must not reset delegates"
        );
        assert!(
            task_window_request(
                &reader,
                2,
                &ImageTask {
                    generation: 1,
                    index: 4
                },
                reader_engine::PageDimensions {
                    width: 400,
                    height: 400
                }
            )
            .is_err()
        );
    }
    /// Next-chapter metadata (an inventory, possibly a network round trip) must
    /// be in hand by the time the read-ahead wants that chapter's first pixels,
    /// so it starts a lead ahead of them: pixels read two viewport heights (or
    /// two pages) ahead, metadata two more.
    #[test]
    fn next_chapter_metadata_starts_ahead_of_the_pixel_read_ahead() {
        let mut reader = reader();
        reader.pages = (0..12)
            .map(|index| PageInput::Local {
                path: PathBuf::new(),
                id: format!("page-{index}"),
            })
            .collect();
        reader.ratios = vec![1.; 12];
        reader.segments.clear();
        reader.initialise_segments("One chapter".into());
        let (tail, head) = (reader.segments[0].clone(), reader.segments[0].clone());
        // 400 px pages in a 600 px viewport: 4.5 pages left is 3 viewports.
        reader.anchor = ResumeAnchor::new("page-7", 7, 0.5).unwrap();
        assert!(
            reader.near_continuation(&tail),
            "metadata is requested two viewports before the pixel read-ahead reaches it"
        );
        reader.anchor = ResumeAnchor::new("page-2", 2, 0.).unwrap();
        assert!(
            !reader.near_continuation(&tail),
            "ten pages away is too early"
        );
        // Backwards: the same lead from the first page.
        reader.anchor = ResumeAnchor::new("page-4", 4, 0.5).unwrap();
        assert!(reader.near_previous(&head));
        reader.anchor = ResumeAnchor::new("page-9", 9, 0.).unwrap();
        assert!(!reader.near_previous(&head));
        // Paged modes flip a page at a time: three from the end is near.
        for mode in [
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            reader.mode = mode;
            reader.anchor = ResumeAnchor::new("page-9", 9, 0.).unwrap();
            assert!(reader.near_continuation(&tail), "{mode:?}");
            reader.anchor = ResumeAnchor::new("page-8", 8, 0.).unwrap();
            assert!(!reader.near_continuation(&tail), "{mode:?}");
            reader.anchor = ResumeAnchor::new("page-2", 2, 0.).unwrap();
            assert!(reader.near_previous(&head), "{mode:?}");
            reader.anchor = ResumeAnchor::new("page-3", 3, 0.).unwrap();
            assert!(!reader.near_previous(&head), "{mode:?}");
        }
    }

    #[test]
    fn prefetch_uses_last_three_source_pages_or_four_viewports_and_pruning_protects_visible_pages()
    {
        let mut reader = reader();
        reader.segments.clear();
        reader.initialise_segments("One chapter".into());
        let segment = reader.segments[0].clone();
        reader.viewport_height = 500.;
        reader.anchor = ResumeAnchor::new(reader.page_id(0), 0, 0.).unwrap();
        assert!(!reader.near_continuation(&segment));
        reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.).unwrap();
        assert!(
            reader.near_continuation(&segment),
            "five page heights are within four 500px viewports"
        );
        reader.viewport_height = 100.;
        reader.anchor = ResumeAnchor::new(reader.page_id(2), 2, 0.).unwrap();
        assert!(!reader.near_continuation(&segment));
        reader.anchor = ResumeAnchor::new(reader.page_id(3), 3, 0.).unwrap();
        assert!(
            reader.near_continuation(&segment),
            "last three source pages prefetch even when tall"
        );
        for mode in [
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            reader.mode = mode;
            reader.viewport_height = 10000.;
            reader.anchor.page_index = 0;
            assert!(
                !reader.near_continuation(&segment),
                "paged prefetch ignores stacked scroll extent"
            );
            reader.anchor.page_index = 3;
            assert!(
                reader.near_continuation(&segment),
                "paged modes preload at the last three source pages"
            );
        }

        let mut reader = self::reader();
        reader.anchor.page_index = 1;
        assert!(reader.prune_for_append().is_none());
        reader.anchor.page_index = 4;
        assert_eq!(reader.prune_for_append(), Some(2));
    }

    #[test]
    fn one_physical_metadata_worker_coalesces_repeated_opposite_demands() {
        let work = ContinuationWork::default();
        let (started, starts) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first = CancelToken::default();
        let (count, max, signal) = (active.clone(), maximum.clone(), started.clone());
        work.submit(
            first.clone(),
            Box::new(move || {
                let running = count.fetch_add(1, Ordering::AcqRel) + 1;
                max.fetch_max(running, Ordering::AcqRel);
                signal.send(0).unwrap();
                // Deliberately ignore cancellation, as an uncooperative provider
                // might do until its blocking network operation returns.
                held.recv_timeout(Duration::from_secs(5)).unwrap();
                count.fetch_sub(1, Ordering::AcqRel);
            }),
        );
        assert_eq!(starts.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
        let mut tokens = Vec::new();
        for direction in 1..=20 {
            let token = CancelToken::default();
            tokens.push(token.clone());
            let (count, max, signal) = (active.clone(), maximum.clone(), started.clone());
            work.submit(
                token,
                Box::new(move || {
                    let running = count.fetch_add(1, Ordering::AcqRel) + 1;
                    max.fetch_max(running, Ordering::AcqRel);
                    signal.send(direction).unwrap();
                    count.fetch_sub(1, Ordering::AcqRel);
                }),
            );
        }
        assert!(first.is_cancelled());
        assert!(tokens[..19].iter().all(CancelToken::is_cancelled));
        assert!(!tokens[19].is_cancelled());
        assert!(
            starts.try_recv().is_err(),
            "held canceled job still owns the only physical slot"
        );
        release.send(()).unwrap();
        assert_eq!(starts.recv_timeout(Duration::from_secs(5)).unwrap(), 20);
        assert!(starts.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(maximum.load(Ordering::Acquire), 1);
    }

    #[test]
    fn metadata_failure_advances_other_edge_and_reverse_demand_supersedes_forward() {
        const CHILD: &str = "READER_METADATA_DIRECTION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_continuity::tests::metadata_failure_advances_other_edge_and_reverse_demand_supersedes_forward", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let directory = tempfile::tempdir().unwrap();
        let services = Services::open(&directory.path().join("data")).unwrap();
        let ui = AppWindow::new().unwrap();
        ui.set_section(6);
        ui.window().set_size(slint::LogicalSize::new(400., 600.));
        let mut reader = super::super::tests::tall_reader();
        reader.initialise_segments("Current".into());
        reader.preload_epoch = 7;
        let current = reader.chapter.clone();
        let previous = ChapterId::new(current.series.clone(), "previous");
        let next = ChapterId::new(current.series.clone(), "unavailable-next");
        let chapters = [&previous, &current, &next]
            .into_iter()
            .enumerate()
            .map(|(i, id)| Chapter {
                id: id.clone(),
                title: i.to_string(),
                number: Some(i.to_string()),
                source_order: i as i64,
                label: String::new(),
                volume: None,
                uploaded_at: None,
                language: None,
                scanlator: None,
            })
            .collect::<Vec<_>>();
        assert_eq!(reader.continuation_candidates(&chapters)[0].1, next);
        reader.continuity_backwards = true;
        assert_eq!(reader.continuation_candidates(&chapters)[0].1, previous);
        reader.continuity_backwards = false;
        let folder = directory.path().join("originals");
        std::fs::create_dir(&folder).unwrap();
        image::RgbaImage::from_pixel(10, 20, image::Rgba([35, 81, 210, 255]))
            .save(folder.join("one.png"))
            .unwrap();
        services
            .downloads
            .import_folder(previous.clone(), &folder)
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
                directory.path().join("posters"),
            ),
            services,
            chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
                directory.path().join("cache"),
            )),
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::new(View {
                section: 6,
                reader: Some(reader),
                chapters,
                ..Default::default()
            }),
            generation: AtomicU64::new(7),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        app.mount_pages(&ui);
        app.maybe_preload_next(&ui);
        assert_eq!(
            app.view().reader.as_ref().unwrap().preloading.as_ref(),
            Some(&next)
        );
        let finished = std::rc::Rc::new(Cell::new(false));
        let complete = finished.clone();
        let owner = app.clone();
        let wanted_previous = previous.clone();
        let wanted_next = next.clone();
        let timer = slint::Timer::default();
        let start = std::time::Instant::now();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(5),
            move || {
                let view = owner.view();
                let r = view.reader.as_ref().unwrap();
                if r.segments.iter().any(|s| s.chapter == wanted_previous)
                    && r.preloading.is_none()
                    && r.continuity_failures.contains_key(&wanted_next)
                {
                    complete.set(true);
                    slint::quit_event_loop().unwrap();
                } else if start.elapsed() > Duration::from_secs(5) {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        slint::run_event_loop().unwrap();
        timer.stop();
        assert!(
            finished.get(),
            "failure must immediately advance the other edge without input"
        );
        assert_eq!(app.view().reader.as_ref().unwrap().chapter, current);
        for mode in [
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            let mut reader = super::super::tests::tall_reader();
            reader.mode = mode;
            reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
            reader.initialise_segments("Current".into());
            reader.preload_epoch = app.generation.load(Ordering::Acquire);
            reader.preload_nonce = 100;
            reader.preloading = Some(next.clone());
            app.view().reader = Some(reader);
            ui.set_reading_mode(mode_index(mode));
            ui.set_current_page(1);
            app.mount_pages(&ui);
            let (started, began) = mpsc::channel();
            let (release, held) = mpsc::channel();
            let cancel = CancelToken::default();
            app.continuation_work.submit(
                cancel.clone(),
                Box::new(move || {
                    started.send(()).unwrap();
                    held.recv_timeout(Duration::from_secs(5)).unwrap();
                }),
            );
            began.recv_timeout(Duration::from_secs(5)).unwrap();
            app.step_page(&ui, -1);
            {
                let view = app.view();
                let reader = view.reader.as_ref().unwrap();
                assert!(reader.continuity_backwards);
                assert_eq!(reader.preloading.as_ref(), Some(&previous));
                assert!(reader.preload_nonce > 100);
            }
            assert!(
                cancel.is_cancelled(),
                "paged backward input supersedes held Next preparation"
            );
            release.send(()).unwrap();
            let wanted = previous.clone();
            super::super::reader_handoff_tests::pump_reader_until(&app, move |r| {
                r.segments.iter().any(|s| s.chapter == wanted) && r.preloading.is_none()
            });
        }
        app.invalidate();
    }

    #[test]
    fn visible_short_tail_can_continue_without_becoming_the_top_anchor() {
        let mut reader = reader();
        reader.chapter_boundaries = false;
        reader.ratios = vec![0.1; 6];
        reader.anchor = ResumeAnchor::new(reader.page_id(0), 0, 0.).unwrap();
        reader.viewport_height = 600.;
        let chapters = (0..5)
            .map(|i| Chapter {
                id: ChapterId::new(reader.chapter.series.clone(), format!("chapter-{i}")),
                title: format!("Chapter {i}"),
                number: Some(i.to_string()),
                source_order: i,
                label: String::new(),
                volume: None,
                uploaded_at: None,
                language: None,
                scanlator: None,
            })
            .collect::<Vec<_>>();
        assert!(reader.visible_end() >= reader.segments[2].start);
        let candidates = reader.continuation_candidates(&chapters);
        assert_eq!(candidates[0].1, chapters[3].id);
        assert!(!candidates[0].3);
        assert_eq!(reader.prune_for_append(), Some(0));
        // Independent directional failure records survive successful attachment.
        let previous = ChapterId::new(reader.chapter.series.clone(), "previous");
        reader
            .continuity_failures
            .insert(previous.clone(), "previous failed".into());
        let rows = VecModel::from((0..6).map(|_| PageRow::default()).collect::<Vec<_>>());
        let prepared = next(&reader);
        append_rows(&mut reader, &rows, prepared, 0);
        assert_eq!(reader.segments.len(), 4);
        assert!(reader.continuity_failures.contains_key(&previous));
        assert_eq!(reader.anchor.page_index, 0);
    }

    #[test]
    fn tiny_chapters_walk_beyond_three_both_directions_without_pruning_visible_ids() {
        let mut reader = reader();
        reader.pages.truncate(1);
        reader.ratios = vec![0.125];
        reader.segments.truncate(1);
        reader.segments[0].start = 0;
        reader.segments[0].end = 1;
        reader.chapter_boundaries = false;
        reader.anchor = ResumeAnchor::new(reader.page_id(0), 0, 0.).unwrap();
        let rows = VecModel::from(vec![PageRow::default()]);
        let chapter = |reader: &Reader, number: i32| PreparedChapter {
            chapter: ChapterId::new(reader.chapter.series.clone(), format!("walk-{number}")),
            title: number.to_string(),
            source: None,
            pages: vec![PageInput::Local {
                path: PathBuf::new(),
                id: "same-source-page-id".into(),
            }],
            ratios: vec![0.125],
            completed: false,
        };
        let visible = |r: &Reader| {
            (r.anchor.page_index..=r.visible_end())
                .map(|i| r.row_page_id(i))
                .collect::<BTreeSet<_>>()
        };
        for number in 1..=24 {
            while reader.prune_for_append().is_none() {
                let index = (reader.anchor.page_index + 2).min(reader.pages.len() - 1);
                reader.anchor = ResumeAnchor::new(reader.page_id(index), index, 0.).unwrap();
            }
            let before = visible(&reader);
            let remove = reader.prune_for_append().unwrap();
            let prepared = chapter(&reader, number);
            append_rows(&mut reader, &rows, prepared, remove);
            let after = (0..reader.pages.len())
                .map(|i| reader.row_page_id(i))
                .collect::<BTreeSet<_>>();
            assert!(before.is_subset(&after));
            assert!(reader.segments.len() <= visible(&reader).len() + 2);
        }
        assert!(
            reader.segments.len() > MAX_SEGMENTS,
            "one viewport really contains more than three tiny chapters"
        );
        for number in (-24..0).rev() {
            reader.anchor = ResumeAnchor::new(reader.page_id(0), 0, 0.).unwrap();
            let before = visible(&reader);
            let keep = reader.prune_for_prepend().unwrap();
            let prepared = chapter(&reader, number);
            prepend_rows(&mut reader, &rows, prepared, keep);
            let after = (0..reader.pages.len())
                .map(|i| reader.row_page_id(i))
                .collect::<BTreeSet<_>>();
            assert!(before.is_subset(&after));
            assert!(reader.segments.len() <= visible(&reader).len() + 3);
        }
    }
}

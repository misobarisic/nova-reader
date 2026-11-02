//! Chapter metadata prefetch and bounded, in-place reader page-model growth.
//! Encoded pixels still flow through the existing image worker and its budgets.
use super::*;

const MAX_SEGMENTS: usize = 3;

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
        if self.anchor.page_index.saturating_add(2) >= segment.end {
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
            && remaining * self.viewport_width <= self.viewport_height.max(0.) * 2.
    }
    fn prune_for_append(&self) -> Option<usize> {
        if self.segments.len() < MAX_SEGMENTS {
            return Some(0);
        }
        let remove = self.segments[self.segments.len() - (MAX_SEGMENTS - 1) - 1].end;
        // Never remove any page intersecting the current viewport. A user who
        // scrolled back during metadata loading can keep reading existing art.
        (remove <= self.anchor.page_index).then_some(remove)
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
    let (source, pages, ratios) = if let Ok(offline) = services.downloads.offline(&chapter) {
        let job = services.downloads.job(&chapter).map_err(error)?;
        if offline.pages.len() != job.pages.len() || offline.pages.is_empty() {
            return Err(text::tr("This chapter has no pages"));
        }
        let pages = offline
            .pages
            .into_iter()
            .zip(job.pages)
            .map(|(path, page)| PageInput::Local {
                path,
                id: page.resource.id,
            })
            .collect::<Vec<_>>();
        // Same original probing as ordinary chapter opening, using the bounded
        // page reader. No decoded originals or prepared display pixels here.
        let ratios = pages
            .iter()
            .map(|page| {
                cancel.check().map_err(error)?;
                Ok(page
                    .read(None, cancel)
                    .ok()
                    .and_then(|bytes| {
                        reader_engine::inspect_image(&bytes, reader_engine::current_limits()).ok()
                    })
                    .map_or(1.5, |dimensions| {
                        dimensions.height as f32 / dimensions.width as f32
                    }))
            })
            .collect::<Result<Vec<_>>>()?;
        (None, pages, ratios)
    } else {
        let (source, pages) = services.pages(&chapter, cancel)?;
        let ratios = pages
            .iter()
            .map(|page| {
                page.width
                    .zip(page.height)
                    .filter(|(width, height)| *width > 0 && *height > 0)
                    .map_or(1.5, |(width, height)| height as f32 / width as f32)
            })
            .collect();
        (
            Some(source),
            pages.into_iter().map(PageInput::Remote).collect(),
            ratios,
        )
    };
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
    reader.continuity_error = None;
    reader.desired_anchor = usize::MAX;
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

impl Controller {
    /// Prefetch chapter metadata near the active tail in every reading mode.
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
            if reader.mode_switching || reader.pages.is_empty() {
                return;
            }
            let Some(tail) = reader.segments.last() else {
                return;
            };
            // The common scroll path stops before scanning the chapter list or
            // cloning any IDs when a following segment is already attached.
            if !(tail.start..tail.end).contains(&reader.anchor.page_index)
                || !reader.near_continuation(tail)
            {
                if !ui.get_reader_continuation_error().is_empty() {
                    ui.set_reader_continuation_error("".into());
                }
                return;
            }
            if reader.preloading.is_some() {
                return;
            }
            if let Some((_, message)) = &reader.continuity_error {
                if ui.get_reader_continuation_error().as_str() != message {
                    ui.set_reader_continuation_error(message.clone().into());
                }
                return;
            }
            if reader.prune_for_append().is_none() {
                return;
            }
            let previous = tail.chapter.clone();
            let next = view
                .chapters
                .iter()
                .position(|chapter| chapter.id == previous)
                .and_then(|index| view.chapters.get(index + 1))
                .map(|chapter| (chapter.id.clone(), chapter.title.clone()));
            let Some((chapter, title)) = next else {
                return;
            };
            let reader = view.reader.as_mut().unwrap();
            if reader
                .segments
                .iter()
                .any(|segment| segment.chapter == chapter)
            {
                return;
            }
            reader.preloading = Some(chapter.clone());
            (previous, chapter, title, generation)
        };
        let (previous, chapter, title, generation) = request;
        let cancel = self
            .cancel
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let services = self.services.clone();
        let app = Arc::downgrade(self);
        let weak = ui.as_weak();
        std::thread::spawn(move || {
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
                    || reader
                        .segments
                        .last()
                        .is_none_or(|segment| segment.chapter != previous)
                {
                    return;
                }
                reader.preloading = None;
                let prepared = match result {
                    Ok(prepared)
                        if prepared
                            .source
                            .as_ref()
                            .is_none_or(|source| source.availability().is_available()) =>
                    {
                        prepared
                    }
                    Ok(_) => {
                        let message = text::tr("Source unavailable");
                        reader.continuity_error = Some((chapter, message.clone()));
                        if reader
                            .segments
                            .last()
                            .is_some_and(|tail| reader.near_continuation(tail))
                        {
                            ui.set_reader_continuation_error(message.into());
                        }
                        return;
                    }
                    Err(message) => {
                        let message =
                            format!("{}: {message}", text::tr("Could not load next chapter"));
                        reader.continuity_error = Some((chapter, message.clone()));
                        if reader
                            .segments
                            .last()
                            .is_some_and(|tail| reader.near_continuation(tail))
                        {
                            ui.set_reader_continuation_error(message.into());
                        }
                        return;
                    }
                };
                let Some(remove) = reader.prune_for_append() else {
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
                if remove > 0 {
                    // Old image tasks carry absolute list indices. Change their
                    // generation before rebasing, then retain surviving pixels.
                    app.invalidate();
                    reader.preload_epoch = app.generation.load(Ordering::Acquire);
                    app.active_image
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                }
                let shift = append_rows(reader, rows, prepared, remove);
                ui.set_total_ratio(reader.total_extent());
                ui.set_current_page(reader.anchor.page_index as i32);
                if shift != 0. {
                    ui.set_scroll_y(ui.get_scroll_y() + shift * ui.get_reader_width());
                }
                reader.mode_switching = old_mode_switching;
                let index = reader.anchor.page_index;
                drop(view);
                ui.set_reader_continuation_error("".into());
                app.publish_reader_controls(&ui);
                app.request_images(&ui, index);
            });
        });
    }
    pub(super) fn retry_continuation(self: &Arc<Self>, ui: &AppWindow) {
        let mut view = self.view();
        if let Some(reader) = view.reader.as_mut() {
            reader.continuity_error = None;
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
    use std::{cell::Cell, pin::Pin};

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
        assert_eq!(previous.anchor.page_id, "canonical-1");
        assert_eq!(previous.anchor.page_index, 1);
        assert_eq!(previous.anchor.offset(), 0.875);
        assert!(
            previous.completed,
            "crossing forward finishes the chapter left behind"
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
        assert_eq!(back.anchor.page_index, 1);
        assert_eq!(back.anchor.page_id, "canonical-1");
        assert!(
            back.completed,
            "already completed retained chapters stay completed when revisited"
        );
        assert!(
            reader.activate_page(0).is_none(),
            "same-chapter movement does not manufacture a chapter transition"
        );
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
        let encoded = Arc::new(vec![7_u8; 16]);
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
        reader.decoded_responses = BTreeMap::from([(4, Arc::downgrade(&encoded))]);
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
        assert!(Arc::ptr_eq(
            &reader.decoded_responses[&2].upgrade().unwrap(),
            &encoded
        ));
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
    #[test]
    fn prefetch_uses_last_two_source_pages_or_two_viewports_and_pruning_protects_visible_pages() {
        let mut reader = reader();
        reader.segments.clear();
        reader.initialise_segments("One chapter".into());
        let segment = reader.segments[0].clone();
        reader.anchor = ResumeAnchor::new(reader.page_id(0), 0, 0.).unwrap();
        assert!(!reader.near_continuation(&segment));
        reader.anchor = ResumeAnchor::new(reader.page_id(3), 3, 0.).unwrap();
        assert!(
            reader.near_continuation(&segment),
            "three page heights are within two 600px viewports"
        );
        reader.viewport_height = 100.;
        assert!(!reader.near_continuation(&segment));
        reader.anchor = ResumeAnchor::new(reader.page_id(4), 4, 0.).unwrap();
        assert!(
            reader.near_continuation(&segment),
            "last two source pages prefetch even when tall"
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
            reader.anchor.page_index = 4;
            assert!(
                reader.near_continuation(&segment),
                "paged modes preload at the last two source pages"
            );
        }

        let mut reader = self::reader();
        reader.anchor.page_index = 1;
        assert!(reader.prune_for_append().is_none());
        reader.anchor.page_index = 2;
        assert_eq!(reader.prune_for_append(), Some(2));
    }
}

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
    fn near_previous(&self, segment: &ChapterSegment) -> bool {
        if self.anchor.page_index < segment.start.saturating_add(2) {
            return true;
        }
        let distance = self.page_position(self.anchor.page_index)
            + self.ratios[self.anchor.page_index] * self.anchor.offset() as f32
            - self.page_position(segment.start);
        self.mode == ReadingMode::Webtoon
            && self.viewport_width > 0.
            && distance * self.viewport_width <= self.viewport_height.max(0.) * 2.
    }
    /// Return the retained prefix length, never dropping visible artwork.
    fn prune_for_prepend(&self) -> Option<usize> {
        if self.segments.len() < MAX_SEGMENTS {
            return Some(self.pages.len());
        }
        let keep = self.segments[MAX_SEGMENTS - 1].start;
        (keep > self.visible_end()).then_some(keep)
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

/// Prepending shifts indices, but retains existing tile handles and logical
/// source offsets. Drop only an offscreen suffix to keep the three-chapter cap.
fn prepend_rows(
    reader: &mut Reader,
    rows: &VecModel<PageRow>,
    prepared: PreparedChapter,
    keep: usize,
) -> f32 {
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
        presentations
    );
    reader.continuity_error = None;
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
            if reader.preloading.is_some() {
                return;
            }
            let tail = reader
                .segments
                .last()
                .filter(|tail| {
                    (tail.start..tail.end).contains(&reader.anchor.page_index)
                        && reader.near_continuation(tail)
                        && reader.prune_for_append().is_some()
                })
                .map(|edge| edge.chapter.clone());
            let head = reader
                .segments
                .first()
                .filter(|head| {
                    (head.start..head.end).contains(&reader.anchor.page_index)
                        && reader.near_previous(head)
                        && reader.prune_for_prepend().is_some()
                })
                .map(|edge| edge.chapter.clone());
            let continuity_error = reader.continuity_error.clone();
            // source_order is canonical reading order; display sorting never
            // reverses this list. Prepare both neighbors when opening chapter 2.
            let mut failure = None;
            let candidate =
                [(tail, false), (head, true)]
                    .into_iter()
                    .find_map(|(edge, backwards)| {
                        let edge = edge?;
                        let index = view
                            .chapters
                            .iter()
                            .position(|chapter| chapter.id == edge)?;
                        let adjacent = if backwards {
                            index.checked_sub(1)?
                        } else {
                            index + 1
                        };
                        let chapter = view.chapters.get(adjacent)?;
                        if let Some((failed, message)) = &continuity_error
                            && failed == &chapter.id
                        {
                            failure = Some(message.clone());
                            return None;
                        }
                        Some((edge, chapter.id.clone(), chapter.title.clone(), backwards))
                    });
            let Some((previous, chapter, title, backwards)) = candidate else {
                ui.set_reader_continuation_error(failure.unwrap_or_default().into());
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
            (previous, chapter, title, generation, backwards)
        };
        let (previous, chapter, title, generation, backwards) = request;
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
                        if (if backwards {
                            reader.segments.first()
                        } else {
                            reader.segments.last()
                        })
                        .is_some_and(|edge| {
                            if backwards {
                                reader.near_previous(edge)
                            } else {
                                reader.near_continuation(edge)
                            }
                        }) {
                            ui.set_reader_continuation_error(message.into());
                        }
                        return;
                    }
                    Err(message) => {
                        let message = format!(
                            "{}: {message}",
                            text::tr(if backwards {
                                "Could not load previous chapter"
                            } else {
                                "Could not load next chapter"
                            })
                        );
                        reader.continuity_error = Some((chapter, message.clone()));
                        if (if backwards {
                            reader.segments.first()
                        } else {
                            reader.segments.last()
                        })
                        .is_some_and(|edge| {
                            if backwards {
                                reader.near_previous(edge)
                            } else {
                                reader.near_continuation(edge)
                            }
                        }) {
                            ui.set_reader_continuation_error(message.into());
                        }
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
                if shift != 0. {
                    ui.set_scroll_y(ui.get_scroll_y() + shift * ui.get_reader_width());
                }
                reader.mode_switching = old_mode_switching;
                let index = reader.anchor.page_index;
                drop(view);
                ui.set_reader_continuation_error("".into());
                app.publish_reader_controls(&ui);
                app.request_images(&ui, index);
                // The opposite neighbor may also be needed on initial open;
                // do not require a second scroll just to start its preparation.
                app.maybe_preload_next(&ui);
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
            assert_eq!(progress.anchor.page_index, 0);
            assert_eq!(progress.anchor.offset(), 0.);
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
                assert_eq!(shared.anchor.page_index, 0);
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
        assert!(reader.prune_for_prepend().is_none());
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

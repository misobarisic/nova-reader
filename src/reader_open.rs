//! Immediate reader route; preparation, persistence barriers and metadata stay on workers.
use super::*;

pub(super) struct Prepared {
    reader: Reader,
    series: Option<Series>,
    chapters: Vec<Chapter>,
}

fn prepare(app: &Arc<Controller>, chapter: ChapterId, cancel: &CancelToken) -> Result<Prepared> {
    let inventory = crate::page_inventory::load(&app.services, &chapter, cancel).map_err(error)?;
    let collection = crate::content_presentation::Settings::load(&chapter.series)
        .map(|s| s.effective() == crate::content_presentation::Presentation::Gallery)
        .unwrap_or(false);
    prepare_inventory(app, chapter, cancel, inventory.pages, None, collection)
}

fn prepare_inventory(
    app: &Arc<Controller>,
    chapter: ChapterId,
    cancel: &CancelToken,
    resources: Arc<Vec<crate::page_inventory::Resource>>,
    selected: Option<String>,
    collection: bool,
) -> Result<Prepared> {
    use crate::page_inventory::ResourceKind;
    cancel.check().map_err(error)?;
    let state = app.services.domain.snapshot().map_err(error)?;
    let progress = state
        .progress
        .iter()
        .find(|p| p.chapter == chapter)
        .cloned();
    let completed = progress.as_ref().is_some_and(|p| p.completed);
    if let Err(error) = crate::reader_settings::load_series(&chapter.series) {
        eprintln!("Nova Reader: reader settings unavailable: {error}");
    }
    let mode_choice = reader_modes::load(
        &chapter.series,
        reader_modes::legacy_mode(&state, &chapter.series),
    )
    .map_err(error)?;
    let source = resources.iter().find_map(|resource| match &resource.kind {
        ResourceKind::Remote { source, .. } => Some((**source).clone()),
        _ => None,
    });
    let source_name = source.as_ref().map(|s| s.descriptor().name.clone());
    let automatic_mode =
        reader_modes::automatic_for_state(&state, &chapter.series, source_name.as_deref());
    let mode = if collection && mode_choice == reader_modes::ModeChoice::Default {
        ReadingMode::LeftToRight
    } else {
        mode_choice.resolve(automatic_mode)
    };
    let mut anchor = progress.map(|p| p.anchor).unwrap_or_default();
    if let Some(id) = selected {
        let index = resources
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| text::tr("Image is no longer available"))?;
        anchor = ResumeAnchor::new(id, index, 0.).map_err(error)?;
    }
    let ratios = resources
        .iter()
        .map(|resource| match &resource.kind {
            ResourceKind::Remote { page, .. } => page
                .width
                .zip(page.height)
                .filter(|(w, h)| *w > 0 && *h > 0)
                .map(|(w, h)| h as f32 / w as f32)
                .unwrap_or(1.5),
            _ => 1.5,
        })
        .collect();
    let pages = resources
        .iter()
        .map(|resource| match &resource.kind {
            ResourceKind::Remote { page, .. } => PageInput::Remote(page.clone()),
            _ => PageInput::Verified(Box::new(resource.clone())),
        })
        .collect();
    let mut reader = Reader {
        chapter,
        source,
        pages,
        ratios,
        requested: BTreeSet::new(),
        retained: BTreeSet::new(),
        desired: BTreeSet::new(),
        failed: BTreeSet::new(),
        budget_blocked: BTreeSet::new(),
        desired_anchor: usize::MAX,
        decoded_bytes: BTreeMap::new(),
        decoded_widths: BTreeMap::new(),
        decoded_windows: BTreeMap::new(),
        decoded_responses: BTreeMap::new(),
        native_dimensions: BTreeMap::new(),
        progressive_job: None,
        presentations: BTreeMap::new(),
        display_options: reader_engine::DisplayOptions::default(),
        gap_pixels: 0.,
        chapter_boundaries: true,
        preload_epoch: 0,
        segments: Vec::new(),
        preloading: None,
        continuity_error: None,
        bookmarked_chapters: BTreeSet::new(),
        target_width: 0,
        viewport_width: 0.,
        viewport_height: 0.,
        anchor,
        mode,
        mode_choice,
        automatic_mode,
        mode_switching: false,
        completed,
        collection,
    };
    let ids = (0..reader.pages.len())
        .map(|i| reader.page_id(i))
        .collect::<Vec<_>>();
    reader.anchor = reader_engine::resolve_anchor(&reader.anchor, &ids).unwrap_or_default();
    // A deliberate mode edit during preparation wins; Gallery's default is transient.
    reader.mode_choice = reader_modes::load(
        &reader.chapter.series,
        reader_modes::legacy_mode(&state, &reader.chapter.series),
    )
    .map_err(error)?;
    reader.mode = if collection && reader.mode_choice == reader_modes::ModeChoice::Default {
        ReadingMode::LeftToRight
    } else {
        reader.mode_choice.resolve(reader.automatic_mode)
    };
    let series = state
        .series
        .into_iter()
        .find(|s| s.id == reader.chapter.series);
    let mut chapters = state
        .chapters
        .into_iter()
        .filter(|c| c.id.series == reader.chapter.series)
        .collect::<Vec<_>>();
    chapters.sort_by_key(|c| c.source_order);
    reader.initialise_segments(
        chapters
            .iter()
            .find(|c| c.id == reader.chapter)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| text::tr("Chapter")),
    );
    reader.bookmarked_chapters = state.bookmarks.into_iter().map(|b| b.chapter).collect();
    Ok(Prepared {
        reader,
        series,
        chapters,
    })
}

impl Controller {
    pub(super) fn open_chapter(self: &Arc<Self>, ui: &AppWindow, chapter: ChapterId) {
        if self.open_gallery_chapter(ui, &chapter) {
            return;
        }
        self.clear_overview_images(ui);
        self.view().overview = None;
        self.begin_reader_open(ui, chapter, prepare);
    }
    pub(super) fn open_inventory_chapter(
        self: &Arc<Self>,
        ui: &AppWindow,
        chapter: ChapterId,
        resources: Arc<Vec<crate::page_inventory::Resource>>,
        selected_page: String,
        collection: bool,
    ) {
        self.begin_reader_open(ui, chapter, move |app, chapter, cancel| {
            prepare_inventory(
                app,
                chapter,
                cancel,
                resources,
                Some(selected_page),
                collection,
            )
        });
    }
    fn begin_reader_open(
        self: &Arc<Self>,
        ui: &AppWindow,
        chapter: ChapterId,
        prepare: impl FnOnce(&Arc<Self>, ChapterId, &CancelToken) -> Result<Prepared> + Send + 'static,
    ) {
        ui.invoke_cancel_reader_zoom();
        self.checkpoint_progress();
        let (generation, cancel) = self.invalidate();
        let origin = self.view().section;
        if origin != 5 && origin != 6 {
            ui.set_detail_origin(origin);
        }
        if origin != 6 {
            ui.set_reader_return_section(origin);
        }
        {
            let mut view = self.view();
            view.reader = None;
            view.reader_open = Some(chapter.clone());
            view.section = 6;
        }
        ui.set_reader_osd(false);
        ui.set_reader_continuation_error("".into());
        ui.set_reader_open_error("".into());
        ui.set_status("".into());
        ui.set_busy(true);
        ui.set_pages(ModelRc::default());
        ui.set_total_ratio(0.);
        ui.set_current_page(0);
        ui.set_scroll_x(0.);
        ui.set_scroll_y(0.);
        ui.set_zoom(1.);
        ui.set_reader_opening(true);
        ui.set_section(6);
        ui.invoke_begin_reader_entry();
        let app = self.clone();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            // Preserve ordered progress before reading resume state. Never wait
            // for disk or a source inside the chapter tap handler.
            let result = app
                .progress_checkpoint
                .flush(None)
                .map_err(error)
                .and_then(|_| {
                    cancel.check().map_err(error)?;
                    reader_engine::with_current_limits(|| prepare(&app, chapter, &cancel))
                });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if app.generation.load(Ordering::Acquire) != generation || app.view().section != 6 {
                    return;
                }
                ui.set_busy(false);
                match result {
                    Ok(prepared) => {
                        let index = prepared.reader.anchor.page_index;
                        let mode = prepared.reader.mode;
                        let choice = prepared.reader.mode_choice;
                        ui.set_series_title(
                            prepared
                                .series
                                .as_ref()
                                .map(|s| s.title.as_str())
                                .unwrap_or("Series")
                                .into(),
                        );
                        {
                            let mut view = app.view();
                            view.series = prepared.series;
                            view.chapters = prepared.chapters;
                            ui.set_reader_collection(prepared.reader.collection);
                            ui.set_reader_gallery_group_count(if prepared.reader.collection {
                                view.overview
                                    .as_ref()
                                    .and_then(|o| o.groups.as_ref())
                                    .map_or(1, |g| g.len()) as i32
                            } else {
                                0
                            });
                            view.reader = Some(prepared.reader);
                            view.reader_open = None;
                        }
                        ui.set_reader_opening(false);
                        app.mount_pages(&ui);
                        ui.set_reader_mode_choice(choice.selection());
                        ui.set_reading_mode(mode_index(mode));
                        ui.set_current_page(index as i32);
                        app.restore_position(&ui);
                        app.apply_reader_preferences(&ui);
                        app.reflow(&ui, ui.get_reader_width(), ui.get_reader_viewport_height());
                        app.step_page(&ui, 0);
                    }
                    Err(message) => {
                        ui.set_reader_open_error(text::tr(&message).into());
                    }
                }
            });
        });
    }
}

pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) {
    let app = app.clone();
    let weak = ui.as_weak();
    ui.on_reader_retry_open(move || {
        let chapter = app.view().reader_open.clone();
        if let Some(ui) = weak.upgrade()
            && let Some(chapter) = chapter
        {
            app.open_chapter(&ui, chapter);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::ComponentHandle;
    use std::{
        cell::Cell,
        rc::Rc,
        time::{Duration, Instant},
    };

    #[test]
    fn chapter_open_is_immediate_cancellable_and_reports_errors_inside_reader() {
        const CHILD: &str = "NOVA_INSTANT_READER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_open::tests::chapter_open_is_immediate_cancellable_and_reports_errors_inside_reader", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
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
        crate::reader_settings::update("keep_awake", 0).unwrap();
        let ui = AppWindow::new().unwrap();
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
            view: Mutex::new(View::default()),
            generation: AtomicU64::new(0),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            metadata_refresh: Arc::default(),
        });
        ui.on_navigate({
            let app = app.clone();
            let weak = ui.as_weak();
            move |section| app.navigate(&weak.upgrade().unwrap(), section)
        });
        wire(&app, &ui);
        let chapter = ChapterId::new(reader_domain::SeriesId::new("missing", "story"), "chapter");
        for origin in [5, 2] {
            app.view().section = origin;
            ui.set_section(origin);
            ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
                loaded: true,
                ratio: 1.5,
                ..Default::default()
            }])));
            let release = Arc::new(AtomicBool::new(false));
            let gate = release.clone();
            ui.set_zoom(4.);
            ui.set_scroll_x(-120.);
            ui.set_scroll_y(-200.);
            app.begin_reader_open(&ui, chapter.clone(), move |_, _, _| {
                while !gate.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err("obsolete response".into())
            });
            assert_eq!(
                ui.get_section(),
                6,
                "chapter taps publish Reader before preparation completes"
            );
            assert_eq!(
                ui.get_pages().row_count(),
                0,
                "old chapter pixels are removed immediately"
            );
            assert!(ui.get_busy());
            assert!(ui.get_reader_opening());
            assert_eq!(ui.get_zoom(), 1., "every opening starts at fit zoom");
            assert_eq!(ui.get_scroll_x(), 0.);
            assert_eq!(ui.get_scroll_y(), 0.);
            assert_eq!(app.view().section, 6);
            ui.invoke_go_back();
            assert_eq!(
                ui.get_section(),
                origin,
                "Back remains available from both entry points"
            );
            release.store(true, Ordering::Release);
            slint::Timer::single_shot(Duration::from_millis(100), || {
                slint::quit_event_loop().unwrap()
            });
            slint::run_event_loop().unwrap();
            assert_eq!(ui.get_section(), origin);
            assert_eq!(
                ui.get_reader_open_error(),
                "",
                "cancelled preparation cannot reopen or fail the old reader"
            );
        }
        app.view().section = 2;
        ui.set_section(2);
        app.open_chapter(&ui, chapter);
        assert_eq!(ui.get_section(), 6);
        let finished = Rc::new(Cell::new(false));
        let timer = slint::Timer::default();
        let started = Instant::now();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(5), {
            let weak = ui.as_weak();
            let finished = finished.clone();
            move || {
                let ui = weak.upgrade().unwrap();
                if !ui.get_busy() && !ui.get_reader_open_error().is_empty() {
                    finished.set(true);
                    slint::quit_event_loop().unwrap();
                } else if started.elapsed() > Duration::from_secs(5) {
                    slint::quit_event_loop().unwrap();
                }
            }
        });
        slint::run_event_loop().unwrap();
        timer.stop();
        assert!(finished.get());
        assert_eq!(ui.get_section(), 6);
        assert!(
            i_slint_backend_testing::ElementHandle::find_by_accessible_label(
                &ui,
                "Retry opening chapter"
            )
            .next()
            .is_some()
        );
        ui.invoke_reader_retry_open();
        assert!(ui.get_busy());
        assert_eq!(ui.get_reader_open_error(), "");
        ui.invoke_go_back();
        assert_eq!(ui.get_section(), 2);

        // Exercise actual offline preparation and installation, not a synthetic
        // completion: source absence must not prevent a downloaded chapter.
        let offline = ChapterId::new(reader_domain::SeriesId::new("missing", "offline"), "one");
        app.services
            .domain
            .update(|state| {
                state.series.push(Series {
                    id: offline.series.clone(),
                    title: "Offline fixture".into(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                    publication_status: Default::default(),
                });
                state.chapters.push(Chapter {
                    id: offline.clone(),
                    title: "Chapter one".into(),
                    label: String::new(),
                    number: Some("1".into()),
                    volume: None,
                    source_order: 0,
                    language: None,
                    scanlator: None,
                    uploaded_at: None,
                });
                Ok(())
            })
            .unwrap();
        let mut bytes = Vec::new();
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &[40, 80, 120, 255].repeat(32 * 48),
                32,
                48,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        app.services
            .downloads
            .enqueue(
                offline.clone(),
                vec![reader_downloads::PageResource {
                    id: "page".into(),
                    url: "https://example.test/page.png".into(),
                    headers: Default::default(),
                }],
            )
            .unwrap();
        app.services
            .downloads
            .run(&offline, |_| Ok(bytes.clone()))
            .unwrap();
        app.open_chapter(&ui, offline.clone());
        assert_eq!(ui.get_section(), 6);
        let ready = Rc::new(Cell::new(false));
        let began = Instant::now();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(5), {
            let weak = ui.as_weak();
            let ready = ready.clone();
            move || {
                let ui = weak.upgrade().unwrap();
                if !ui.get_busy() && !ui.get_reader_opening() && ui.get_pages().row_count() == 1 {
                    ready.set(true);
                    slint::quit_event_loop().unwrap();
                } else if began.elapsed() > Duration::from_secs(5) {
                    slint::quit_event_loop().unwrap();
                }
            }
        });
        slint::run_event_loop().unwrap();
        timer.stop();
        assert!(ready.get(), "offline preparation never mounted the reader");
        assert_eq!(app.view().reader.as_ref().unwrap().chapter, offline);
        assert_eq!(ui.get_series_title(), "Offline fixture");
        assert_eq!(ui.get_reader_open_error(), "");
        assert!(
            i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Loading page…")
                .next()
                .is_none()
        );
        // Retained adjacent chapters also reset zoom, while ordinary page moves
        // within the same chapter keep the user's current zoom.
        let next = ChapterId::new(offline.series.clone(), "two");
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.pages.push(reader.pages[0].clone());
            reader.ratios.push(reader.ratios[0]);
            reader.segments.push(reader_continuity::ChapterSegment {
                chapter: next.clone(),
                title: "Chapter two".into(),
                start: 1,
                end: 2,
                source: None,
                completed: false,
            });
            reader.mode = ReadingMode::LeftToRight;
        }
        app.mount_pages(&ui);
        ui.set_reading_mode(1);
        ui.set_zoom(3.);
        app.step_page(&ui, 1);
        assert_eq!(ui.get_zoom(), 1.);
        assert_eq!(ui.get_scroll_x(), 0.);
        assert_eq!(ui.get_scroll_y(), 0.);
        assert_eq!(app.view().reader.as_ref().unwrap().chapter, next);
        ui.set_zoom(2.);
        app.step_page(&ui, 0);
        assert_eq!(
            ui.get_zoom(),
            2.,
            "same-chapter page movement does not reset zoom"
        );
        app.open_chapter(&ui, offline);
        assert_eq!(
            ui.get_zoom(),
            1.,
            "reopening a chapter never restores its old zoom"
        );
    }
}

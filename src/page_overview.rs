//! In-reader page picker. Grid metadata and thumbnail leases never own progress.
use super::*;
use crate::page_inventory::{Resource, ResourceKind};
use crate::page_thumbnails::{Completion, PageThumbnails};

pub(super) struct ReaderSnapshot {
    anchor: ResumeAnchor,
    zoom: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}
pub(super) struct Overview {
    pub(super) collection: bool,
    pub(super) series: Series,
    pub(super) chapter: ChapterId,
    pub(super) groups: Option<Vec<Chapter>>,
    pub(super) origin: i32,
    pub(super) inventory: Arc<Vec<Resource>>,
    pub(super) anchor: (String, f32),
    pub(super) density: i32,
    pub(super) generation: u64,
    pub(super) completions: BTreeMap<usize, Completion>,
    snapshot: Option<ReaderSnapshot>,
    range: (usize, usize, usize),
    start: usize,
}
impl Overview {
    pub(super) fn collection(
        series: Series,
        chapter: ChapterId,
        inventory: Arc<Vec<Resource>>,
        groups: Option<Vec<Chapter>>,
        origin: i32,
        generation: u64,
    ) -> Self {
        let anchor = (
            inventory.first().map(|r| r.id.clone()).unwrap_or_default(),
            0.,
        );
        Self {
            collection: true,
            series,
            chapter,
            groups,
            origin,
            inventory,
            anchor,
            density: 1,
            generation,
            completions: BTreeMap::new(),
            snapshot: None,
            range: (0, 0, 1),
            start: 0,
        }
    }
}
impl Controller {
    pub(super) fn thumbnails(self: &Arc<Self>, ui: &AppWindow) -> Arc<PageThumbnails> {
        self.thumbnail_service
            .get_or_init(|| {
                let owner = Arc::downgrade(self);
                let weak = ui.as_weak();
                Arc::new(PageThumbnails::new(
                    self.chapter_cache.clone(),
                    Arc::new(move |completion| {
                        let owner = owner.clone();
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            if let Some(app) = owner.upgrade() {
                                app.publish_thumbnail(&ui, completion);
                            }
                        });
                    }),
                ))
            })
            .clone()
    }
    pub(super) fn clear_overview_images(&self, ui: &AppWindow) {
        // Clear Slint references before releasing the budget reservations.
        let model = ui.get_overview_tiles();
        for index in 0..model.row_count() {
            if let Some(mut tile) = model.row_data(index) {
                tile.thumbnail = slint::Image::default();
                tile.loading = true;
                tile.error = "".into();
                model.set_row_data(index, tile);
            }
        }
        if let Some(overview) = self.view().overview.as_mut() {
            overview.completions.clear();
        }
    }
    pub(super) fn open_page_overview(self: &Arc<Self>, ui: &AppWindow) {
        let (series, chapter, start, inventory, snapshot) = {
            let view = self.view();
            if view.section != 6 {
                return;
            }
            let Some(reader) = &view.reader else {
                return;
            };
            if reader.pages.is_empty() {
                return;
            }
            let series = view
                .series
                .clone()
                .filter(|s| s.id == reader.chapter.series)
                .unwrap_or_else(|| Series {
                    id: reader.chapter.series.clone(),
                    title: ui.get_series_title().to_string(),
                    publication_status: Default::default(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                });
            let segment = reader.chapter_segment(reader.anchor.page_index);
            let start = segment.map_or(0, |s| s.start);
            let end = segment.map_or(reader.pages.len(), |s| s.end);
            let chapter = reader.owner(reader.anchor.page_index).clone();
            let inventory = (start..end)
                .map(|index| Resource {
                    chapter: chapter.clone(),
                    id: reader.page_id(index),
                    kind: match &reader.pages[index] {
                        PageInput::Verified(resource) => resource.kind.clone(),
                        PageInput::Remote(page) => reader.source_for(index).map_or_else(
                            || ResourceKind::Unavailable {
                                message: text::tr("Source unavailable"),
                            },
                            |source| ResourceKind::Remote {
                                source: Arc::new(source),
                                page: page.clone(),
                            },
                        ),
                        // Verified local metadata is filled on the worker below.
                        PageInput::Local { .. } => ResourceKind::Unavailable {
                            message: text::tr("Full-resolution image is unavailable offline"),
                        },
                    },
                })
                .collect::<Vec<_>>();
            (
                series,
                chapter,
                start,
                inventory,
                ReaderSnapshot {
                    anchor: reader.anchor.clone(),
                    zoom: ui.get_zoom(),
                    x: ui.get_scroll_x(),
                    y: ui.get_scroll_y(),
                    width: reader.viewport_width,
                    height: reader.viewport_height,
                },
            )
        };
        let generation = self.invalidate().0;
        let current = snapshot.anchor.page_index - start;
        let (anchor, density) = self
            .view()
            .overview
            .as_ref()
            .filter(|o| !o.collection && o.chapter == chapter)
            .map(|o| (o.anchor.clone(), o.density))
            .unwrap_or_else(|| ((inventory[current].id.clone(), 0.), 1));
        let inventory = Arc::new(inventory);
        self.view().overview = Some(Overview {
            collection: false,
            series,
            chapter: chapter.clone(),
            groups: None,
            origin: 5,
            inventory: inventory.clone(),
            anchor,
            density,
            generation,
            completions: BTreeMap::new(),
            snapshot: Some(snapshot),
            range: (0, 0, 1),
            start,
        });
        self.view().section = 23;
        self.thumbnails(ui)
            .replace_inventory(generation, inventory.clone());
        self.publish_overview(ui, current);
        // Local bytes already passed reader admission. Resolve their durable
        // expected hashes off the event loop without constructing another reader.
        if inventory
            .iter()
            .any(|r| matches!(r.kind, ResourceKind::Unavailable { .. }))
        {
            let owner = Arc::downgrade(self);
            let services = self.services.clone();
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let stored = services.downloads.page_inventory(&chapter);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let Some(app) = owner.upgrade() else {
                        return;
                    };
                    let Ok(stored) = stored else {
                        return;
                    };
                    let mut resources = (*inventory).clone();
                    for resource in &mut resources {
                        if let Some(page) = stored.iter().find(|p| p.resource.id == resource.id)
                            && let Some(sha256) = &page.sha256
                        {
                            resource.kind = ResourceKind::Local {
                                path: page.path.clone(),
                                expected_bytes: page.bytes,
                                sha256: sha256.clone(),
                            };
                        }
                    }
                    let resources = Arc::new(resources);
                    let range = {
                        let mut view = app.view();
                        if view.section != 23 {
                            return;
                        }
                        let Some(o) = view
                            .overview
                            .as_mut()
                            .filter(|o| o.generation == generation)
                        else {
                            return;
                        };
                        o.inventory = resources.clone();
                        o.range
                    };
                    app.thumbnails(&ui).replace_inventory(generation, resources);
                    app.thumbnails(&ui)
                        .viewport(generation, range.0, range.1, range.2);
                });
            });
        }
    }
    pub(super) fn publish_overview(&self, ui: &AppWindow, current: usize) {
        let view = self.view();
        let Some(o) = &view.overview else {
            return;
        };
        ui.set_overview_tiles(ModelRc::new(VecModel::from(
            o.inventory
                .iter()
                .enumerate()
                .map(|(i, r)| crate::OverviewTile {
                    page_id: r.id.clone().into(),
                    page_number: i as i32 + 1,
                    loading: true,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_overview_title(o.series.title.clone().into());
        ui.set_overview_subtitle(
            if o.collection {
                String::new()
            } else {
                format!("{} {}", o.inventory.len(), text::tr("pages"))
            }
            .into(),
        );
        ui.set_overview_collection(o.collection);
        ui.set_overview_rtl(
            view.reader
                .as_ref()
                .is_some_and(|r| r.mode == ReadingMode::RightToLeft),
        );
        ui.set_overview_current_index(current as i32);
        ui.set_overview_density(o.density);
        ui.set_overview_origin(o.origin);
        ui.set_overview_groups(ModelRc::new(VecModel::from(
            o.groups
                .as_ref()
                .filter(|groups| groups.len() > 1)
                .map(|groups| {
                    groups
                        .iter()
                        .map(|chapter| chapter.title.clone().into())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )));
        ui.set_overview_group_index(
            o.groups
                .as_ref()
                .and_then(|groups| groups.iter().position(|chapter| chapter.id == o.chapter))
                .unwrap_or(0) as i32,
        );
        ui.set_overview_busy(false);
        ui.set_overview_status("".into());
        ui.set_overview_restore_index(
            o.inventory
                .iter()
                .position(|r| r.id == o.anchor.0)
                .unwrap_or(current) as i32,
        );
        ui.set_overview_restore_fraction(o.anchor.1);
        ui.set_overview_restore_serial(ui.get_overview_restore_serial().wrapping_add(1));
        ui.set_section(23);
        ui.set_overview_focus_request(ui.get_overview_focus_request().wrapping_add(1));
    }
    fn publish_thumbnail(&self, ui: &AppWindow, completion: Completion) {
        let mut view = self.view();
        if view.section != 23
            || completion.generation != self.generation.load(Ordering::Acquire)
            || !completion.is_current()
        {
            return;
        }
        let Some(o) = view
            .overview
            .as_mut()
            .filter(|o| o.generation == completion.generation)
        else {
            return;
        };
        let (first, last, cols) = o.range;
        if completion.index < first.saturating_sub(cols)
            || completion.index >= last.saturating_add(cols)
        {
            return;
        }
        let model = ui.get_overview_tiles();
        if let Some(mut tile) = model.row_data(completion.index) {
            tile.loading = false;
            tile.tall_preview = completion.top_preview;
            match &completion.result {
                Ok(thumb) => {
                    tile.thumbnail = slint::Image::from_rgba8(thumb.pixels.clone());
                    tile.error = "".into();
                }
                Err(message) => {
                    tile.thumbnail = slint::Image::default();
                    tile.error = message.clone().into();
                    ui.set_overview_status(message.clone().into());
                }
            }
            model.set_row_data(completion.index, tile);
            o.completions.insert(completion.index, completion);
        }
    }
    pub(super) fn overview_viewport(
        self: &Arc<Self>,
        ui: &AppWindow,
        first: usize,
        last: usize,
        columns: usize,
    ) {
        let generation = {
            let mut view = self.view();
            if view.section != 23 {
                return;
            }
            let Some(o) = view.overview.as_mut() else {
                return;
            };
            o.range = (first, last, columns.max(1));
            let model = ui.get_overview_tiles();
            o.completions.retain(|index, _| {
                let keep = *index >= first.saturating_sub(columns)
                    && *index < last.saturating_add(columns);
                if !keep && let Some(mut tile) = model.row_data(*index) {
                    tile.thumbnail = slint::Image::default();
                    tile.loading = true;
                    tile.error = "".into();
                    model.set_row_data(*index, tile);
                }
                keep
            });
            o.generation
        };
        self.thumbnails(ui)
            .viewport(generation, first, last, columns);
    }
    pub(super) fn overview_back(self: &Arc<Self>, ui: &AppWindow) {
        if self.view().overview.as_ref().is_some_and(|o| o.collection) {
            self.gallery_back(ui);
        } else {
            self.leave_manga_overview(ui, None);
        }
    }
    fn change_overview_density(&self, ui: &AppWindow, density: i32, focal: Option<(&str, f32)>) {
        let (index, fraction, density, reflow, generation) = {
            let mut view = self.view();
            if view.section != 23 {
                return;
            }
            let Some(overview) = view.overview.as_mut() else {
                return;
            };
            let (id, fraction) = focal.unwrap_or((&overview.anchor.0, overview.anchor.1));
            if !fraction.is_finite() {
                return;
            }
            let Some(index) = overview.inventory.iter().position(|page| page.id == id) else {
                return;
            };
            overview.anchor = (id.to_owned(), fraction);
            let density = density.clamp(0, 2);
            let reflow = overview.collection && overview.density != density;
            overview.density = density;
            (index, fraction, density, reflow, overview.generation)
        };
        if reflow {
            // Release UI leases first. The worker can then rebalance its loaded
            // squares for the new visible count, including with disk cache off.
            self.clear_overview_images(ui);
            if let Some(service) = self.thumbnail_service.get() {
                service.prepare_gallery_reflow(generation);
            }
        }
        // Publish the logical focal anchor before changing the column count.
        // Otherwise ListView's density reflow can restore an older top row.
        ui.set_overview_restore_index(index as i32);
        ui.set_overview_restore_fraction(fraction);
        ui.set_overview_density(density);
        ui.set_overview_restore_serial(ui.get_overview_restore_serial().wrapping_add(1));
    }
    fn remember_overview_anchor(&self, ui: &AppWindow, id: &str, fraction: f32) {
        let index = {
            let mut view = self.view();
            if view.section != 23 || !fraction.is_finite() {
                return;
            }
            let Some(overview) = view.overview.as_mut() else {
                return;
            };
            let Some(index) = overview.inventory.iter().position(|page| page.id == id) else {
                return;
            };
            overview.anchor = (id.to_owned(), fraction);
            index
        };
        // Keep the next resize's inputs current without initiating a restore
        // during ordinary scrolling. Rotation must not reuse the entry anchor.
        ui.set_overview_restore_index(index as i32);
        ui.set_overview_restore_fraction(fraction);
    }
    pub(super) fn overview_select(self: &Arc<Self>, ui: &AppWindow, id: &str) {
        if self.view().overview.as_ref().is_some_and(|o| o.collection) {
            self.gallery_select(ui, id);
            return;
        }
        if self
            .view()
            .overview
            .as_ref()
            .is_some_and(|o| o.inventory.iter().any(|r| r.id == id))
        {
            self.leave_manga_overview(ui, Some(id));
        }
    }
    fn leave_manga_overview(self: &Arc<Self>, ui: &AppWindow, selected: Option<&str>) {
        if self.view().section != 23 {
            return;
        }
        self.clear_overview_images(ui);
        let overview = self.view().overview.take();
        let Some(mut o) = overview else {
            return;
        };
        let Some(snapshot) = o.snapshot.take() else {
            return;
        };
        self.invalidate();
        let index = selected
            .and_then(|id| o.inventory.iter().position(|r| r.id == id))
            .map(|i| o.start + i);
        {
            let mut view = self.view();
            view.section = 6;
            let Some(reader) = view.reader.as_mut() else {
                return;
            };
            reader.anchor = snapshot.anchor;
            reader.viewport_width = snapshot.width;
            reader.viewport_height = snapshot.height;
            reader.requested = reader.retained.clone();
            if let Some(index) = index {
                reader.anchor =
                    ResumeAnchor::new(reader.page_id(index), index, 0.).unwrap_or_default();
            }
        }
        ui.set_overview_tiles(ModelRc::default());
        ui.set_section(6);
        ui.set_zoom(snapshot.zoom);
        ui.set_current_page(self.view().reader.as_ref().unwrap().anchor.page_index as i32);
        if index.is_some() {
            self.step_page(ui, 0);
        } else {
            ui.set_scroll_x(snapshot.x);
            ui.set_scroll_y(snapshot.y);
            self.publish_reader_controls(ui);
        }
        let index = self.view().reader.as_ref().unwrap().anchor.page_index;
        self.request_images(ui, index);
        self.view().overview = Some(o);
    }
    pub(super) fn overview_check_current(self: &Arc<Self>, ui: &AppWindow) {
        if self.view().section != 23 {
            self.clear_overview_images(ui);
            return;
        }
        let invalid = self.view().overview.as_ref().is_some_and(|o| {
            o.generation != self.generation.load(Ordering::Acquire)
                || o.completions.values().any(|c| !c.is_current())
        });
        if invalid {
            self.clear_overview_images(ui);
            let data = self.view().overview.as_mut().map(|o| {
                o.generation = self.generation.load(Ordering::Acquire);
                (o.generation, o.inventory.clone(), o.range, o.collection)
            });
            if let Some((generation, inventory, range, collection)) = data {
                if collection {
                    self.thumbnails(ui)
                        .replace_inventory_for_gallery(generation, inventory);
                } else {
                    self.thumbnails(ui).replace_inventory(generation, inventory);
                }
                self.overview_viewport(ui, range.0, range.1, range.2);
            }
        }
    }
}

pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) -> slint::Timer {
    macro_rules! callback { ($name:ident, |$owner:ident, $ui:ident $(, $arg:ident)*| $body:expr) => {{
        let owner = Arc::downgrade(app); let weak = ui.as_weak();
        ui.$name(move |$($arg),*| { if let (Some($owner), Some($ui)) = (owner.upgrade(), weak.upgrade()) { $body } });
    }} }
    callback!(on_overview_back, |app, ui| app.overview_back(&ui));
    callback!(on_overview_selected, |app, ui, id| app
        .overview_select(&ui, &id));
    callback!(
        on_overview_visible_range,
        |app, ui, first, last, columns| app.overview_viewport(
            &ui,
            first.max(0) as usize,
            last.max(0) as usize,
            columns.max(1) as usize
        )
    );
    callback!(on_overview_anchor, |app, ui, id, fraction| {
        app.remember_overview_anchor(&ui, &id, fraction);
    });
    callback!(on_overview_density_changed, |app, ui, density| {
        app.change_overview_density(&ui, density, None);
    });
    callback!(
        on_overview_zoom_requested,
        |app, ui, density, id, fraction| {
            app.change_overview_density(&ui, density, Some((&id, fraction)));
        }
    );
    callback!(on_overview_group_selected, |app, ui, index| app
        .gallery_group(&ui, index));
    callback!(on_overview_menu, |app, ui| app.gallery_menu(&ui));
    callback!(on_overview_retry, |app, ui| {
        let collection = app.view().overview.as_ref().is_some_and(|o| o.collection);
        if collection {
            app.gallery_retry(&ui);
        } else if let Some(o) = app.view().overview.as_ref() {
            app.thumbnails(&ui).retry(o.generation);
        }
        ui.set_overview_status("".into());
    });
    let timer = slint::Timer::default();
    let owner = Arc::downgrade(app);
    let weak = ui.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(250),
        move || {
            if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                app.overview_check_current(&ui);
            }
        },
    );
    timer
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{
        LogicalSize,
        platform::{Key, WindowEvent},
    };

    #[test]
    fn native_manga_picker_preserves_modes_position_and_history() {
        const CHILD: &str = "NOVA_OVERVIEW_CONTROLLER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::page_overview::tests::native_manga_picker_preserves_modes_position_and_history", "--nocapture"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        i_slint_backend_testing::init_no_event_loop();
        let directory = tempfile::tempdir().unwrap();
        nova_storage::init_at(directory.path());
        crate::reader_settings::init().unwrap();
        let ui = AppWindow::new().unwrap();
        let services = Services::open(&directory.path().join("reader")).unwrap();
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
            image_worker_live: AtomicBool::new(true),
            view: Mutex::new(View::default()),
            generation: AtomicU64::new(1),
            cancel: Mutex::new(CancelToken::default()),
            images,
            download_serial: Mutex::new(()),
            active_image: Mutex::new(None),
            metadata_refresh: Arc::default(),
        });
        ui.global::<crate::Anim>().set_enabled(false);
        reader_controls::wire(&app, &ui);
        ui.on_reader_reflow({
            let owner = Arc::downgrade(&app);
            let weak = ui.as_weak();
            move |width, height| {
                if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                    app.reflow(&ui, width, height);
                }
            }
        });
        let _timer = wire(&app, &ui);
        ui.window().set_size(LogicalSize::new(390., 844.));
        ui.show().unwrap();
        assert!(
            app.thumbnail_service.get().is_none(),
            "ordinary app construction starts no thumbnail worker"
        );
        for (mode_index, mode) in [
            ReadingMode::Webtoon,
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ]
        .into_iter()
        .enumerate()
        {
            let mut reader = super::super::tests::tall_reader();
            reader.mode = mode;
            reader.anchor = ResumeAnchor::new("page-0", 0, 0.375).unwrap();
            reader.initialise_segments("Current chapter".into());
            let chapter = reader.chapter.clone();
            // Retained continuous neighbours must never enter this picker.
            reader.pages.push(PageInput::Local {
                id: "next-page".into(),
                path: PathBuf::new(),
            });
            reader.ratios.push(1.);
            app.view().reader = Some(reader);
            app.view().series = Some(Series {
                id: chapter.series.clone(),
                title: "Fixture".into(),
                publication_status: Default::default(),
                description: String::new(),
                authors: vec![],
                genres: vec![],
                cover_url: None,
                canonical_url: None,
            });
            app.view().section = 6;
            ui.set_section(6);
            ui.set_reading_mode(mode_index as i32);
            ui.set_current_page(0);
            app.mount_pages(&ui);
            ui.set_zoom(2.);
            ui.set_scroll_x(-17.);
            if mode == ReadingMode::Webtoon {
                app.restore_position(&ui);
            } else {
                ui.set_scroll_y(-123.);
            }
            for _ in 0..8 {
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
            }
            let pan = (ui.get_scroll_x(), ui.get_scroll_y());
            let anchor = app.view().reader.as_ref().unwrap().anchor.clone();
            let state = app.services.domain.snapshot().unwrap();
            ui.invoke_reader_action("pages".into());
            assert_eq!(ui.get_section(), 23);
            assert_eq!(ui.get_overview_tiles().row_count(), 2);
            app.overview_viewport(&ui, 0, 2, 2);
            app.position(&ui, 99999., 390., 844.);
            app.reflow(&ui, 600., 800.);
            assert_eq!(app.view().reader.as_ref().unwrap().anchor, anchor);
            assert!(app.view().pending.is_none());
            assert_eq!(
                app.services.domain.snapshot().unwrap().history,
                state.history
            );
            assert_eq!(
                app.services.domain.snapshot().unwrap().progress,
                state.progress
            );
            // Native Escape reaches AppWindow's production overview-back route.
            for _ in 0..8 {
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
            }
            ui.window().dispatch_event(WindowEvent::KeyPressed {
                text: Key::Escape.into(),
            });
            for _ in 0..8 {
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
            }
            assert_eq!(ui.get_section(), 6);
            assert_eq!(app.view().reader.as_ref().unwrap().anchor, anchor);
            assert_eq!(app.view().reader.as_ref().unwrap().anchor.offset(), 0.375);
            assert_eq!(app.view().reader.as_ref().unwrap().mode, mode);
            assert_eq!(ui.get_reading_mode(), mode_index as i32);
            assert_eq!(ui.get_zoom(), 2.);
            assert_eq!(ui.get_scroll_x(), pan.0);
            assert_eq!(ui.get_scroll_y(), pan.1);
            ui.invoke_reader_action("pages".into());
            for _ in 0..8 {
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
            }
            i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Page 2")
                .find(|e| {
                    e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
                })
                .expect("current chapter's second tile is visible")
                .invoke_accessible_default_action();
            assert_eq!(ui.get_section(), 6);
            assert_eq!(ui.get_reading_mode(), mode_index as i32);
            assert_eq!(app.view().reader.as_ref().unwrap().mode, mode);
            assert_eq!(app.view().reader.as_ref().unwrap().anchor.page_id, "page-1");
            assert_eq!(app.view().reader.as_ref().unwrap().segments.len(), 1);
            app.view().pending = None;
        }
        app.reader_action(&ui, "back");
        assert_eq!(
            ui.get_section(),
            5,
            "picker selection adds no Back destination"
        );
    }
}

//! Gallery routing above the existing chapter and reader identities.
use super::*;
use crate::page_inventory::Inventory;

fn load(
    services: &Services,
    series: &Series,
    requested: Option<&ChapterId>,
    cancel: &CancelToken,
) -> Result<(Vec<Chapter>, Inventory)> {
    cancel.check().map_err(error)?;
    let state = services.domain.snapshot().map_err(error)?;
    let metadata = state
        .series
        .iter()
        .find(|saved| saved.id == series.id)
        .cloned();
    let mut chapters = state
        .chapters
        .into_iter()
        .filter(|chapter| chapter.id.series == series.id)
        .collect::<Vec<_>>();
    // Keep listing metadata usable by the header menu even if the source can
    // no longer return its groups. This writes no reading state.
    if metadata.is_none() {
        services.save_details(series.clone(), Vec::new())?;
    }
    if chapters.is_empty() {
        let source = services.source(&series.id.source_id)?;
        let request = json!({"sourceId":series.id.source_id,"id":series.id.local_id,"seriesId":series.id.local_id,"url":series.canonical_url});
        let list: reader_sources::ChapterList = serde_json::from_value(
            source
                .call_cancelled("chapters", &request, cancel)
                .map_err(error)?,
        )
        .map_err(error)?;
        chapters = list
            .chapters
            .into_iter()
            .map(|chapter| crate::services::chapter_from_source(&series.id, chapter))
            .collect::<Result<Vec<_>>>()?;
        cancel.check().map_err(error)?;
        services.save_details(metadata.unwrap_or_else(|| series.clone()), chapters.clone())?;
    }
    chapters.sort_by_key(|chapter| chapter.source_order);
    let chapter = if let Some(id) = requested {
        chapters
            .iter()
            .find(|chapter| &chapter.id == id)
            .ok_or_else(|| text::tr("Group is no longer available"))?
    } else {
        chapters
            .first()
            .ok_or_else(|| text::tr("No images are available"))?
    };
    let inventory = crate::page_inventory::load(services, &chapter.id, cancel)?;
    cancel.check().map_err(error)?;
    Ok((chapters, inventory))
}

struct Request {
    series: Series,
    chapter: Option<ChapterId>,
    resolve_metadata: bool,
    refresh: bool,
}
impl Controller {
    pub(super) fn open_gallery(self: &Arc<Self>, ui: &AppWindow, series: Series) {
        self.open_gallery_at(
            ui,
            Request {
                series,
                chapter: None,
                resolve_metadata: false,
                refresh: false,
            },
        );
    }
    pub(super) fn open_gallery_chapter(
        self: &Arc<Self>,
        ui: &AppWindow,
        chapter: &ChapterId,
    ) -> bool {
        if crate::content_presentation::Settings::load(&chapter.series)
            .unwrap_or_default()
            .effective()
            != crate::content_presentation::Presentation::Gallery
        {
            return false;
        }
        let series = self
            .view()
            .series
            .clone()
            .filter(|s| s.id == chapter.series)
            .unwrap_or_else(|| Series {
                id: chapter.series.clone(),
                title: text::tr("Collection"),
                publication_status: Default::default(),
                description: String::new(),
                authors: vec![],
                genres: vec![],
                cover_url: None,
                canonical_url: None,
            });
        self.open_gallery_at(
            ui,
            Request {
                series,
                chapter: Some(chapter.clone()),
                resolve_metadata: true,
                refresh: false,
            },
        );
        true
    }
    fn open_gallery_at(self: &Arc<Self>, ui: &AppWindow, request: Request) {
        let series = &request.series;
        let origin = {
            let view = self.view();
            if matches!(view.section, 6 | 23) {
                view.overview
                    .as_ref()
                    .filter(|o| o.collection)
                    .map_or(ui.get_detail_origin(), |o| o.origin)
            } else if view.section == 5 {
                ui.get_detail_origin()
            } else {
                view.section
            }
        };
        self.flush();
        self.clear_overview_images(ui);
        let (generation, cancel) = self.invalidate();
        {
            let mut view = self.view();
            view.reader = None;
            view.reader_open = None;
            view.pending = None;
            view.series = Some(series.clone());
            view.section = 23;
            view.overview = Some(page_overview::Overview::collection(
                series.clone(),
                ChapterId::new(series.id.clone(), ""),
                Arc::new(Vec::new()),
                None,
                origin,
                generation,
            ));
        }
        ui.set_pages(ModelRc::default());
        ui.set_total_ratio(0.);
        ui.set_reader_opening(false);
        ui.set_busy(false);
        ui.set_status("".into());
        self.publish_overview(ui, 0);
        ui.set_overview_busy(true);
        self.load_gallery(ui, request, generation, cancel);
    }
    fn load_gallery(
        self: &Arc<Self>,
        ui: &AppWindow,
        request: Request,
        generation: u64,
        cancel: CancelToken,
    ) {
        let owner = Arc::downgrade(self);
        let services = self.services.clone();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let mut series = request.series;
            if request.resolve_metadata
                && let Ok(state) = services.domain.snapshot()
                && let Some(saved) = state.series.into_iter().find(|saved| saved.id == series.id)
            {
                series = saved;
            }
            let result: Result<(Vec<Chapter>, Inventory)> = (|| {
                if request.refresh {
                    series = services.details(&series, &cancel)?.0;
                }
                let (groups, inventory) =
                    load(&services, &series, request.chapter.as_ref(), &cancel)?;
                Ok((groups, inventory))
            })();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let Some(app) = owner.upgrade() else {
                    return;
                };
                if app.generation.load(Ordering::Acquire) != generation || app.view().section != 23
                {
                    return;
                }
                ui.set_overview_busy(false);
                match result {
                    Ok((groups, inventory)) => {
                        app.install_gallery(&ui, series, groups, inventory, generation)
                    }
                    Err(message) => ui.set_overview_status(message.into()),
                }
            });
        });
    }
    fn install_gallery(
        self: &Arc<Self>,
        ui: &AppWindow,
        series: Series,
        groups: Vec<Chapter>,
        inventory: Inventory,
        generation: u64,
    ) {
        let (origin, anchor, density, previous) =
            {
                let view = self.view();
                let Some(o) = view.overview.as_ref().filter(|o| {
                    o.collection && o.generation == generation && o.series.id == series.id
                }) else {
                    return;
                };
                (o.origin, o.anchor.clone(), o.density, o.chapter.clone())
            };
        let mut overview = page_overview::Overview::collection(
            series,
            inventory.chapter.clone(),
            inventory.pages.clone(),
            Some(groups.clone()),
            origin,
            generation,
        );
        if previous == inventory.chapter && inventory.pages.iter().any(|r| r.id == anchor.0) {
            overview.anchor = anchor;
        }
        overview.density = density;
        let current = self
            .services
            .domain
            .snapshot()
            .ok()
            .and_then(|state| {
                state
                    .progress
                    .into_iter()
                    .find(|p| p.chapter == inventory.chapter)
            })
            .and_then(|progress| {
                inventory
                    .pages
                    .iter()
                    .position(|r| r.id == progress.anchor.page_id)
            })
            .unwrap_or(0);
        {
            let mut view = self.view();
            view.chapters = groups;
            view.overview = Some(overview);
        }
        self.thumbnails(ui)
            .replace_inventory_for_gallery(generation, inventory.pages);
        self.publish_overview(ui, current);
    }
    pub(super) fn gallery_group(self: &Arc<Self>, ui: &AppWindow, index: i32) {
        let selected = {
            let view = self.view();
            if view.section != 23 {
                return;
            }
            let Some(o) = view.overview.as_ref().filter(|o| o.collection) else {
                return;
            };
            let Some(chapter) = o.groups.as_ref().and_then(|g| g.get(index.max(0) as usize)) else {
                return;
            };
            if chapter.id == o.chapter && !o.inventory.is_empty() {
                return;
            }
            (o.series.clone(), chapter.id.clone())
        };
        self.reload_gallery(ui, selected.0, Some(selected.1), false);
    }
    pub(super) fn gallery_retry(self: &Arc<Self>, ui: &AppWindow) {
        let selected = self
            .view()
            .overview
            .as_ref()
            .filter(|o| o.collection)
            .map(|o| {
                (
                    o.series.clone(),
                    (!o.chapter.local_id.is_empty()).then(|| o.chapter.clone()),
                    o.inventory.is_empty(),
                )
            });
        if let Some((series, chapter, empty)) = selected {
            if empty || !ui.get_overview_status().is_empty() {
                self.reload_gallery(ui, series, chapter, false);
            } else if let Some(o) = self.view().overview.as_ref() {
                self.thumbnails(ui).retry(o.generation);
            }
        }
    }
    pub(super) fn gallery_refresh(self: &Arc<Self>, ui: &AppWindow) {
        let target = self
            .view()
            .overview
            .as_ref()
            .filter(|o| o.collection)
            .map(|o| {
                (
                    o.series.clone(),
                    (!o.chapter.local_id.is_empty()).then(|| o.chapter.clone()),
                )
            });
        if let Some((series, chapter)) = target {
            self.reload_gallery(ui, series, chapter, true);
        }
    }
    fn reload_gallery(
        self: &Arc<Self>,
        ui: &AppWindow,
        series: Series,
        chapter: Option<ChapterId>,
        refresh: bool,
    ) {
        self.clear_overview_images(ui);
        let (generation, cancel) = self.invalidate();
        let mut view = self.view();
        view.section = 23;
        if let Some(o) = view.overview.as_mut() {
            o.generation = generation;
            o.inventory = Arc::new(Vec::new());
            if let Some(chapter) = &chapter
                && o.chapter != *chapter
            {
                o.chapter = chapter.clone();
                o.anchor = (String::new(), 0.);
            }
        }
        drop(view);
        self.publish_overview(ui, 0);
        ui.set_overview_busy(true);
        ui.set_overview_status("".into());
        self.load_gallery(
            ui,
            Request {
                series,
                chapter,
                resolve_metadata: false,
                refresh,
            },
            generation,
            cancel,
        );
    }
    pub(super) fn gallery_select(self: &Arc<Self>, ui: &AppWindow, id: &str) {
        let target = {
            let view = self.view();
            if view.section != 23 {
                return;
            }
            view.overview
                .as_ref()
                .filter(|o| o.collection && o.inventory.iter().any(|r| r.id == id))
                .map(|o| (o.chapter.clone(), o.inventory.clone()))
        };
        if let Some((chapter, inventory)) = target {
            self.clear_overview_images(ui);
            ui.set_overview_tiles(ModelRc::default());
            if let Some(service) = self.thumbnail_service.get() {
                service.cancel();
            }
            self.open_inventory_chapter(ui, chapter, inventory, id.to_owned(), true);
        }
    }
    pub(super) fn gallery_return(self: &Arc<Self>, ui: &AppWindow) -> bool {
        let target = {
            let view = self.view();
            let Some(o) = view.overview.as_ref().filter(|o| o.collection) else {
                return false;
            };
            let current = view
                .reader
                .as_ref()
                .and_then(|reader| {
                    o.inventory
                        .iter()
                        .position(|r| r.id == reader.anchor.page_id)
                })
                .unwrap_or(ui.get_overview_current_index().max(0) as usize);
            (o.inventory.clone(), current)
        };
        self.checkpoint_progress();
        self.clear_overview_images(ui);
        let generation = self.invalidate().0;
        ui.set_pages(ModelRc::default());
        ui.set_total_ratio(0.);
        ui.set_reader_opening(false);
        ui.set_busy(false);
        {
            let mut view = self.view();
            view.reader = None;
            view.reader_open = None;
            view.section = 23;
            if let Some(o) = view.overview.as_mut() {
                o.generation = generation;
            }
        }
        if target.0.is_empty() {
            let request = self.view().overview.as_ref().map(|o| {
                (
                    o.series.clone(),
                    (!o.chapter.local_id.is_empty()).then(|| o.chapter.clone()),
                )
            });
            if let Some((series, chapter)) = request {
                self.reload_gallery(ui, series, chapter, false);
            }
            return true;
        }
        self.thumbnails(ui)
            .replace_inventory_for_gallery(generation, target.0);
        self.publish_overview(ui, target.1);
        true
    }
    pub(super) fn gallery_back(self: &Arc<Self>, ui: &AppWindow) {
        let origin = self
            .view()
            .overview
            .as_ref()
            .filter(|o| o.collection)
            .map(|o| o.origin);
        if let Some(origin) = origin {
            if origin == 3 && ui.get_source_catalog_open() {
                self.clear_overview_images(ui);
                ui.set_overview_tiles(ModelRc::default());
                self.view().overview = None;
                self.return_to_catalogue(ui);
                return;
            }
            self.navigate(ui, origin);
        }
    }
    pub(super) fn gallery_details(self: &Arc<Self>, ui: &AppWindow, series: Series) {
        self.clear_overview_images(ui);
        let (generation, cancel) = self.invalidate();
        ui.set_overview_tiles(ModelRc::default());
        ui.set_detail_origin(23);
        {
            let mut view = self.view();
            view.series = Some(series.clone());
            view.section = 5;
        }
        ui.set_status("".into());
        self.render(ui);
        self.refresh_download_states(ui);
        ui.set_busy(true);
        let services = self.services.clone();
        let owner = Arc::downgrade(self);
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = services.details(&series, &cancel);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let Some(app) = owner.upgrade() else {
                    return;
                };
                if app.generation.load(Ordering::Acquire) != generation {
                    return;
                }
                let mut view = app.view();
                if view.section != 5 || view.series.as_ref().is_none_or(|s| s.id != series.id) {
                    return;
                }
                ui.set_busy(false);
                match result {
                    Ok((series, mut groups)) => {
                        if let Some(o) = view
                            .overview
                            .as_mut()
                            .filter(|o| o.collection && o.series.id == series.id)
                        {
                            // Metadata refresh may withdraw a group. Keep the
                            // active inventory and its identities usable until
                            // the user explicitly chooses another group.
                            if !groups.iter().any(|group| group.id == o.chapter)
                                && let Some(current) = o
                                    .groups
                                    .as_ref()
                                    .and_then(|old| old.iter().find(|group| group.id == o.chapter))
                            {
                                groups.push(current.clone());
                            }
                            groups.sort_by_key(|group| group.source_order);
                            o.series = series.clone();
                            o.groups = Some(groups.clone());
                        }
                        view.series = Some(series);
                        view.chapters = groups;
                        drop(view);
                        ui.set_status("".into());
                        app.render(&ui);
                    }
                    Err(message) => {
                        drop(view);
                        ui.set_status(
                            format!("{}: {message}", text::tr("Showing saved metadata")).into(),
                        );
                    }
                }
            });
        });
    }
    pub(super) fn gallery_menu(&self, ui: &AppWindow) {
        let series = self.view().overview.as_ref().map(|o| o.series.clone());
        if let Some(series) = series {
            let in_library = self
                .services
                .domain
                .snapshot()
                .is_ok_and(|state| state.library.iter().any(|entry| entry.series == series.id));
            ui.invoke_open_entry_menu(
                serde_json::to_string(&series.id).unwrap_or_default().into(),
                series.title.into(),
                in_library,
                16.,
                64.,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_backend_testing::{AccessibleRole, ElementHandle};
    use slint::LogicalSize;
    use std::{
        cell::Cell,
        io::{Cursor, Write},
        rc::Rc,
        time::{Duration, Instant},
    };

    #[track_caller]
    fn pump(ui: &AppWindow, predicate: impl Fn(&AppWindow) -> bool + 'static) {
        let weak = ui.as_weak();
        let done = Rc::new(Cell::new(None));
        let passed = done.clone();
        let start = Instant::now();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(10),
            move || {
                if passed.get().is_some() {
                    return;
                }
                if let Some(ui) = weak.upgrade() {
                    if predicate(&ui) {
                        passed.set(Some(true));
                        slint::quit_event_loop().unwrap();
                    } else if start.elapsed() > Duration::from_secs(12) {
                        passed.set(Some(false));
                        slint::quit_event_loop().unwrap();
                    }
                }
            },
        );
        slint::run_event_loop().unwrap();
        assert!(
            done.get() == Some(true),
            "timed out: section={} tiles={} busy={} overview={} reader={} opening={}",
            ui.get_section(),
            ui.get_overview_tiles().row_count(),
            ui.get_overview_busy(),
            ui.get_overview_status(),
            ui.get_reader_open_error(),
            ui.get_reader_opening()
        );
    }
    #[track_caller]
    fn activate(ui: &AppWindow, label: &str) {
        // The headless backend instantiates virtualized item trees through its
        // explicit frame helper, even when worker callbacks use system time.
        for _ in 0..60 {
            i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
            if let Some(button) = ElementHandle::find_by_accessible_label(ui, label)
                .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
            {
                button.invoke_accessible_default_action();
                return;
            }
            slint::Timer::single_shot(Duration::from_millis(10), || {
                slint::quit_event_loop().unwrap();
            });
            slint::run_event_loop().unwrap();
        }
        use i_slint_backend_testing::ElementRoot;
        let mut labels = Vec::new();
        ui.root_element().visit_descendants(|e| {
            if let Some(label) = e.accessible_label() {
                labels.push((label.to_string(), e.accessible_role()));
            }
            std::ops::ControlFlow::<()>::Continue(())
        });
        panic!("missing native button {label}: {labels:?}");
    }
    fn package() -> (Vec<u8>, Vec<u8>) {
        let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"gallery-fixture","name":"Gallery fixture","version":"1.0.0","language":"en","nsfw":false,"entry":"main.js","scripts":[],"assets":["page.png"],"permissions":{"domains":[],"httpPost":false},"capabilities":["capabilities","browse","chapters","pages","details"],"configuration":[{"key":"explicitMetadata","name":"Explicit metadata test gate","kind":"bool","default":false}]});
        let script = r#"globalThis.novaProvider={handle(r){
            const op=r.op||r.operation;
            const entries=['single','multi','manga'].map(id=>({id,title:id,url:'',presentation:id==='manga'?undefined:'gallery'}));
            if(op==='capabilities')return {sources:[{id:'gallery-fixture',name:'Gallery fixture',language:'en',nsfw:false,operations:['browse','chapters','pages','details']}]};
            if(op==='browse')return {items:entries,hasNextPage:false};
            if(op==='details'){if(!r.settings.explicitMetadata)throw Error('Gallery must not request details before explicit action');return {...entries.find(e=>e.id===(r.seriesId||r.id)),description:'Full collection description',authors:['Fixture author'],genres:['Fixture genre']};}
            if(op==='chapters'){const id=r.seriesId||r.id;return {chapters:(id==='multi'?['b','a']:['a']).map(group=>({id:id+'-'+group,seriesId:id,title:'Group '+group,sourceOrder:group==='a'?0:1,url:''}))};}
            if(op==='pages')return {pages:Array.from({length:(r.chapterId||r.id).endsWith('-b')?1:3},(_,i)=>({id:'stable-'+i,url:'addon://gallery-fixture/page.png',headers:{},width:2,height:3}))};
            return {};
        }};"#;
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 3, image::Rgba([70, 120, 190, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let image = png.into_inner();
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("main.js", script.as_bytes().to_vec()),
            ("page.png", image.clone()),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&bytes).unwrap();
        }
        (archive.finish().unwrap().into_inner(), image)
    }
    #[test]
    fn native_gallery_entry_groups_viewer_back_and_offline_keep_identities() {
        const CHILD: &str = "NOVA_NATIVE_GALLERY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::gallery_presentation::tests::native_gallery_entry_groups_viewer_back_and_offline_keep_identities", "--nocapture"])
                .env(CHILD,"1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let directory = tempfile::tempdir().unwrap();
        nova_storage::init_at(directory.path());
        let services = Services::open(&directory.path().join("reader")).unwrap();
        crate::reader_settings::init().unwrap();
        crate::reader_settings::update("fullscreen", 0).unwrap();
        crate::reader_settings::update("keep_awake", 0).unwrap();
        let ui = AppWindow::new().unwrap();
        ui.global::<crate::Anim>().set_enabled(false);
        let (images, receiver) = mpsc::channel();
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
            chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
                directory.path().join("cache"),
            )),
            services,
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(true),
            view: Mutex::new(View::default()),
            generation: AtomicU64::new(1),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            metadata_refresh: Arc::default(),
        });
        let owner = Arc::downgrade(&app);
        let weak = ui.as_weak();
        std::thread::spawn(move || Controller::image_worker(owner, receiver, weak));
        reader_controls::wire(&app, &ui);
        reader_open::wire(&app, &ui);
        thumbnail_cache_settings::wire(&app, &ui);
        let _overview_timer = page_overview::wire(&app, &ui);
        ui.on_navigate({
            let owner = Arc::downgrade(&app);
            let weak = ui.as_weak();
            move |section| {
                if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                    app.navigate(&ui, section);
                }
            }
        });
        ui.on_entry_action({
            let owner = Arc::downgrade(&app);
            let weak = ui.as_weak();
            move |key, action| {
                if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                    app.entry_action(&ui, &key, &action);
                }
            }
        });
        ui.window().set_size(LogicalSize::new(390., 844.));
        ui.show().unwrap();
        let (package, _image) = package();
        app.services.install(&package).unwrap();
        let source = app.services.source("gallery-fixture").unwrap();
        let mut entries = source
            .browse(&json!({"page":1}))
            .unwrap()
            .items
            .into_iter()
            .map(|dto| app.services.series("gallery-fixture", dto))
            .collect::<Vec<_>>();
        let single = entries.remove(0);
        let multi = entries.remove(0);
        let manga = entries.remove(0);
        assert!(
            app.thumbnail_service.get().is_none(),
            "browse never starts thumbnail work"
        );
        let before_cache = app.services.domain.snapshot().unwrap();
        ui.global::<crate::ImageStorageSettings>()
            .invoke_thumbnail_cache_refresh();
        assert!(
            ui.global::<crate::ImageStorageSettings>()
                .get_thumbnail_cache_busy()
        );
        pump(&ui, |ui| {
            !ui.global::<crate::ImageStorageSettings>()
                .get_thumbnail_cache_busy()
        });
        assert!(
            !ui.global::<crate::ImageStorageSettings>()
                .get_thumbnail_cache_info()
                .is_empty()
        );
        ui.global::<crate::ImageStorageSettings>()
            .invoke_thumbnail_cache_clear();
        assert!(
            ui.global::<crate::ImageStorageSettings>()
                .get_thumbnail_cache_busy()
        );
        pump(&ui, |ui| {
            !ui.global::<crate::ImageStorageSettings>()
                .get_thumbnail_cache_busy()
        });
        assert!(
            ui.global::<crate::ImageStorageSettings>()
                .get_thumbnail_cache_status()
                .contains("0")
        );
        assert!(
            app.thumbnail_service.get().is_none(),
            "cache maintenance never starts thumbnail work"
        );
        assert_eq!(
            app.services.domain.snapshot().unwrap().history,
            before_cache.history
        );
        assert_eq!(
            app.services.domain.snapshot().unwrap().progress,
            before_cache.progress
        );
        app.view().choices = vec![Choice {
            package: "gallery-fixture".into(),
            id: "gallery-fixture".into(),
            name: "Gallery fixture".into(),
            enabled: true,
            filters: false,
            language: "en".into(),
            latest: false,
        }];
        app.view().saved_catalogue = Some(CatalogueState {
            source_id: "gallery-fixture".into(),
            rows: vec![single.clone(), multi.clone(), manga.clone()],
            query: "retained query".into(),
            catalogue: "popular".into(),
            page: 2,
            has_more: true,
            page_error: String::new(),
            selected: 0,
            filters_active: false,
        });
        ui.set_source_catalog_open(true);
        app.view().section = 3;
        ui.set_section(3);
        app.open_series(&ui, single.clone());
        assert_eq!(ui.get_section(), 23);
        assert!(ui.get_overview_busy());
        assert!(app.view().reader.is_none());
        pump(&ui, |ui| !ui.get_overview_busy());
        assert_eq!(ui.get_overview_status(), "");
        assert_eq!(ui.get_overview_tiles().row_count(), 3);
        assert_eq!(ui.get_overview_groups().row_count(), 0);
        assert!(
            app.services
                .domain
                .snapshot()
                .unwrap()
                .series
                .iter()
                .find(|s| s.id == single.id)
                .unwrap()
                .description
                .is_empty(),
            "entry uses listing metadata without eager details"
        );
        assert!(app.services.domain.snapshot().unwrap().progress.is_empty());
        assert!(app.services.domain.snapshot().unwrap().history.is_empty());
        // Grid zoom changes presentation only, and publishes its focal anchor
        // atomically before ListView can reflow the new column count.
        ui.invoke_overview_zoom_requested(2, "stable-1".into(), 0.15);
        assert_eq!(ui.get_overview_density(), 2);
        assert_eq!(ui.get_overview_restore_index(), 1);
        assert_eq!(ui.get_overview_restore_fraction(), 0.15);
        assert_eq!(
            app.view().overview.as_ref().unwrap().anchor,
            ("stable-1".into(), 0.15)
        );
        ui.invoke_overview_zoom_requested(0, "missing-image".into(), 0.);
        ui.invoke_overview_zoom_requested(0, "stable-0".into(), f32::NAN);
        assert_eq!(ui.get_overview_density(), 2);
        assert_eq!(ui.get_overview_restore_index(), 1);
        assert!(app.services.domain.snapshot().unwrap().progress.is_empty());
        assert!(app.services.domain.snapshot().unwrap().history.is_empty());
        ui.invoke_overview_anchor("stable-0".into(), 0.);
        assert_eq!(ui.get_overview_restore_index(), 0);
        assert_eq!(ui.get_overview_restore_fraction(), 0.);
        ui.invoke_overview_density_changed(0);
        activate(&ui, "Image 3");
        pump(&ui, |ui| !ui.get_reader_opening());
        assert_eq!(ui.get_reader_open_error(), "");
        assert_eq!(ui.get_section(), 6);
        assert_eq!(ui.get_reading_mode(), 1, "new collections default to LTR");
        assert_eq!(
            app.view().reader.as_ref().unwrap().anchor.page_id,
            "stable-2"
        );
        let owner = Arc::downgrade(&app);
        pump(&ui, move |_| {
            owner.upgrade().is_some_and(|app| {
                app.view()
                    .reader
                    .as_ref()
                    .is_some_and(|r| r.has_visible_pixels(2))
            })
        });
        app.flush();
        assert!(
            !app.services.domain.snapshot().unwrap().progress[0].completed,
            "last image first does not complete a collection"
        );
        ui.invoke_reader_action("back".into());
        assert_eq!(ui.get_section(), 23);
        assert_eq!(ui.get_overview_density(), 0);
        assert_eq!(
            app.view().overview.as_ref().unwrap().anchor,
            ("stable-0".into(), 0.)
        );
        let global_settings = crate::reader_settings::snapshot();
        for mode in [
            ReadingMode::RightToLeft,
            ReadingMode::Webtoon,
            ReadingMode::Vertical,
        ] {
            let choice = reader_modes::ModeChoice::Manual(mode);
            reader_modes::save(&single.id, choice).unwrap();
            activate(&ui, "Image 3");
            pump(&ui, |ui| !ui.get_reader_opening());
            assert_eq!(app.view().reader.as_ref().unwrap().mode, mode);
            assert_eq!(ui.get_reading_mode(), mode_index(mode));
            let owner = Arc::downgrade(&app);
            pump(&ui, move |_| {
                owner.upgrade().is_some_and(|app| {
                    app.view()
                        .reader
                        .as_ref()
                        .is_some_and(|r| r.has_visible_pixels(2))
                })
            });
            app.flush();
            assert!(
                !app.services
                    .domain
                    .snapshot()
                    .unwrap()
                    .progress
                    .iter()
                    .find(|p| p.chapter.series == single.id)
                    .unwrap()
                    .completed
            );
            assert_eq!(crate::reader_settings::snapshot(), global_settings);
            assert_eq!(reader_modes::load(&single.id, None).unwrap(), choice);
            ui.invoke_reader_action("back".into());
            assert_eq!(ui.get_section(), 23);
        }
        reader_modes::save(&single.id, reader_modes::ModeChoice::Default).unwrap();
        activate(&ui, "Image 1");
        pump(&ui, |ui| !ui.get_reader_opening());
        assert_eq!(
            app.view().reader.as_ref().unwrap().anchor.page_id,
            "stable-0"
        );
        ui.invoke_reader_action("pages".into());
        assert_eq!(ui.get_section(), 23);
        app.flush();
        let before_metadata = app.services.domain.snapshot().unwrap();
        let original_inventory = app.view().overview.as_ref().unwrap().inventory.clone();
        app.services
            .sources
            .set_settings("gallery-fixture", json!({"explicitMetadata":true}))
            .unwrap();
        activate(&ui, "Overview options");
        assert!(ui.get_entry_menu_visible());
        activate(&ui, "Metadata");
        assert_eq!(ui.get_section(), 5);
        assert!(ui.get_busy());
        pump(&ui, |ui| !ui.get_busy());
        assert_eq!(ui.get_status(), "");
        assert_eq!(ui.get_series_description(), "Full collection description");
        assert!(ui.get_series_metadata().contains("Fixture author"));
        assert!(Arc::ptr_eq(
            &original_inventory,
            &app.view().overview.as_ref().unwrap().inventory
        ));
        assert_eq!(
            app.services.domain.snapshot().unwrap().history,
            before_metadata.history
        );
        assert_eq!(
            app.services.domain.snapshot().unwrap().progress,
            before_metadata.progress
        );
        ui.invoke_go_back();
        assert_eq!(
            ui.get_section(),
            23,
            "metadata Back restores the retained grid"
        );
        activate(&ui, "Overview options");
        activate(&ui, "Metadata");
        ui.invoke_go_back();
        slint::Timer::single_shot(Duration::from_millis(50), || {
            slint::quit_event_loop().unwrap();
        });
        slint::run_event_loop().unwrap();
        assert_eq!(
            ui.get_section(),
            23,
            "late metadata completion cannot reopen details after Back"
        );
        ui.invoke_overview_back();
        pump(&ui, |ui| ui.get_section() == 3);
        assert_eq!(ui.get_section(), 3);
        assert!(ui.get_source_catalog_open());
        assert_eq!(app.view().query, "retained query");
        assert_eq!(
            app.view().page,
            2,
            "Back restores the originating source catalogue"
        );
        app.flush();
        let before_history = app.services.domain.snapshot().unwrap();
        app.view().section = 2;
        ui.set_section(2);
        app.open_chapter(&ui, ChapterId::new(single.id.clone(), "single-a"));
        assert_eq!(
            ui.get_section(),
            23,
            "History chapter opens its collection overview"
        );
        assert!(app.view().reader.is_none());
        pump(&ui, |ui| !ui.get_overview_busy());
        assert_eq!(
            ui.get_overview_current_index(),
            0,
            "highlight last viewed stable image"
        );
        let after_history = app.services.domain.snapshot().unwrap();
        assert_eq!(before_history.progress, after_history.progress);
        assert_eq!(before_history.history, after_history.history);
        ui.invoke_overview_back();
        assert_eq!(ui.get_section(), 2);
        app.view().section = 3;
        ui.set_section(3);
        app.open_series(&ui, multi.clone());
        pump(&ui, |ui| !ui.get_overview_busy());
        assert_eq!(ui.get_overview_groups().row_count(), 2);
        assert_eq!(ui.get_overview_tiles().row_count(), 3);
        assert_eq!(
            app.view().overview.as_ref().unwrap().chapter.local_id,
            "multi-a"
        );
        ui.invoke_overview_group_selected(1);
        assert!(ui.get_overview_busy());
        pump(&ui, |ui| !ui.get_overview_busy());
        assert_eq!(ui.get_overview_tiles().row_count(), 1);
        assert_eq!(
            app.view().overview.as_ref().unwrap().chapter.local_id,
            "multi-b"
        );
        ui.invoke_overview_back();
        let chapter = ChapterId::new(manga.id.clone(), "manga-a");
        let cached = crate::services::chapter_from_source(
            &manga.id,
            serde_json::from_value(
                json!({"id":"manga-a","seriesId":"manga","title":"Manga chapter","sourceOrder":0}),
            )
            .unwrap(),
        )
        .unwrap();
        app.services
            .save_details(manga.clone(), vec![cached])
            .unwrap();
        app.open_series(&ui, manga);
        assert_eq!(
            ui.get_section(),
            5,
            "untagged one-shot retains the reader detail route"
        );
        assert!(app.view().reader.is_none());
        assert_eq!(chapter.local_id, "manga-a");
        // Header download keeps the source's stable identities and remains
        // separate from derivative fetching; only this action creates offline originals.
        let downloaded = ChapterId::new(single.id.clone(), "single-a");
        assert!(app.services.downloads.offline(&downloaded).is_err());
        app.view().section = 0;
        ui.set_section(0);
        app.open_series(&ui, single.clone());
        pump(&ui, |ui| !ui.get_overview_busy());
        activate(&ui, "Overview options");
        activate(&ui, "Download");
        assert_eq!(ui.get_section(), 23);
        let owner = Arc::downgrade(&app);
        let chapter = downloaded.clone();
        pump(&ui, move |_| {
            owner
                .upgrade()
                .is_some_and(|app| app.services.downloads.offline(&chapter).is_ok())
        });
        ui.invoke_overview_back();
        app.services.sources.remove("gallery-fixture").unwrap();
        drop(source);
        app.view().section = 0;
        ui.set_section(0);
        app.open_series(&ui, single);
        pump(&ui, |ui| !ui.get_overview_busy());
        assert_eq!(ui.get_overview_status(), "");
        assert_eq!(ui.get_overview_tiles().row_count(), 3);
        activate(&ui, "Overview options");
        activate(&ui, "Metadata");
        pump(&ui, |ui| !ui.get_busy());
        assert_eq!(ui.get_section(), 5);
        assert_eq!(
            ui.get_series_description(),
            "Full collection description",
            "offline metadata remains visible"
        );
        assert!(
            !ui.get_status().is_empty(),
            "offline refresh reports the saved-metadata fallback"
        );
        ui.invoke_go_back();
        assert_eq!(ui.get_section(), 23);
        activate(&ui, "Image 2");
        pump(&ui, |ui| !ui.get_reader_opening());
        assert_eq!(ui.get_reader_open_error(), "");
        assert_eq!(
            app.view().reader.as_ref().unwrap().anchor.page_id,
            "stable-1"
        );
        assert!(
            app.view()
                .reader
                .as_ref()
                .unwrap()
                .pages
                .iter()
                .all(|p| matches!(p, PageInput::Verified(resource) if matches!(resource.kind, crate::page_inventory::ResourceKind::Local { .. })))
        );
        ui.invoke_reader_action("back".into());
        ui.invoke_overview_back();
        assert_eq!(ui.get_section(), 0);
        app.flush();
        let before_error = app.services.domain.snapshot().unwrap();
        let missing = Series {
            id: reader_domain::SeriesId::new("missing-addon", "missing-collection"),
            title: "Unavailable collection".into(),
            publication_status: Default::default(),
            description: String::new(),
            authors: vec![],
            genres: vec![],
            cover_url: None,
            canonical_url: None,
        };
        crate::content_presentation::Settings::set_user(
            &missing.id,
            crate::content_presentation::UserPreference::Gallery,
        )
        .unwrap();
        app.open_series(&ui, missing);
        assert!(ui.get_overview_busy());
        assert!(app.view().reader.is_none());
        pump(&ui, |ui| !ui.get_overview_busy());
        assert!(
            !ui.get_overview_status().is_empty(),
            "inventory failure stays in the header"
        );
        ui.invoke_overview_retry();
        assert!(ui.get_overview_busy());
        pump(&ui, |ui| !ui.get_overview_busy());
        let after_error = app.services.domain.snapshot().unwrap();
        assert_eq!(before_error.progress, after_error.progress);
        assert_eq!(before_error.history, after_error.history);
        ui.invoke_overview_back();
        assert_eq!(ui.get_section(), 0);
        app.image_worker_live.store(false, Ordering::Release);
        app.invalidate();
    }
}

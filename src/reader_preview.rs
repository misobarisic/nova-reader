//! Opt-in raster previews of the production reader, controls and preferences.
//! Run this ignored test alone: Nova storage owns one directory per process.
//! Set READER_UI_PREVIEW_BOUNDARIES_ONLY=1 for just the chapter-separator PNGs.
use super::*;
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum PreviewFrame {
    Osd {
        width: u32,
        height: u32,
        name: &'static str,
    },
    ReaderTab {
        tab: &'static str,
        name: &'static str,
    },
    Boundary {
        width: u32,
        height: u32,
        name: &'static str,
    },
    Global {
        width: u32,
        height: u32,
        name: &'static str,
    },
}
impl PreviewFrame {
    fn name(self) -> &'static str {
        match self {
            Self::Osd { name, .. }
            | Self::ReaderTab { name, .. }
            | Self::Boundary { name, .. }
            | Self::Global { name, .. } => name,
        }
    }
}

fn activate(ui: &AppWindow, label: &str, role: AccessibleRole) {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(role))
        .unwrap_or_else(|| panic!("missing active {label} control"))
        .invoke_accessible_default_action();
}

fn snapshot(ui: &AppWindow, output: &std::path::Path, name: &str) {
    let pixels = ui.window().take_snapshot().expect("software reader pixels");
    image::save_buffer(
        output.join(format!("{name}.png")),
        pixels.as_bytes(),
        pixels.width(),
        pixels.height(),
        image::ColorType::Rgba8,
    )
    .expect("save actual application preview");
    eprintln!(
        "Reader preview: {} ({}×{})",
        output.join(format!("{name}.png")).display(),
        pixels.width(),
        pixels.height()
    );
}

fn reader_loaded(app: &Controller, ui: &AppWindow) -> bool {
    let view = app.view();
    let Some(reader) = &view.reader else {
        return false;
    };
    !ui.get_busy()
        && reader.failed.is_empty()
        && reader.requested.is_empty()
        && reader.visible_end() < ui.get_pages().row_count()
        && (reader.anchor.page_index..=reader.visible_end()).all(|i| {
            ui.get_pages()
                .row_data(i)
                .is_some_and(|page| page.loaded && page.tiles.row_count() > 0)
        })
}

fn prepare_separator_preview(app: &Arc<Controller>, ui: &AppWindow) {
    ui.set_reader_osd(false);
    ui.invoke_mode_changed(0);
    app.reader_zoom(ui, 1., 0.5, 0.5);
    // Keep the real imported image-worker output. Only partition its page
    // inventory into two fixture chapters to expose the production separator.
    let identities = {
        let mut view = app.view();
        let reader = view.reader.as_mut().unwrap();
        assert!(reader.pages.len() >= 2);
        if reader.segments.len() < 2 {
            let mut next = reader.chapter.clone();
            next.local_id = "separator-preview-next".into();
            reader.segments = vec![
                reader_continuity::ChapterSegment {
                    chapter: reader.chapter.clone(),
                    title: "Chapter 1".into(),
                    start: 0,
                    end: 1,
                    source: reader.source.clone(),
                    completed: false,
                },
                reader_continuity::ChapterSegment {
                    chapter: next,
                    title: "Chapter 2".into(),
                    start: 1,
                    end: reader.pages.len(),
                    source: reader.source.clone(),
                    completed: false,
                },
            ];
        }
        (0..reader.pages.len())
            .map(|index| reader.row_page_id(index))
            .collect::<Vec<_>>()
    };
    let pages = ui.get_pages();
    for (index, identity) in identities.into_iter().enumerate() {
        let mut row = pages.row_data(index).unwrap();
        row.page_id = identity.into();
        pages.set_row_data(index, row);
    }
    app.reflow(ui, ui.get_reader_width(), ui.get_reader_viewport_height());
    app.update_reader_geometry(ui);
    app.publish_reader_controls(ui);
    center_separator_preview(app, ui);
}

fn center_separator_preview(app: &Arc<Controller>, ui: &AppWindow) {
    let width = ui.get_reader_width();
    let height = ui.get_reader_viewport_height();
    let progress = {
        let view = app.view();
        let reader = view.reader.as_ref().unwrap();
        assert_eq!(reader.gap_after(0), 96., "preview the doubled chapter gap");
        let page_height = reader.ratios[0] * width;
        let offset = page_height + reader.gap_after(0) / 2. - height / 2.;
        assert!(
            offset >= 0. && offset < page_height,
            "use portrait preview artwork"
        );
        offset / page_height
    };
    // Explicit seek requests the actual decoded rows on either side; it also
    // bypasses ordinary-scroll admission while the preview viewport is loading.
    app.reader_seek(ui, progress);
}

fn assert_live_inheritance(ui: &AppWindow) {
    // These callbacks are the same persistence/projection path used by the form.
    assert_eq!(
        ui.get_reader_mode_choice(),
        6,
        "a new series inherits defaults"
    );
    assert_eq!(
        ui.get_reading_mode(),
        0,
        "the preview source is classified as Webtoon"
    );
    ui.global::<crate::ReaderSettings>()
        .invoke_settings_changed("default_mode".into(), 1);
    assert_eq!(
        ui.get_reading_mode(),
        1,
        "Default follows a changed global manual mode"
    );
    ui.invoke_mode_changed(4);
    assert_eq!(
        ui.get_reading_mode(),
        0,
        "explicit Automatic bypasses the manual global default"
    );
    ui.invoke_mode_changed(6);
    assert_eq!(ui.get_reading_mode(), 1);
    ui.global::<crate::ReaderSettings>()
        .invoke_settings_changed("default_mode".into(), 4);
    assert_eq!(ui.get_reading_mode(), 0);
    assert_eq!(ui.get_reader_mode_choice(), 6);
}

#[test]
#[ignore = "renders production phone/desktop reader and global settings PNG previews"]
fn render_reader_settings_previews() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: false,
        threading: true,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    nova_storage::init_at(temp.path());
    let ui = AppWindow::new().unwrap();
    crate::appearance::init(&ui).unwrap();
    crate::capture_settings::init(&ui).unwrap();
    crate::image_storage::init(&ui).unwrap();
    crate::reading_preferences::init(&ui).unwrap();
    crate::reader_settings::init().unwrap();
    ui.global::<crate::Anim>().set_enabled(false);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());

    let output = PathBuf::from(
        std::env::var("READER_UI_PREVIEW_DIR")
            .unwrap_or_else(|_| "/workspace/artifacts/reader-settings-v0122".into()),
    );
    std::fs::create_dir_all(&output).unwrap();
    let comic = std::env::var_os("READER_UI_PREVIEW_PAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let public_domain =
                PathBuf::from("/workspace/artifacts/reader-preview-little-nemo-1905-10-15.jpg");
            if public_domain.exists() {
                public_domain
            } else {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("examples/addons/neutral-fixture/pages/portrait.png")
            }
        });
    let input = temp.path().join("Little Nemo in Slumberland");
    std::fs::create_dir_all(&input).unwrap();
    let extension = comic.extension().and_then(|x| x.to_str()).unwrap_or("png");
    for page in 1..=3 {
        std::fs::copy(&comic, input.join(format!("{page:02}.{extension}"))).unwrap();
    }
    std::fs::write(output.join("preview-source.txt"), format!(
        "Actual AppWindow, production Controller.open_chapter, local import, image worker, ReaderEngine and reader_controls wiring.\nSource image: {}\nDefault sample when available: Winsor McCay, Little Nemo in Slumberland, October 15, 1905 (public domain).\nhttps://commons.wikimedia.org/wiki/File:Little_Nemo_1905-10-15.jpg\nThe fixture uses Webtoon metadata to exercise Automatic vertical reading and repeats the sample across three local pages.\n", comic.display(),
    )).unwrap();
    std::fs::write(output.join("separator-preview-source.txt"), format!(
        "Actual production AppWindow software raster, local-import image worker and chapter-boundary model.\nSource image: {}\nThe imported three-page inventory is partitioned into two fixture ChapterSegments; existing decoded image tiles are retained. This exercises separator rendering, not cross-chapter source preparation.\nOutputs: phone-reader-chapter-separator.png (390x844), desktop-reader-chapter-separator.png (1120x780).\nThe 96px production gap is centered between adjacent real image rows with reader controls hidden.\n", comic.display(),
    )).unwrap();
    let services = Services::open(&temp.path().join("data")).unwrap();
    let series = services.import_local(&input, true).unwrap();
    services
        .domain
        .update(|state| {
            let saved = state
                .series
                .iter_mut()
                .find(|saved| saved.id == series)
                .unwrap();
            saved.genres = vec!["Webtoon".into()];
            let chapter = state
                .chapters
                .iter_mut()
                .find(|chapter| chapter.id.series == series)
                .unwrap();
            chapter.title = "Chapter 1 · October 15, 1905".into();
            Ok(())
        })
        .unwrap();
    let chapter = services
        .domain
        .snapshot()
        .unwrap()
        .chapters
        .into_iter()
        .find(|chapter| chapter.id.series == series)
        .unwrap()
        .id;
    let (images, receiver) = mpsc::channel();
    let app = Arc::new(Controller {
        progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
            services.domain.clone(),
        )
        .unwrap(),
        artwork: crate::artwork::ArtworkLoader::new(
            services.clone(),
            ui.as_weak(),
            temp.path().join("cache/posters"),
        ),
        services,
        chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
            temp.path().join("cache/chapters"),
        )),
        capture_originals: Mutex::default(),
        thumbnail_service: OnceLock::new(),
        image_worker_live: AtomicBool::new(true),
        view: Mutex::new(View::default()),
        generation: AtomicU64::new(0),
        cancel: Mutex::new(CancelToken::default()),
        images,
        download_serial: Mutex::new(()),
        active_image: Mutex::new(None),
        continuation_work: Default::default(),
        metadata_refresh: Arc::default(),
    });
    reader_controls::wire(&app, &ui);
    wire_reader_context(&app, &ui);
    ui.on_navigate({
        let app = app.clone();
        let weak = ui.as_weak();
        move |section| {
            if let Some(ui) = weak.upgrade() {
                app.navigate(&ui, section);
            }
        }
    });
    ui.on_reader_reflow({
        let app = app.clone();
        let weak = ui.as_weak();
        move |width, height| {
            if let Some(ui) = weak.upgrade() {
                app.reflow(&ui, width, height);
            }
        }
    });
    ui.on_reader_position({
        let app = app.clone();
        let weak = ui.as_weak();
        move |offset, width, height| {
            if let Some(ui) = weak.upgrade() {
                app.position(&ui, offset, width, height);
            }
        }
    });
    ui.on_mode_changed({
        let app = app.clone();
        let weak = ui.as_weak();
        move |mode| {
            if let Some(ui) = weak.upgrade() {
                app.choose_reader_mode(&ui, mode, reader_modes::save);
            }
        }
    });
    let worker = {
        let weak = Arc::downgrade(&app);
        let ui = ui.as_weak();
        std::thread::spawn(move || Controller::image_worker(weak, receiver, ui))
    };
    ui.show().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(390, 844));
    app.open_chapter(&ui, chapter);

    let boundaries_only = std::env::var_os("READER_UI_PREVIEW_BOUNDARIES_ONLY").is_some();
    let frames = [
        PreviewFrame::Osd {
            width: 390,
            height: 844,
            name: "phone-reader-osd",
        },
        PreviewFrame::ReaderTab {
            tab: "Reading mode",
            name: "phone-reader-reading-mode",
        },
        PreviewFrame::ReaderTab {
            tab: "General",
            name: "phone-reader-general",
        },
        PreviewFrame::ReaderTab {
            tab: "Filters",
            name: "phone-reader-filters",
        },
        PreviewFrame::Osd {
            width: 1120,
            height: 780,
            name: "desktop-reader-osd",
        },
        PreviewFrame::ReaderTab {
            tab: "Reading mode",
            name: "desktop-reader-reading-mode",
        },
        PreviewFrame::ReaderTab {
            tab: "General",
            name: "desktop-reader-general",
        },
        PreviewFrame::ReaderTab {
            tab: "Filters",
            name: "desktop-reader-filters",
        },
        PreviewFrame::Boundary {
            width: 390,
            height: 844,
            name: "phone-reader-chapter-separator",
        },
        PreviewFrame::Boundary {
            width: 1120,
            height: 780,
            name: "desktop-reader-chapter-separator",
        },
        PreviewFrame::Global {
            width: 390,
            height: 844,
            name: "phone-global-reader-settings",
        },
        PreviewFrame::Global {
            width: 1120,
            height: 780,
            name: "desktop-global-reader-settings",
        },
    ]
    .into_iter()
    .filter(|frame| !boundaries_only || matches!(frame, PreviewFrame::Boundary { .. }))
    .collect::<Vec<_>>();
    let next = Rc::new(Cell::new(0usize));
    let pending = Rc::new(Cell::new(false));
    let inheritance_checked = Rc::new(Cell::new(false));
    let ready_at = Rc::new(Cell::new(Instant::now()));
    let start = Instant::now();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), {
        let app = app.clone();
        let ui = ui.as_weak();
        move || {
            let ui = ui.upgrade().unwrap();
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "preview timed out: stage {}, status {}",
                next.get(),
                ui.get_status()
            );
            if next.get() == frames.len() {
                slint::quit_event_loop().unwrap();
                return;
            }
            let frame = frames[next.get()];
            if !matches!(frame, PreviewFrame::Global { .. }) && !reader_loaded(&app, &ui) {
                return;
            }
            if !inheritance_checked.get() {
                assert_live_inheritance(&ui);
                inheritance_checked.set(true);
                return;
            }
            if !pending.get() {
                match frame {
                    PreviewFrame::Osd { width, height, .. } => {
                        if ElementHandle::find_by_accessible_label(&ui, "Close reader settings")
                            .next()
                            .is_some()
                        {
                            activate(&ui, "Close reader settings", AccessibleRole::Button);
                        }
                        ui.window()
                            .set_size(slint::PhysicalSize::new(width, height));
                        ui.set_touch_menus(width < 700);
                        ui.set_reader_osd(true);
                    }
                    PreviewFrame::ReaderTab { tab, .. } => {
                        if tab == "Reading mode" {
                            activate(&ui, "Settings", AccessibleRole::Button);
                        } else {
                            activate(&ui, tab, AccessibleRole::Tab);
                        }
                    }
                    PreviewFrame::Boundary { width, height, .. } => {
                        ui.window()
                            .set_size(slint::PhysicalSize::new(width, height));
                        ui.set_touch_menus(width < 700);
                        prepare_separator_preview(&app, &ui);
                    }
                    PreviewFrame::Global { width, height, .. } => {
                        ui.window()
                            .set_size(slint::PhysicalSize::new(width, height));
                        ui.set_touch_menus(width < 700);
                        app.navigate(&ui, 19);
                    }
                }
                ready_at.set(Instant::now() + Duration::from_millis(300));
                pending.set(true);
                return;
            }
            if Instant::now() < ready_at.get() {
                return;
            }
            if matches!(frame, PreviewFrame::Boundary { .. }) {
                center_separator_preview(&app, &ui);
            }
            snapshot(&ui, &output, frame.name());
            pending.set(false);
            next.set(next.get() + 1);
        }
    });
    slint::run_event_loop().unwrap();
    timer.stop();
    app.image_worker_live.store(false, Ordering::Release);
    app.invalidate();
    app.flush();
    worker.join().unwrap();
    ui.hide().unwrap();
}

/// Exercise first-frame quality through the production chapter/image worker,
/// rather than constructing a model with textures already at the desired size.
#[test]
fn paged_first_frame_and_zoom_keep_native_detail_at_android_density() {
    const CHILD: &str = "READER_PAGED_NATIVE_DENSITY_CHILD";
    let Ok(selection) = std::env::var(CHILD) else {
        for mode in ["1", "2"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_preview::paged_first_frame_and_zoom_keep_native_detail_at_android_density", "--nocapture"])
                .env(CHILD, mode).output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let selection: i32 = selection.parse().unwrap();
    let mode = if selection == 1 {
        ReadingMode::LeftToRight
    } else {
        ReadingMode::RightToLeft
    };
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: false,
        threading: true,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    nova_storage::init_at(temp.path());
    crate::reader_settings::init().unwrap();
    let ui = AppWindow::new().unwrap();
    ui.global::<crate::Anim>().set_enabled(false);
    let input = temp.path().join("Native detail fixture");
    std::fs::create_dir_all(&input).unwrap();
    let original = image::RgbImage::from_fn(1920, 5760, |x, y| {
        let line = if x % 8 < 4 { 20 } else { 235 };
        image::Rgb([line, if y % 8 < 4 { 20 } else { 235 }, line])
    });
    original.save(input.join("01.png")).unwrap();
    drop(original);
    for page in ["02.png", "03.png"] {
        std::fs::copy(input.join("01.png"), input.join(page)).unwrap();
    }
    let services = Services::open(&temp.path().join("data")).unwrap();
    let series = services.import_local(&input, true).unwrap();
    reader_modes::save(&series, ModeChoice::Manual(mode)).unwrap();
    let chapter = services
        .domain
        .snapshot()
        .unwrap()
        .chapters
        .into_iter()
        .find(|chapter| chapter.id.series == series)
        .unwrap()
        .id;
    let page_id = services.downloads.job(&chapter).unwrap().pages[1]
        .resource
        .id
        .clone();
    services
        .domain
        .update_checkpoint(|state| {
            state.progress.push(ReadingProgress {
                chapter: chapter.clone(),
                anchor: ResumeAnchor::new(page_id, 1, 0.).unwrap(),
                mode,
                last_read_at: 1,
                completed: false,
            });
            Ok(())
        })
        .unwrap();
    let (images, receiver) = mpsc::channel();
    let app = Arc::new(Controller {
        progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
            services.domain.clone(),
        )
        .unwrap(),
        artwork: crate::artwork::ArtworkLoader::new(
            services.clone(),
            ui.as_weak(),
            temp.path().join("cache/posters"),
        ),
        services,
        chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
            temp.path().join("cache/chapters"),
        )),
        capture_originals: Mutex::default(),
        thumbnail_service: OnceLock::new(),
        image_worker_live: AtomicBool::new(true),
        view: Mutex::new(View::default()),
        generation: AtomicU64::new(0),
        cancel: Mutex::new(CancelToken::default()),
        images,
        download_serial: Mutex::new(()),
        active_image: Mutex::new(None),
        continuation_work: Default::default(),
        metadata_refresh: Arc::default(),
    });
    reader_controls::wire(&app, &ui);
    // Simulate Android layout policy and actual device-pixel density separately.
    ui.set_reader_platform_android(true);
    ui.on_reader_reflow({
        let app = app.clone();
        let weak = ui.as_weak();
        move |width, height| {
            if let Some(ui) = weak.upgrade() {
                app.reflow(&ui, width, height);
            }
        }
    });
    let worker = {
        let weak = Arc::downgrade(&app);
        let ui = ui.as_weak();
        std::thread::spawn(move || Controller::image_worker(weak, receiver, ui))
    };
    ui.show().unwrap();
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 2.75 });
    // TestingBackend's set_size path assumes DPR1; emit the physical surface
    // and logical resize separately, as an Android window backend does.
    ui.window().set_size(slint::PhysicalSize::new(1073, 2321));
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(390., 844.),
        });
    app.open_chapter(&ui, chapter);
    let stage = Rc::new(Cell::new(0));
    let start = Instant::now();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated,Duration::from_millis(10),{
        let app=app.clone();let weak=ui.as_weak();move||{
            let ui=weak.upgrade().unwrap();
            assert!(start.elapsed()<Duration::from_secs(30),"paged fixture stalled: {}",ui.get_status());
            if ui.get_busy() || !ui.get_pages().row_data(1).is_some_and(|row|row.loaded) {return;}
            assert_eq!(ui.get_current_page(),1);
            assert_eq!(ui.get_reading_mode(),selection);
            let artwork=ElementHandle::find_by_accessible_label(&ui,"Page 2 image").find(|element|element.accessible_role()==Some(AccessibleRole::Button)).unwrap();
            let logical_width=artwork.size().width;
            let density=ui.window().scale_factor();
            let expected_physical=(logical_width*density).ceil() as u32;
            let nominal_physical=(ui.get_reader_width()*density).ceil() as u32;
            let view=app.view();let reader=view.reader.as_ref().unwrap();
            let plan=reader.decoded_windows[&1];
            eprintln!("paged mode={selection} stage={} zoom={} viewport={}x{} artwork={}px DPR={} decoded={} native=1920",stage.get(),ui.get_zoom(),ui.get_reader_viewport_width(),ui.get_reader_viewport_height(),logical_width,density,plan.width);
            assert!((ui.get_reader_viewport_width()-390.).abs()<0.5);
            assert!((ui.get_reader_viewport_height()-844.).abs()<0.5);
            assert!(logical_width>250.);
            // Keep the previous texture while zoom replacement is in flight;
            // the very first resident frame must already cover initial DPR.
            if stage.get()==1 && plan.width<expected_physical.min(1920) {return;}
            assert!(plan.width>=expected_physical.min(1920),"first visible texture must cover actual physical artwork width; {} < {}",plan.width,expected_physical.min(1920));
            assert!(plan.width<=1920,"CPU display buffers must never upscale originals");
            if stage.get()==0 {assert!(plan.width>=nominal_physical.min(1920),"height-limited paged fit must preserve the same source-column detail as Webtoon");}
            assert!(plan.byte_len()<=READER_MODEL_BYTES);
            assert!(reader.retained.iter().map(|index|reader.decoded_bytes.get(index).copied().unwrap_or(0)).sum::<usize>()<=READER_MODEL_BYTES);
            assert!(ui.get_scroll_y()<=0. && -ui.get_scroll_y()<= (ui.get_reader_paged_height()-ui.get_reader_viewport_height()).max(0.)+0.01);
            drop(view);
            if !reader_loaded(&app,&ui){return;}
            if stage.get()==0 {
                assert_eq!(ui.get_zoom(),1.);
                assert!(ui.get_scroll_x().abs()<0.01 && ui.get_scroll_y().abs()<0.01,"initial paged resume must use page-local coordinates");
                stage.set(1);
                app.reader_zoom(&ui,2.,0.5,0.5);
                return;
            }
            assert_eq!(ui.get_zoom(),2.);
            assert!(plan.width>=expected_physical.min(1920));
            let dimensions=reader_engine::PageDimensions{width:1920,height:5760};
            let next=reader_engine::plan_display_window(dimensions,plan.width+1,0.,1.,READER_MODEL_BYTES);
            assert!(next.is_err(),"zoomed detail should use the largest full-page width fitting32MiB");
            slint::quit_event_loop().unwrap();
        }
    });
    slint::run_event_loop().unwrap();
    timer.stop();
    app.image_worker_live.store(false, Ordering::Release);
    app.invalidate();
    app.flush();
    worker.join().unwrap();
    ui.hide().unwrap();
}

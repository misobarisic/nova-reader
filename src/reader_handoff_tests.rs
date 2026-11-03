//! Deterministic reverse-demand regressions. Decode completions are gated by
//! the test, while the real controller, queue, admission and display planner run.
use super::*;

pub(super) fn fixture_app(
    ui: &AppWindow,
    directory: &std::path::Path,
    reader: Reader,
    live: bool,
) -> (Arc<Controller>, mpsc::Receiver<ImageTask>) {
    let services = Services::open(&directory.join("data")).unwrap();
    let (images, receiver) = mpsc::channel();
    let app = Arc::new(Controller {
        progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
            services.domain.clone(),
        )
        .unwrap(),
        artwork: crate::artwork::ArtworkLoader::new(
            services.clone(),
            ui.as_weak(),
            directory.join("posters"),
        ),
        services,
        chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
            directory.join("cache"),
        )),
        capture_originals: Mutex::default(),
        thumbnail_service: OnceLock::new(),
        image_worker_live: AtomicBool::new(live),
        view: Mutex::new(View {
            section: 6,
            reader: Some(reader),
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
    (app, receiver)
}

pub(super) fn pump_reader_until(app: &Arc<Controller>, ready: impl Fn(&Reader) -> bool + 'static) {
    let finished = std::rc::Rc::new(std::cell::Cell::new(false));
    let complete = finished.clone();
    let owner = app.clone();
    let start = std::time::Instant::now();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(5),
        move || {
            let view = owner.view();
            let r = view.reader.as_ref().unwrap();
            assert!(r.retained_bytes_without(usize::MAX) <= READER_MODEL_BYTES);
            if ready(r) {
                complete.set(true);
                slint::quit_event_loop().unwrap();
            } else if start.elapsed() > std::time::Duration::from_secs(10) {
                slint::quit_event_loop().unwrap();
            }
        },
    );
    slint::run_event_loop().unwrap();
    timer.stop();
    assert!(
        finished.get(),
        "worker/UI publication failed to reach expected state"
    );
}

fn presentation(dimensions: reader_engine::PageDimensions) -> reader_engine::PagePresentation {
    let mut p = reader_engine::page_presentation(
        &image::RgbaImage::new(1, 1),
        reader_engine::DisplayOptions::default(),
    )
    .unwrap();
    p.source = dimensions;
    p.cropped = dimensions;
    p.effective = dimensions;
    p
}

fn plan(
    reader: &Reader,
    index: usize,
    dimensions: reader_engine::PageDimensions,
) -> reader_engine::DisplayWindowPlan {
    let (width, (start, end), budget) = reader.window_request(index, dimensions).unwrap();
    reader_engine::plan_display_window(dimensions, width, start, end, budget).unwrap()
}

fn retain(
    reader: &mut Reader,
    index: usize,
    dimensions: reader_engine::PageDimensions,
    plan: reader_engine::DisplayWindowPlan,
) {
    reader.requested.remove(&index);
    reader.retained.insert(index);
    reader.decoded_bytes.insert(index, plan.byte_len());
    reader.decoded_widths.insert(index, reader.target_width);
    reader.decoded_windows.insert(index, plan);
    reader.presentations.insert(index, presentation(dimensions));
    reader.native_dimensions.insert(index, dimensions);
    reader.ratios[index] = dimensions.height as f32 / dimensions.width as f32;
    assert!(reader.retained_bytes_without(usize::MAX) <= READER_MODEL_BYTES);
}

#[test]
fn same_chapter_middle_resume_records_demand_without_moving_saved_anchor() {
    let dimensions = reader_engine::PageDimensions {
        width: 720,
        height: 25000,
    };
    for offset in [0., 0.4] {
        let mut reader = super::tests::tall_reader();
        reader.anchor = ResumeAnchor::new("page-1", 1, offset).unwrap();
        reader.ratios = vec![25000. / 720.; 2];
        reader.desired.extend([0, 1]);
        let current = plan(&reader, 1, dimensions);
        retain(&mut reader, 1, dimensions, current);
        assert!(current.first_row + current.rows < current.full_height);
        assert!(
            reader.defers_prefetch(0),
            "ordinary speculation keeps decoder reuse"
        );
        let before = reader.progress_record().anchor;
        reader.observe_reverse_scroll(0., 390., |i| i == 1);
        assert_eq!(reader.reverse_target(), Some(0));
        assert_eq!(reader.progress_record().anchor, before);
        assert_eq!(
            reader.loaded_scroll_offset(0., 390., 844., |i| i == 1),
            reader.page_position(1) * 390.
        );
        // A saved middle window must first be replaced with a covering top
        // window. Demand survives this admitted intra-page movement.
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
        let current = plan(&reader, 1, dimensions);
        retain(&mut reader, 1, dimensions, current);
        assert!(
            !reader.defers_prefetch(0),
            "demand bypasses the forward-only reuse rule"
        );
        let previous = plan(&reader, 0, dimensions);
        assert!(reader.accepts_window(0, dimensions, previous));
        assert_eq!(previous.first_row + previous.rows, previous.full_height);
        assert!(previous.byte_len() <= 16 * 1024 * 1024);
        retain(&mut reader, 0, dimensions, previous);
        assert_eq!(
            reader.reverse_target(),
            Some(0),
            "completion cannot release its reservation"
        );
        reader.observe_reverse_scroll(reader.page_position(1) * 390. + 20., 390., |_| true);
        assert!(
            reader.reverse_demand.is_none(),
            "moving away releases demand"
        );
    }
}

#[test]
fn unknown_visible_geometry_defers_tail_and_effective_dimensions_set_floors() {
    let mut reader = super::tests::tall_reader();
    reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
    reader.reverse_demand = Some(reader.row_page_id(0));
    // The placeholder ratio cannot certify any visible-coverage allocation.
    assert!(reader.handoff_budgets(None).is_none());
    assert!(reader.defers_prefetch(0));
    assert!(
        reader
            .window_request(
                0,
                reader_engine::PageDimensions {
                    width: 720,
                    height: 25000
                }
            )
            .is_none()
    );
    // Cropping/splitting/rotation can change the effective width independently
    // of native geometry. The budget must use this effective shape.
    let effective = reader_engine::PageDimensions {
        width: 512,
        height: 6144,
    };
    let mut p = presentation(effective);
    p.source = reader_engine::PageDimensions {
        width: 2048,
        height: 3072,
    };
    reader.presentations.insert(1, p);
    let budgets = reader.handoff_budgets(None).unwrap();
    let current = plan(&reader, 1, effective);
    assert_eq!(current.width, 512);
    assert!(current.byte_len() <= budgets[&1]);
    assert_eq!(budgets.values().sum::<usize>(), READER_MODEL_BYTES);
    assert!(reader.handoff_waits_for_visible(0));
    retain(&mut reader, 1, effective, current);
    assert!(!reader.handoff_waits_for_visible(0));
}

#[test]
fn fully_visible_model_budget_reaches_terminal_tail_admission_instead_of_waiting() {
    let dimensions = reader_engine::PageDimensions {
        width: 4096,
        height: 2048,
    };
    let mut reader = super::tests::tall_reader();
    reader.viewport_width = 2048.;
    reader.viewport_height = 1024.;
    reader.target_width = 4096;
    reader.ratios = vec![0.5; 2];
    reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
    let current = plan(&reader, 1, dimensions);
    assert_eq!(current.byte_len(), READER_MODEL_BYTES);
    retain(&mut reader, 1, dimensions, current);
    reader.reverse_demand = Some(reader.row_page_id(0));
    reader.desired.extend([0, 1]);
    reader.requested.insert(0);
    assert_eq!(reader.handoff_budgets(None).unwrap()[&0], 0);
    assert!(!reader.handoff_waits_for_visible(0));
    assert!(
        reader.start_image(0),
        "zero headroom must probe and report admission failure"
    );
    let (width, (start, end), budget) = reader.window_request(0, dimensions).unwrap();
    assert_eq!(budget, 0);
    assert!(reader_engine::plan_display_window(dimensions, width, start, end, budget).is_err());
    assert!(reader.has_visible_pixels(1));
    assert_eq!(
        reader.retained_bytes_without(usize::MAX),
        READER_MODEL_BYTES
    );
}

#[test]
fn reverse_handoff_reclaims_overscan_and_loads_two_preceding_images() {
    const CHILD: &str = "READER_REVERSE_HANDOFF_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app::reader_handoff_tests::reverse_handoff_reclaims_overscan_and_loads_two_preceding_images", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.set_reading_mode(0);
    ui.set_reader_side_padding(0);
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 2. });
    ui.window().set_size(slint::PhysicalSize::new(1024, 1600));
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(512., 800.),
        });
    let dir = tempfile::tempdir().unwrap();
    let services = Services::open(&dir.path().join("data")).unwrap();
    let (images, receiver) = mpsc::channel();
    let mut reader = super::tests::tall_reader();
    reader.pages = (0..4)
        .map(|i| PageInput::Local {
            path: PathBuf::new(),
            id: format!("page-{i}"),
        })
        .collect();
    reader.ratios = vec![6., 4., 6., 2.];
    reader.viewport_width = 512.;
    reader.viewport_height = 800.;
    reader.target_width = 1024;
    reader.anchor = ResumeAnchor::new("page-2", 2, 0.).unwrap();
    let tall = reader_engine::PageDimensions {
        width: 1024,
        height: 6144,
    };
    let previous = reader_engine::PageDimensions {
        width: 1024,
        height: 4096,
    };
    let original = plan(&reader, 2, tall);
    assert_eq!(original.byte_len(), 24 * 1024 * 1024);
    let speculative = plan(&reader, 1, previous);
    assert_eq!(speculative.byte_len(), 16 * 1024 * 1024);
    assert!(original.byte_len() + speculative.byte_len() > READER_MODEL_BYTES);
    retain(&mut reader, 2, tall, original);
    reader.budget_blocked.insert(1);
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
        view: Mutex::new(View {
            section: 6,
            reader: Some(reader),
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
    let rows = ui.get_pages();
    let mut row = rows.row_data(2).unwrap();
    row.loaded = true;
    rows.set_row_data(2, row);
    let before = app.view().reader.as_ref().unwrap().progress_record().anchor;
    let boundary = app.view().reader.as_ref().unwrap().page_position(2) * 512.;
    app.position(&ui, boundary - 20., 512., 800.);
    assert_eq!(
        app.view().reader.as_ref().unwrap().progress_record().anchor,
        before
    );
    let shrink = receiver
        .try_recv()
        .expect("visible overscan replacement is issued");
    assert_eq!(shrink.index, 2);
    assert!(
        receiver.try_recv().is_err(),
        "previous tail waits for publication to release bytes"
    );
    {
        let mut view = app.view();
        let r = view.reader.as_mut().unwrap();
        assert_eq!(r.target_width, 1024);
        assert_eq!(
            r.desired,
            BTreeSet::from([1, 2]),
            "opposite speculation is suppressed"
        );
        assert!(
            !r.accepts_window(1, previous, speculative),
            "already-running speculation rechecks the phase"
        );
        let replacement = plan(r, 2, tall);
        assert_eq!(replacement.width, original.width);
        assert!(replacement.byte_len() <= 16 * 1024 * 1024);
        assert!(r.accepts_window(2, tall, replacement));
        retain(r, 2, tall, replacement);
        assert!(r.has_visible_pixels(2));
    }
    app.request_images(&ui, 2);
    let tail = receiver
        .try_recv()
        .expect("released bytes restart the demanded tail");
    assert_eq!(tail.index, 1);
    {
        let mut view = app.view();
        let r = view.reader.as_mut().unwrap();
        let trailing = plan(r, 1, previous);
        assert!(r.accepts_window(1, previous, trailing));
        retain(r, 1, previous, trailing);
        let mut row = rows.row_data(1).unwrap();
        row.loaded = true;
        rows.set_row_data(1, row);
    }
    app.request_images(&ui, 2);
    assert!(
        receiver.try_recv().is_err(),
        "retained demand prevents immediate overscan reclamation"
    );
    app.position(&ui, boundary - 20., 512., 800.);
    assert_eq!(app.view().reader.as_ref().unwrap().anchor.page_index, 1);
    while let Ok(task) = receiver.try_recv() {
        let mut view = app.view();
        let r = view.reader.as_mut().unwrap();
        if r.start_image(task.index) {
            assert_eq!(task.index, 0);
            let speculative = plan(r, 0, tall);
            assert!(r.retained_bytes_without(0) + speculative.byte_len() > READER_MODEL_BYTES);
            r.requested.remove(&0);
            r.budget_blocked.insert(0);
        }
    }
    // Enter the previous image's head; a second reverse boundary must make
    // progress as well, including a tall previous image needing only its tail.
    let boundary = app.view().reader.as_ref().unwrap().page_position(1) * 512.;
    app.position(&ui, boundary, 512., 800.);
    while let Ok(task) = receiver.try_recv() {
        let mut view = app.view();
        let r = view.reader.as_mut().unwrap();
        if r.start_image(task.index) {
            assert_eq!(task.index, 0);
            r.requested.remove(&0);
            r.budget_blocked.insert(0);
        }
    }
    app.position(&ui, boundary - 20., 512., 800.);
    let tail = receiver
        .try_recv()
        .expect("the second preceding image is issued");
    assert_eq!(tail.index, 0);
    let mut view = app.view();
    let r = view.reader.as_mut().unwrap();
    let trailing = plan(r, 0, tall);
    assert!(trailing.first_row > 0);
    assert_eq!(trailing.first_row + trailing.rows, trailing.full_height);
    assert!(r.accepts_window(0, tall, trailing));
    retain(r, 0, tall, trailing);
    assert!(r.has_visible_pixels(1));
    assert_eq!(
        r.anchor.page_index, 1,
        "decoding never advances the saved anchor"
    );
}

#[test]
fn stale_presentation_cannot_mutate_rebased_or_replaced_pages() {
    let mut reader = super::tests::tall_reader();
    let chapter = reader.chapter.clone();
    let dimensions = reader_engine::PageDimensions {
        width: 720,
        height: 25000,
    };
    let p = presentation(dimensions);
    let task = ImageTask {
        generation: 4,
        index: 1,
    };
    let options = reader.display_options;
    assert!(
        record_task_presentation(&mut reader, 5, &task, (&chapter, "page-1"), options, p).is_err()
    );
    assert!(reader.presentations.is_empty());
    reader.pages.swap(0, 1);
    assert!(
        record_task_presentation(&mut reader, 4, &task, (&chapter, "page-1"), options, p).is_err()
    );
    assert!(reader.presentations.is_empty());
    reader.pages.truncate(1);
    assert!(
        record_task_presentation(&mut reader, 4, &task, (&chapter, "page-1"), options, p).is_err()
    );
    assert!(reader.presentations.is_empty());
}

#[test]
fn handoff_window_decode_preserves_native_width_and_source_pixels() {
    use std::io::Cursor;
    let dimensions = reader_engine::PageDimensions {
        width: 1024,
        height: 6144,
    };
    let mut reader = super::tests::tall_reader();
    reader.viewport_width = 512.;
    reader.viewport_height = 800.;
    reader.target_width = 1024;
    reader.ratios = vec![6.; 2];
    reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
    reader.reverse_demand = Some(reader.row_page_id(0));
    reader.presentations.insert(1, presentation(dimensions));
    let image = image::RgbaImage::from_fn(1024, 6144, |x, y| {
        image::Rgba([(x % 251) as u8, (y % 251) as u8, 89, 255])
    });
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let bytes = Arc::new(encoded.into_inner());
    let (width, (start, end), budget) = reader.window_request(1, dimensions).unwrap();
    let mut decoder = reader_engine::DisplayDecoder::default();
    let window = decoder
        .decode_window_with_options(
            &bytes,
            reader_engine::current_limits(),
            width,
            start,
            end,
            budget,
            reader.display_options,
        )
        .unwrap();
    assert_eq!(window.image.width(), 1024);
    assert!(window.plan.covers(start, end));
    assert!(window.plan.byte_len() <= 16 * 1024 * 1024);
    for y in [0, window.image.height() / 2, window.image.height() - 1] {
        assert_eq!(
            window.image.get_pixel(123, y).0,
            [123, ((window.plan.first_row + y) % 251) as u8, 89, 255]
        );
    }
}

#[test]
fn live_image_worker_resumes_midchapter_and_crosses_two_reverse_boundaries() {
    live_worker_handoff_case(false);
}

#[test]
fn failed_visible_replacement_pins_pixels_and_admits_smaller_previous_tail() {
    live_worker_handoff_case(true);
}

fn live_worker_handoff_case(fail_shrink: bool) {
    const CHILD: &str = "READER_LIVE_REVERSE_HANDOFF_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", if fail_shrink { "app::reader_handoff_tests::failed_visible_replacement_pins_pixels_and_admits_smaller_previous_tail" } else { "app::reader_handoff_tests::live_image_worker_resumes_midchapter_and_crosses_two_reverse_boundaries" }, "--nocapture"])
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
    ui.set_reading_mode(0);
    ui.set_reader_side_padding(0);
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 2. });
    ui.window().set_size(slint::PhysicalSize::new(1024, 1600));
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(512., 800.),
        });
    let dir = tempfile::tempdir().unwrap();
    let services = Services::open(&dir.path().join("data")).unwrap();
    let mut reader = super::tests::tall_reader();
    reader.pages.clear();
    for (index, height) in [6144, 4096, 6144].into_iter().enumerate() {
        let path = dir.path().join(format!("{index}.png"));
        image::RgbaImage::from_fn(1024, height, |x, y| {
            image::Rgba([(x % 251) as u8, (y % 251) as u8, 40 + index as u8, 255])
        })
        .save(&path)
        .unwrap();
        reader.pages.push(PageInput::Local {
            path,
            id: format!("page-{index}"),
        });
    }
    reader.ratios = vec![6., 4., 6.];
    reader.viewport_width = 512.;
    reader.viewport_height = 800.;
    reader.target_width = 1024;
    reader.anchor = ResumeAnchor::new("page-2", 2, 0.25).unwrap();
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
        image_worker_live: AtomicBool::new(true),
        view: Mutex::new(View {
            section: 6,
            reader: Some(reader),
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
    app.restore_position(&ui);
    let worker = Arc::downgrade(&app);
    let weak = ui.as_weak();
    let worker = std::thread::spawn(move || Controller::image_worker(worker, receiver, weak));
    app.request_images(&ui, 2);
    // Hold UI publication while the real worker fetches/probes/decompresses.
    // The worker's early presentation is generation/identity guarded, but no
    // final tile or progress admission can occur until the event loop resumes.
    let start = std::time::Instant::now();
    while !app
        .view()
        .reader
        .as_ref()
        .unwrap()
        .presentations
        .contains_key(&2)
    {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app.view().reader.as_ref().unwrap().retained.is_empty());
    assert_eq!(app.view().reader.as_ref().unwrap().anchor.offset(), 0.25);
    fn pump(
        app: &Arc<Controller>,
        allow_failed_shrink: bool,
        ready: impl Fn(&Reader) -> bool + 'static,
    ) {
        let complete = std::rc::Rc::new(std::cell::Cell::new(false));
        let finished = complete.clone();
        let owner = app.clone();
        let start = std::time::Instant::now();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(5),
            move || {
                let view = owner.view();
                let reader = view.reader.as_ref().unwrap();
                assert!(reader.retained_bytes_without(usize::MAX) <= READER_MODEL_BYTES);
                assert!(
                    reader.failed.is_empty()
                        || (allow_failed_shrink && reader.failed == BTreeSet::from([2])),
                    "valid local originals cannot fail: {:?}",
                    reader.failed
                );
                if ready(reader) {
                    finished.set(true);
                    slint::quit_event_loop().unwrap();
                } else if start.elapsed() > std::time::Duration::from_secs(10) {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        slint::run_event_loop().unwrap();
        timer.stop();
        assert!(
            complete.get(),
            "live image scheduler did not reach the expected state"
        );
    }
    pump(&app, false, |r| {
        r.retained.contains(&2) && r.budget_blocked.contains(&1)
    });
    assert_eq!(
        app.view().reader.as_ref().unwrap().decoded_bytes[&2],
        24 * 1024 * 1024
    );
    let original_limits = reader_engine::current_limits();
    if fail_shrink {
        let mut constrained = original_limits;
        // Native current is 24 MiB; only 6 MiB remains for each output copy,
        // less than its 6.25 MiB viewport. Previous native16 + tail8 fits36.
        constrained.max_decoded_bytes = 36 * 1024 * 1024;
        reader_engine::set_limits(constrained);
    }
    let boundary = app.view().reader.as_ref().unwrap().page_position(2) * 512.;
    app.position(&ui, boundary - 20., 512., 800.);
    {
        let view = app.view();
        let r = view.reader.as_ref().unwrap();
        assert_eq!(r.anchor.page_index, 2);
        assert_eq!(r.anchor.offset(), 0.);
        assert!(r.requested.contains(&2));
        assert!(
            !r.requested.contains(&1),
            "tail waits for the held shrink publication"
        );
        assert_eq!(r.decoded_bytes[&2], 24 * 1024 * 1024);
    }
    pump(&app, fail_shrink, |r| {
        r.retained.contains(&1) && r.retained.contains(&2)
    });
    {
        let view = app.view();
        let r = view.reader.as_ref().unwrap();
        if fail_shrink {
            assert_eq!(r.failed, BTreeSet::from([2]));
            assert_eq!(r.decoded_bytes[&2], 24 * 1024 * 1024);
            assert!(r.decoded_bytes[&1] <= 8 * 1024 * 1024);
        } else {
            assert!(r.decoded_bytes[&2] <= 16 * 1024 * 1024);
        }
        assert!(r.has_visible_pixels(2));
        assert_eq!(r.anchor.page_index, 2);
        assert_eq!(r.reverse_target(), Some(1));
    }
    if fail_shrink {
        let row = ui.get_pages().row_data(2).unwrap();
        assert!(row.loaded && row.tiles.row_count() > 0);
        assert!(!row.error.is_empty());
        let pixels = row.tiles.row_data(0).unwrap().image.to_rgba8().unwrap();
        assert_eq!(
            pixels.as_slice()[123].b,
            42,
            "failed replacement keeps the old source pixels"
        );
        app.position(&ui, boundary - 20., 512., 800.);
        assert_eq!(app.view().reader.as_ref().unwrap().anchor.page_index, 1);
        app.image_worker_live.store(false, Ordering::Release);
        app.invalidate();
        worker.join().unwrap();
        reader_engine::set_limits(original_limits);
        return;
    }
    app.position(&ui, boundary - 20., 512., 800.);
    assert_eq!(app.view().reader.as_ref().unwrap().anchor.page_index, 1);
    let boundary = app.view().reader.as_ref().unwrap().page_position(1) * 512.;
    app.position(&ui, boundary, 512., 800.);
    app.position(&ui, boundary - 20., 512., 800.);
    pump(&app, false, |r| {
        r.retained.contains(&0) && r.retained.contains(&1)
    });
    {
        let view = app.view();
        let r = view.reader.as_ref().unwrap();
        assert!(r.has_visible_pixels(1));
        assert_eq!(r.anchor.page_index, 1);
        let trailing = r.decoded_windows[&0];
        assert!(trailing.first_row > 0);
        assert_eq!(trailing.first_row + trailing.rows, trailing.full_height);
        // Verify final Slint buffers, not merely row.loaded or planner metadata.
        for index in [0, 1] {
            let row = ui.get_pages().row_data(index).unwrap();
            assert!(row.loaded && !row.preview && row.tiles.row_count() > 0);
            for tile in row.tiles.iter() {
                let pixels = tile.image.to_rgba8().unwrap();
                let source_y =
                    (tile.offset * r.decoded_windows[&index].full_height as f32).round() as u32;
                let pixel = pixels.as_slice()[123];
                assert_eq!(
                    [pixel.r, pixel.g, pixel.b, pixel.a],
                    [123, (source_y % 251) as u8, 40 + index as u8, 255]
                );
            }
        }
    }
    app.position(&ui, boundary - 20., 512., 800.);
    assert_eq!(app.view().reader.as_ref().unwrap().anchor.page_index, 0);
    app.image_worker_live.store(false, Ordering::Release);
    app.invalidate();
    worker.join().unwrap();
}

#[test]
fn live_worker_large_reverse_and_forward_gestures_preserve_covered_anchor() {
    const CHILD: &str = "READER_LIVE_WINDOW_EDGES_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app::reader_handoff_tests::live_worker_large_reverse_and_forward_gestures_preserve_covered_anchor", "--nocapture"])
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
    ui.set_reading_mode(0);
    ui.set_reader_side_padding(0);
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 2. });
    ui.window().set_size(slint::PhysicalSize::new(1024, 1600));
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(512., 800.),
        });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.png");
    image::RgbaImage::from_fn(1024, 25000, |x, y| {
        image::Rgba([(x % 251) as u8, (y % 251) as u8, 97, 255])
    })
    .save(&path)
    .unwrap();
    let mut reader = super::tests::tall_reader();
    reader.pages = vec![PageInput::Local {
        path,
        id: "page-0".into(),
    }];
    reader.ratios = vec![25000. / 1024.];
    reader.viewport_width = 512.;
    reader.viewport_height = 800.;
    reader.target_width = 1024;
    reader.anchor = ResumeAnchor::new("page-0", 0, 0.5).unwrap();
    let (app, receiver) = fixture_app(&ui, dir.path(), reader, true);
    app.mount_pages(&ui);
    app.restore_position(&ui);
    let owner = Arc::downgrade(&app);
    let weak = ui.as_weak();
    let worker = std::thread::spawn(move || Controller::image_worker(owner, receiver, weak));
    app.request_images(&ui, 0);
    pump_reader_until(&app, |r| r.retained.contains(&0) && r.requested.is_empty());
    let initial = app.view().reader.as_ref().unwrap().decoded_windows[&0];
    assert!(initial.first_row > 0 && initial.first_row + initial.rows < initial.full_height);
    app.position(&ui, 0., 512., 800.);
    let reverse_anchor = app.view().reader.as_ref().unwrap().anchor.clone();
    assert!(
        (reverse_anchor.offset() * f64::from(initial.full_height) - f64::from(initial.first_row))
            .abs()
            < 0.5
    );
    assert_eq!(
        app.view().reader.as_ref().unwrap().decoded_windows[&0],
        initial,
        "replacement publication remains held by this test"
    );
    assert!(app.view().reader.as_ref().unwrap().has_visible_pixels(0));
    pump_reader_until(&app, move |r| {
        r.decoded_windows[&0].first_row < initial.first_row && r.requested.is_empty()
    });
    assert_eq!(
        app.view().reader.as_ref().unwrap().anchor,
        reverse_anchor,
        "reverse overshoot is not replayed on completion"
    );
    let reverse = app.view().reader.as_ref().unwrap().decoded_windows[&0];
    app.position(&ui, 90000., 512., 800.);
    let forward_anchor = app.view().reader.as_ref().unwrap().anchor.clone();
    let forward_top = f64::from(reverse.first_row + reverse.rows) / f64::from(reverse.full_height)
        * 12500.
        - 800.;
    assert!((forward_anchor.offset() * 12500. - forward_top).abs() < 0.3);
    assert!(app.view().reader.as_ref().unwrap().has_visible_pixels(0));
    pump_reader_until(&app, move |r| {
        r.decoded_windows[&0].first_row + r.decoded_windows[&0].rows
            > reverse.first_row + reverse.rows
            && r.requested.is_empty()
    });
    assert_eq!(
        app.view().reader.as_ref().unwrap().anchor,
        forward_anchor,
        "forward overshoot is not replayed on completion"
    );
    let row = ui.get_pages().row_data(0).unwrap();
    for tile in row.tiles.iter() {
        let pixels = tile.image.to_rgba8().unwrap();
        let source_y = (tile.offset * 25000.).round() as u32;
        let pixel = pixels.as_slice()[123];
        assert_eq!(
            [pixel.r, pixel.g, pixel.b, pixel.a],
            [123, (source_y % 251) as u8, 97, 255]
        );
    }
    app.reader_seek(&ui, 0.9);
    let explicit = app.view().reader.as_ref().unwrap().anchor.clone();
    assert!((explicit.offset() - 0.9).abs() < 0.000001);
    app.position(&ui, explicit.offset() as f32 * 12500. - 100., 512., 800.);
    assert_eq!(
        app.view().reader.as_ref().unwrap().anchor,
        explicit,
        "ordinary input waits at an explicit loading anchor"
    );
    pump_reader_until(&app, |r| r.has_visible_pixels(0) && r.requested.is_empty());
    app.image_worker_live.store(false, Ordering::Release);
    app.invalidate();
    worker.join().unwrap();
}

#[test]
fn live_zero_reserve_publishes_terminal_limit_error_without_repeated_work() {
    const CHILD: &str = "READER_LIVE_ZERO_RESERVE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app::reader_handoff_tests::live_zero_reserve_publishes_terminal_limit_error_without_repeated_work", "--nocapture"])
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
    ui.set_reading_mode(0);
    ui.set_reader_side_padding(0);
    ui.global::<crate::ReaderSettings>()
        .set_desktop_webtoon_width(1600);
    ui.set_zoom(1.28);
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 2. });
    ui.window().set_size(slint::PhysicalSize::new(4096, 2048));
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(2048., 1024.),
        });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("whole.png");
    image::RgbaImage::from_pixel(4096, 2048, image::Rgba([77, 34, 91, 255]))
        .save(&path)
        .unwrap();
    let mut reader = super::tests::tall_reader();
    reader.pages = (0..2)
        .map(|i| PageInput::Local {
            path: path.clone(),
            id: format!("page-{i}"),
        })
        .collect();
    reader.ratios = vec![0.5; 2];
    reader.viewport_width = 2048.;
    reader.viewport_height = 1024.;
    reader.target_width = 4096;
    reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
    let (app, receiver) = fixture_app(&ui, dir.path(), reader, true);
    app.mount_pages(&ui);
    app.restore_position(&ui);
    let owner = Arc::downgrade(&app);
    let weak = ui.as_weak();
    let worker = std::thread::spawn(move || Controller::image_worker(owner, receiver, weak));
    app.request_images(&ui, 1);
    pump_reader_until(&app, |r| {
        r.retained.contains(&1) && r.budget_blocked.contains(&0)
    });
    assert_eq!(
        app.view().reader.as_ref().unwrap().decoded_bytes[&1],
        READER_MODEL_BYTES
    );
    app.position(&ui, 1000., 2048., 1024.);
    pump_reader_until(&app, |r| r.failed.contains(&0) && r.requested.is_empty());
    assert!(!ui.get_pages().row_data(0).unwrap().error.is_empty());
    assert!(app.view().reader.as_ref().unwrap().has_visible_pixels(1));
    let before = app.view().reader.as_ref().unwrap().decoded_windows[&1];
    for _ in 0..5 {
        app.request_images(&ui, 1);
    }
    assert!(app.view().reader.as_ref().unwrap().requested.is_empty());
    assert_eq!(
        app.view().reader.as_ref().unwrap().decoded_windows[&1],
        before
    );
    app.image_worker_live.store(false, Ordering::Release);
    app.invalidate();
    worker.join().unwrap();
}

#[test]
fn live_chapter_buttons_use_previous_tail_and_next_start_without_reader_teardown() {
    const CHILD: &str = "READER_LIVE_CHAPTER_ENTRY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app::reader_handoff_tests::live_chapter_buttons_use_previous_tail_and_next_start_without_reader_teardown", "--nocapture"])
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
    ui.set_reading_mode(0);
    ui.set_reader_side_padding(0);
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 2. });
    ui.window().set_size(slint::PhysicalSize::new(1024, 1600));
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(512., 800.),
        });
    let dir = tempfile::tempdir().unwrap();
    let mut reader = super::tests::tall_reader();
    reader.pages.clear();
    reader.ratios = vec![6.; 8];
    for index in 0..8 {
        let path = dir.path().join(format!("page-{index}.png"));
        image::RgbaImage::from_pixel(1024, 6144, image::Rgba([index as u8, 62, 174, 255]))
            .save(&path)
            .unwrap();
        reader.pages.push(PageInput::Local {
            path,
            id: format!("page-{index}"),
        });
    }
    let series = reader.chapter.series.clone();
    let chapters = (0..3)
        .map(|i| Chapter {
            id: ChapterId::new(series.clone(), format!("chapter-{i}")),
            title: i.to_string(),
            number: Some(i.to_string()),
            source_order: i,
            label: String::new(),
            volume: None,
            uploaded_at: None,
            language: None,
            scanlator: None,
        })
        .collect::<Vec<_>>();
    reader.segments = [(0, 3), (3, 5), (5, 8)]
        .into_iter()
        .enumerate()
        .map(|(i, (start, end))| reader_continuity::ChapterSegment {
            chapter: chapters[i].id.clone(),
            title: i.to_string(),
            start,
            end,
            source: None,
            completed: false,
        })
        .collect();
    reader.chapter = chapters[1].id.clone();
    reader.anchor = ResumeAnchor::new("page-3", 3, 0.4).unwrap();
    reader.viewport_width = 512.;
    reader.viewport_height = 800.;
    reader.target_width = 1024;
    let (app, receiver) = fixture_app(&ui, dir.path(), reader, true);
    app.view().chapters = chapters.clone();
    app.mount_pages(&ui);
    app.restore_position(&ui);
    let model = ui.get_pages();
    let generation = app.generation.load(Ordering::Acquire);
    let owner = Arc::downgrade(&app);
    let weak = ui.as_weak();
    let worker = std::thread::spawn(move || Controller::image_worker(owner, receiver, weak));
    app.request_images(&ui, 3);
    pump_reader_until(&app, |r| r.retained.contains(&3) && r.requested.is_empty());
    app.step_chapter(&ui, -1);
    assert_eq!(
        app.view().reader.as_ref().unwrap().chapter,
        chapters[1].id,
        "pending entry preserves old live anchor"
    );
    let previous = chapters[0].id.clone();
    pump_reader_until(&app, move |r| {
        r.chapter == previous && r.chapter_navigation.is_none()
    });
    {
        let view = app.view();
        let r = view.reader.as_ref().unwrap();
        assert_eq!(r.anchor.page_index, 2);
        assert!((r.anchor.offset() - (1. - 800. / (512. * 6.))).abs() < 0.000001);
        assert!(r.has_visible_pixels(2));
        let pixel = ui
            .get_pages()
            .row_data(2)
            .unwrap()
            .tiles
            .row_data(0)
            .unwrap()
            .image
            .to_rgba8()
            .unwrap()
            .as_slice()[0];
        assert_eq!(pixel.r, 2, "previous uses last-image bytes");
    }
    app.step_chapter(&ui, 1);
    let current = chapters[1].id.clone();
    pump_reader_until(&app, move |r| {
        r.chapter == current && r.chapter_navigation.is_none()
    });
    assert_eq!(app.view().reader.as_ref().unwrap().anchor.page_index, 3);
    assert_eq!(app.view().reader.as_ref().unwrap().anchor.offset(), 0.);
    app.step_chapter(&ui, 1);
    let next = chapters[2].id.clone();
    pump_reader_until(&app, move |r| {
        r.chapter == next && r.chapter_navigation.is_none()
    });
    assert_eq!(app.view().reader.as_ref().unwrap().anchor.page_index, 5);
    assert_eq!(app.view().reader.as_ref().unwrap().anchor.offset(), 0.);
    assert!(ui.get_pages() == model, "buttons keep the live model");
    assert_eq!(app.generation.load(Ordering::Acquire), generation);
    app.image_worker_live.store(false, Ordering::Release);
    app.invalidate();
    worker.join().unwrap();
}

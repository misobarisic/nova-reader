//! Event-loop adapter for the source-anchored capture leaf.
use super::*;
use reader_capture::selection::{Revision, Selection, Ticket};

const CAPTURE_MEMORY: u64 = 128 * 1024 * 1024;
const PREPARED_BYTES: usize = 32 * 1024 * 1024;
#[derive(Default)]
pub(super) struct State {
    frozen: Option<(u64, Result<ChapterScreencap>)>,
    active: Option<Editor>,
    revision: Revision,
}
struct Editor {
    generation: u64,
    index: i32,
    capture: ChapterScreencap,
    selection: Selection,
}
pub(super) type Shared = Arc<Mutex<State>>;
// Obsolete requests may finish a native decode before observing cancellation.
// Serialize preparation and composition to retain one shared memory allowance.
static RENDER: Mutex<()> = Mutex::new(());
fn capture_selection(capture: &ChapterScreencap) -> Result<Selection> {
    if capture.paged_fit.is_some() {
        Selection::new_with_viewport(capture.viewport, &capture.pages)
    } else {
        Selection::new(capture.viewport, &capture.pages)
    }
}

pub(super) fn freeze(state: &Shared, app: &Controller, ui: &AppWindow, index: i32) {
    let frozen = (
        app.generation.load(Ordering::Acquire),
        capture_chapter_request(app, ui, index, true),
    );
    state.lock().unwrap_or_else(|e| e.into_inner()).frozen = Some(frozen);
}
pub(super) fn clear_frozen(state: &Shared) {
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .frozen
        .take();
}
pub(super) fn open(state: &Shared, app: &Arc<Controller>, ui: &AppWindow, copy: bool) {
    let frozen = state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .frozen
        .take();
    let result = (|| {
        let (generation, capture) =
            frozen.ok_or_else(|| text::tr("Reader screencap is unavailable"))?;
        if app.generation.load(Ordering::Acquire) != generation {
            return Err(text::tr("Image action cancelled"));
        }
        let capture = capture?;
        let mut selection = capture_selection(&capture)?;
        selection.reveal_context(capture.viewport.height * 0.2);
        let index = capture.reference as i32;
        state.lock().unwrap_or_else(|e| e.into_inner()).active = Some(Editor {
            generation,
            index,
            capture,
            selection,
        });
        ui.set_capture_editor_copy(copy);
        ui.set_capture_editor_error("".into());
        ui.set_capture_editor_preview_mode(false);
        ui.set_capture_editor_image(slint::Image::default());
        ui.set_capture_editor_visible(true);
        refresh(state, app, ui, false, None);
        Ok(())
    })();
    if let Err(message) = result {
        ui.set_reader_context_status(text::tr(&message).into());
    }
}
fn project(state: &Shared, ui: &AppWindow) {
    let projection = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.active.as_ref().map(|editor| {
            (
                editor.selection.normalized(),
                editor.selection.can_extend(true),
                editor.selection.can_extend(false),
            )
        })
    };
    if let Some((rect, above, below)) = projection {
        ui.set_capture_editor_crop(crate::CaptureCropRect {
            x: rect.x as f32,
            y: rect.y as f32,
            width: rect.width as f32,
            height: rect.height as f32,
        });
        ui.set_capture_editor_can_extend_above(above);
        ui.set_capture_editor_can_extend_below(below);
    }
}
fn check(cancel: &CancelToken, ticket: Option<&Ticket>) -> Result<()> {
    cancel.check().map_err(error)?;
    if ticket.is_some_and(|ticket| !ticket.current()) {
        return Err(text::tr("Image action cancelled"));
    }
    Ok(())
}
/// Resolve only pages about to enter the canvas. Placeholder rectangles are
/// navigation hints, never a destination into which original artwork is fit.
fn prepare(
    capture: &mut ChapterScreencap,
    selection: &mut Selection,
    extension: Option<bool>,
    cancel: &CancelToken,
    ticket: Option<&Ticket>,
) -> Result<BTreeMap<usize, Arc<Vec<u8>>>> {
    let mut encoded = BTreeMap::new();
    let mut encoded_bytes = 0usize;
    loop {
        check(cancel, ticket)?;
        let mut proposed = selection.clone();
        if let Some(above) = extension {
            proposed.extend(above);
        }
        let incoming = capture
            .pages
            .iter()
            .find(|page| {
                !capture.known_dimensions.contains(&page.index)
                    && page.rect.intersection(proposed.canvas).is_some()
            })
            .map(|page| page.index);
        let Some(index) = incoming else {
            *selection = proposed;
            return Ok(encoded);
        };
        let (input, source) = capture
            .inputs
            .get(&index)
            .ok_or_else(|| text::tr("Chapter image is unavailable"))?;
        let bytes = capture
            .originals
            .read(index, input, source.as_ref(), cancel)?;
        encoded_bytes = encoded_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= PREPARED_BYTES)
            .ok_or_else(|| {
                text::tr("Incoming capture pages exceed the memory limit. Extend a smaller area.")
            })?;
        check(cancel, ticket)?;
        let limits = reader_engine::DecodeLimits {
            max_decoded_bytes: (CAPTURE_MEMORY
                - encoded_bytes as u64
                - capture.originals.reservation())
            .min(reader_engine::current_limits().max_decoded_bytes),
            ..reader_engine::current_limits()
        };
        let (effective, split) = if capture.options.crop_borders
            || capture.options.split_wide
            || capture.options.rotate_wide
        {
            let image = reader_engine::decode_image(&bytes, limits).map_err(error)?;
            check(cancel, ticket)?;
            let presentation =
                reader_engine::page_presentation(&image, capture.options).map_err(error)?;
            (presentation.effective, presentation.split)
        } else {
            (
                reader_engine::inspect_image(&bytes, limits).map_err(error)?,
                false,
            )
        };
        check(cancel, ticket)?;
        let previous = capture.pages.clone();
        let page = capture
            .pages
            .iter_mut()
            .find(|page| page.index == index)
            .ok_or_else(|| text::tr("Capture geometry unavailable"))?;
        let ratio = f64::from(effective.height) / f64::from(effective.width);
        page.rect.height = page.rect.width * ratio;
        if let Some(fit) = &mut capture.paged_fit {
            if split {
                fit.split_pages.insert(index);
            } else {
                fit.split_pages.remove(&index);
            }
            layout_paged_capture(&mut capture.pages, capture.layout_anchor, fit)?;
        } else {
            let anchor = capture
                .pages
                .iter()
                .position(|page| page.index == capture.layout_anchor)
                .ok_or_else(|| text::tr("Capture geometry unavailable"))?;
            for i in (0..anchor).rev() {
                capture.pages[i].rect.y = capture.pages[i + 1].rect.y
                    - capture.layout_gaps[i]
                    - capture.pages[i].rect.height;
            }
            for i in anchor + 1..capture.pages.len() {
                capture.pages[i].rect.y =
                    capture.pages[i - 1].rect.bottom() + capture.layout_gaps[i - 1];
            }
        }
        selection.reanchor_known(&previous, &capture.pages, |index| {
            capture.known_dimensions.contains(&index)
        })?;
        capture.known_dimensions.insert(index);
        encoded.insert(index, bytes);
    }
}
fn refresh(
    state: &Shared,
    app: &Arc<Controller>,
    ui: &AppWindow,
    preview: bool,
    extension: Option<bool>,
) {
    project(state, ui);
    let (mut capture, mut selection, generation, ticket) = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(editor) = &state.active else { return };
        (
            editor.capture.clone(),
            editor.selection.clone(),
            editor.generation,
            state.revision.advance(),
        )
    };
    ui.set_capture_editor_busy(true);
    ui.set_capture_editor_error("".into());
    let weak = ui.as_weak();
    let app = app.clone();
    let state = state.clone();
    let cancel = app.cancel.lock().unwrap_or_else(|e| e.into_inner()).clone();
    std::thread::spawn(move || {
        let result = reader_engine::with_current_limits(|| {
            let _render = RENDER.lock().unwrap_or_else(|e| e.into_inner());
            let originals = capture.originals.clone();
            let initial_capture = capture.clone();
            let initial_selection = selection.clone();
            let render = || -> Result<_> {
                capture = initial_capture.clone();
                selection = initial_selection.clone();
                let encoded = prepare(
                    &mut capture,
                    &mut selection,
                    extension,
                    &cancel,
                    Some(&ticket),
                )?;
                capture.viewport = if preview {
                    selection.crop
                } else {
                    selection.canvas
                };
                capture.reference = selection
                    .first_page(&capture.pages, preview)
                    .ok_or_else(|| text::tr("No artwork in the selected area."))?;
                // Preview dimensions stay independent of native export scale and
                // never require a large GPU texture or full chapter bitmap.
                let scale = (1_000_000. / (capture.viewport.width * capture.viewport.height))
                    .sqrt()
                    .min(1200. / capture.viewport.width)
                    .min(2048. / capture.viewport.height)
                    .min(capture.device_scale);
                capture.output_scale = Some(scale);
                render_prepared(&capture, &cancel, Some(&ticket), encoded)
            };
            with_memory_fallback(&originals, render)
        });
        if !ticket.current() {
            return;
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if !ticket.current()
                || app.generation.load(Ordering::Acquire) != generation
                || ui.get_section() != 6
                || !ui.get_capture_editor_visible()
            {
                return;
            }
            ui.set_capture_editor_busy(false);
            match result {
                Ok((image, _)) => {
                    {
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(editor) = &mut state.active {
                            editor.capture = capture;
                            editor.selection = selection;
                        }
                    }
                    project(&state, &ui);
                    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &image.pixels,
                        image.width,
                        image.height,
                    );
                    ui.set_capture_editor_image(slint::Image::from_rgba8(buffer));
                    ui.set_capture_editor_preview_mode(preview);
                }
                Err(message) => ui.set_capture_editor_error(text::tr(&message).into()),
            }
        });
    });
}
pub(super) fn stale(state: &Shared, app: &Controller, ui: &AppWindow) {
    let stale = state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .active
        .as_ref()
        .is_some_and(|editor| {
            editor.generation != app.generation.load(Ordering::Acquire) || ui.get_section() != 6
        });
    if stale {
        cancel(state, ui);
    }
}
fn cancel(state: &Shared, ui: &AppWindow) {
    {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.revision.advance();
        state.active.take();
    }
    ui.set_capture_editor_visible(false);
    ui.set_capture_editor_image(slint::Image::default());
    ui.set_capture_editor_busy(false);
}
pub(super) fn export(state: &Shared, ui: &AppWindow) -> Option<(u64, ChapterScreencap, Ticket)> {
    let state = state.lock().unwrap_or_else(|e| e.into_inner());
    let editor = state.active.as_ref()?;
    let mut capture = editor.capture.clone();
    capture.viewport = editor.selection.crop;
    capture.reference = editor.selection.first_page(&capture.pages, true)?;
    capture.output_scale = if crate::capture_settings::preferences(ui).resolution
        == crate::capture_settings::CaptureResolution::Device
    {
        Some(capture.device_scale)
    } else {
        None
    };
    Some((editor.generation, capture, state.revision.advance()))
}
pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) -> Shared {
    let state = Arc::new(Mutex::new(State::default()));
    ui.on_capture_editor_selection_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |rect| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(editor) = &mut state.active {
                    editor
                        .selection
                        .set_normalized(crate::chapter_capture::Rect {
                            x: f64::from(rect.x),
                            y: f64::from(rect.y),
                            width: f64::from(rect.width),
                            height: f64::from(rect.height),
                        });
                }
                state.revision.advance();
            }
            project(&state, &ui);
        }
    });
    ui.on_capture_editor_action({
        let state = state.clone();
        let app = app.clone();
        let weak = ui.as_weak();
        move |action| {
            let Some(ui) = weak.upgrade() else { return };
            match action.as_str() {
                "cancel" => cancel(&state, &ui),
                "extend-above" | "extend-below" => {
                    refresh(&state, &app, &ui, false, Some(action == "extend-above"))
                }
                "preview" => refresh(&state, &app, &ui, true, None),
                "adjust" => refresh(&state, &app, &ui, false, None),
                "confirm" => {
                    let index = state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .active
                        .as_ref()
                        .map(|editor| editor.index);
                    if let Some(index) = index {
                        ui.invoke_reader_context_action(
                            index,
                            if ui.get_capture_editor_copy() {
                                "copy-screencap-selection"
                            } else {
                                "save-screencap-selection"
                            }
                            .into(),
                        );
                    }
                }
                _ => {}
            }
        }
    });
    state
}
pub(super) fn render_capture(
    capture: &ChapterScreencap,
    cancel: &CancelToken,
    ticket: Option<&Ticket>,
) -> Result<(ReaderScreencap, FrozenExportName)> {
    reader_engine::with_current_limits(|| {
        let _render = RENDER.lock().unwrap_or_else(|e| e.into_inner());
        let mut capture = capture.clone();
        let mut selection = capture_selection(&capture)?;
        with_memory_fallback(&capture.originals.clone(), || {
            let encoded = prepare(&mut capture, &mut selection, None, cancel, ticket)?;
            // Native export remains independent of preview's downsampled artwork.
            render_prepared(&capture, cancel, ticket, encoded)
        })
    })
}
fn with_memory_fallback<T>(
    originals: &reader_capture_sources::Session,
    mut render: impl FnMut() -> Result<T>,
) -> Result<T> {
    match render() {
        Err(message)
            if originals.reservation() > 0
                && (message.contains("capture admission")
                    || message.contains("memory limits")
                    || message.contains("decoded image")
                    || message.contains("decode limit")
                    || message.contains("exceeds supported limits")) =>
        {
            originals.release();
            render()
        }
        result => result,
    }
}
fn render_prepared(
    capture: &ChapterScreencap,
    cancel: &CancelToken,
    ticket: Option<&Ticket>,
    mut encoded: BTreeMap<usize, Arc<Vec<u8>>>,
) -> Result<(ReaderScreencap, FrozenExportName)> {
    check(cancel, ticket)?;
    let reserved = encoded
        .values()
        .map(|bytes| bytes.len() as u64)
        .sum::<u64>();
    let mut rendered = crate::chapter_capture::capture_at_scale_with_memory(
        capture.viewport,
        capture.reference,
        &capture.pages,
        capture.background,
        capture.options,
        capture.output_scale,
        CAPTURE_MEMORY - reserved - capture.originals.reservation(),
        |index| {
            check(cancel, ticket)?;
            if let Some(bytes) = encoded.remove(&index) {
                return Ok(reader_capture_sources::Bytes(bytes));
            }
            let (input, source) = capture
                .inputs
                .get(&index)
                .ok_or_else(|| text::tr("Chapter image is unavailable"))?;
            capture
                .originals
                .read(index, input, source.as_ref(), cancel)
                .map(reader_capture_sources::Bytes)
        },
    )?;
    check(cancel, ticket)?;
    if capture.brightness < 100 {
        for pixel in rendered.pixels.as_chunks_mut::<4>().0 {
            for channel in &mut pixel[..3] {
                *channel = (u16::from(*channel) * u16::from(capture.brightness) / 100) as u8;
            }
        }
    }
    let start = rendered
        .source_start
        .ok_or_else(|| text::tr("No artwork in the selected area."))?;
    let (chapter, index) = capture
        .naming
        .get(&start.index)
        .ok_or_else(|| text::tr("Chapter image is unavailable"))?;
    let naming = FrozenExportName {
        chapter: chapter.clone(),
        page: *index,
        source_y: Some(start.y),
    };
    Ok((
        ReaderScreencap {
            width: rendered.width,
            height: rendered.height,
            pixels: rendered.pixels,
        },
        naming,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advanced_actions_freeze_placeholder_geometry_and_cancel_before_delivery() {
        const CHILD: &str = "READER_CAPTURE_EDITOR_BACKEND_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","app::reader_capture_editor::tests::advanced_actions_freeze_placeholder_geometry_and_cancel_before_delivery","--nocapture"])
                .env(CHILD,"1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
            i_slint_backend_testing::TestingBackendOptions {
                mock_time: true,
                renderer_name: Some("software".into()),
                ..Default::default()
            },
        )))
        .unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(100., 100.));
        ui.set_section(6);
        // Use a real fit-width zoom. The source fixture below is expressed in
        // a 100px chapter column; magnification now scales the fitted column
        // directly rather than relying on the old post-zoom 100px floor.
        ui.set_zoom(1.);
        ui.set_reading_mode(0);
        assert_eq!(ui.get_reader_width(), 100.);
        ui.set_reader_capture_viewport(crate::ReaderCaptureViewport {
            x: 0.,
            y: 0.,
            width: 100.,
            height: 100.,
            content_x: 0.,
            content_y: -100.,
        });
        let dir = tempfile::tempdir().unwrap();
        let mut reader = super::super::tests::tall_reader();
        reader.ratios = vec![3.; 2];
        for (index, input) in reader.pages.iter_mut().enumerate() {
            let path = dir.path().join(format!("page-{index}.png"));
            image::RgbaImage::from_fn(10, 30, |x, y| {
                image::Rgba([index as u8 * 100, x as u8, y as u8, 255])
            })
            .save(&path)
            .unwrap();
            *input = PageInput::Local {
                path,
                id: format!("page-{index}"),
            };
        }
        let services = Services::open(&dir.path().join("data")).unwrap();
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
            image_worker_live: AtomicBool::new(true),
            view: Mutex::new(View {
                reader: Some(reader),
                section: 6,
                ..View::default()
            }),
            generation: AtomicU64::new(1),
            cancel: Mutex::new(CancelToken::default()),
            images,
            download_serial: Mutex::new(()),
            active_image: Mutex::new(None),
            metadata_refresh: Arc::default(),
        });
        ui.set_pages(ModelRc::new(VecModel::from(vec![
            PageRow {
                ratio: 3.,
                position: 0.,
                loaded: true,
                page_id: "page-0".into(),
                ..Default::default()
            },
            PageRow {
                ratio: 3.,
                position: 3.,
                loaded: false,
                page_id: "page-1".into(),
                ..Default::default()
            },
        ])));
        let mut capture = capture_chapter_request(&app, &ui, 0, true).unwrap();
        assert_eq!(
            capture.pages.len(),
            2,
            "logical placeholders belong to frozen chapter geometry"
        );
        let selection = Selection::new(capture.viewport, &capture.pages).unwrap();
        assert!(selection.can_extend(true) && selection.can_extend(false));
        capture.viewport = crate::chapter_capture::Rect {
            x: 20.,
            y: 250.,
            width: 60.,
            height: 100.,
        };
        capture.reference = 1;
        capture.output_scale = Some(0.1);
        let (rendered, naming) = render_capture(&capture, &CancelToken::default(), None).unwrap();
        assert_eq!((rendered.width, rendered.height), (6, 10));
        assert_eq!(naming.page, 2);
        assert_eq!(naming.source_y, Some(5));
        assert_eq!(&rendered.pixels[..4], &[100, 2, 5, 255]);
        // Deliberately wrong placeholders on either side are resolved before
        // the extension is admitted. One viewport adds 100 logical pixels,
        // independent of the incoming page's true aspect ratio.
        for above in [true, false] {
            let mut corrected =
                capture_chapter_request(&app, &ui, if above { 1 } else { 0 }, true).unwrap();
            corrected.known_dimensions.insert(if above { 1 } else { 0 });
            let placeholder = if above { 0 } else { 1 };
            corrected.pages[placeholder].rect.height = 50.;
            if above {
                corrected.pages[1].rect.y = 200.;
                corrected.pages[0].rect.y = 150.;
                corrected.viewport = crate::chapter_capture::Rect {
                    x: 0.,
                    y: 200.,
                    width: 100.,
                    height: 100.,
                };
            } else {
                corrected.pages[0].rect.y = -200.;
                corrected.pages[1].rect.y = 100.;
                corrected.viewport = crate::chapter_capture::Rect {
                    x: 0.,
                    y: 0.,
                    width: 100.,
                    height: 100.,
                };
            }
            corrected.layout_gaps = vec![0.];
            let anchor_rect = corrected.pages[if above { 1 } else { 0 }].rect;
            let mut selected = Selection::new(corrected.viewport, &corrected.pages).unwrap();
            let prior = selected.crop;
            let ticket = Revision::default().advance();
            let encoded = prepare(
                &mut corrected,
                &mut selected,
                Some(above),
                &CancelToken::default(),
                Some(&ticket),
            )
            .unwrap();
            assert_eq!(corrected.pages[placeholder].rect.height, 300.);
            assert_eq!(corrected.pages[if above { 1 } else { 0 }].rect, anchor_rect);
            assert_eq!(selected.crop.height, prior.height + 100.);
            assert_eq!(
                encoded.len(),
                1,
                "incoming original fetched once and reused for composition"
            );
            corrected.viewport = selected.crop;
            corrected.reference = selected.first_page(&corrected.pages, true).unwrap();
            corrected.output_scale = Some(0.1);
            let (image, _) =
                render_prepared(&corrected, &CancelToken::default(), Some(&ticket), encoded)
                    .unwrap();
            assert_eq!((image.width, image.height), (10, 20));
            if above {
                assert_eq!(&image.pixels[..4], &[0, 0, 20, 255]);
            } else {
                assert_eq!(
                    &image.pixels[10 * 10 * 4..10 * 10 * 4 + 4],
                    &[100, 0, 0, 255]
                );
            }
        }
        // The editor uses the frozen request even if the live model reflows.
        ui.set_zoom(8.);
        assert_eq!(capture.pages[1].rect.width, 100.);
        ui.set_zoom(1.);
        assert_eq!(ui.get_reader_width(), 100.);
        let state = wire(&app, &ui);
        for copy in [false, true] {
            freeze(&state, &app, &ui, 0);
            open(&state, &app, &ui, copy);
            assert!(ui.get_capture_editor_visible());
            assert_eq!(ui.get_capture_editor_copy(), copy);
            let (_, selected, ticket) = export(&state, &ui).unwrap();
            assert_eq!(selected.viewport, selection.crop);
            ui.invoke_capture_editor_action("cancel".into());
            assert!(!ticket.current());
            assert!(!ui.get_capture_editor_visible());
            assert!(state.lock().unwrap().active.is_none());
        }
        // The actual menu wiring routes both ordinary screencap actions into
        // editing, even if the optional device bitmap is unavailable headless.
        wire_reader_context(&app, &ui);
        for action in ["save-screencap", "copy-screencap"] {
            ui.invoke_reader_context_opening(0);
            ui.invoke_reader_context_action(0, action.into());
            assert!(ui.get_capture_editor_visible());
            ui.invoke_capture_editor_action("cancel".into());
        }
        paged_letterbox_matches_native_frame(&app, &ui, dir.path());
        freeze(&state, &app, &ui, 0);
        app.invalidate();
        open(&state, &app, &ui, false);
        assert!(!ui.get_capture_editor_visible());
        // Neither preview nor Cancel enters any platform publication function.
        assert!(!dir.path().join("image-export").exists());
    }

    fn paged_letterbox_matches_native_frame(
        app: &Controller,
        ui: &AppWindow,
        directory: &std::path::Path,
    ) {
        let colors = [[210, 30, 50, 255], [30, 80, 220, 255]];
        let mut rows = Vec::new();
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.ratios = vec![0.5; 2];
            reader.presentations.clear();
            for (index, color) in colors.iter().enumerate() {
                let path = directory.join(format!("letterbox-{index}.png"));
                let image = image::RgbaImage::from_pixel(10, 5, image::Rgba(*color));
                image.save(&path).unwrap();
                reader.pages[index] = PageInput::Local {
                    path,
                    id: format!("letterbox-{index}"),
                };
                rows.push(PageRow {
                    page_id: format!("letterbox-{index}").into(),
                    ratio: 0.5,
                    loaded: true,
                    position: index as f32 * 0.5,
                    tiles: ModelRc::new(VecModel::from(vec![crate::TileRow {
                        image: slint::Image::from_rgba8(
                            slint::SharedPixelBuffer::clone_from_slice(image.as_raw(), 10, 5),
                        ),
                        fraction: 1.,
                        ..Default::default()
                    }])),
                    ..Default::default()
                });
            }
        }
        ui.global::<crate::Anim>().set_enabled(false);
        ui.set_zoom(1.);
        ui.set_reader_osd(false);
        ui.set_reader_background(slint::Color::from_rgb_u8(0, 0, 0));
        ui.show().unwrap();
        ui.window().set_size(slint::LogicalSize::new(320., 240.));
        let advance = || {
            for _ in 0..32 {
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
            }
        };
        for mode in [1, 2] {
            ui.set_reading_mode(mode);
            app.view().reader.as_mut().unwrap().mode = if mode == 1 {
                ReadingMode::LeftToRight
            } else {
                ReadingMode::RightToLeft
            };
            for reference in 0..2 {
                ui.set_current_page(reference);
                for known in [true, false] {
                    {
                        let mut view = app.view();
                        let reader = view.reader.as_mut().unwrap();
                        reader.native_dimensions.clear();
                        if known {
                            for index in 0..2 {
                                reader.native_dimensions.insert(
                                    index,
                                    reader_engine::PageDimensions {
                                        width: 10,
                                        height: 5,
                                    },
                                );
                            }
                        }
                    }
                    for advanced in [false, true] {
                        let mut provisional = rows.clone();
                        if !known {
                            for row in &mut provisional {
                                row.ratio = 3.;
                            }
                        }
                        ui.set_pages(ModelRc::new(VecModel::from(provisional)));
                        advance();
                        ui.set_reader_capture_viewport(crate::ReaderCaptureViewport {
                            x: 0.,
                            y: 0.,
                            width: 320.,
                            height: 240.,
                            content_x: 0.,
                            content_y: 0.,
                        });
                        let mut capture =
                            capture_chapter_request(app, ui, reference, advanced).unwrap();
                        capture.output_scale = Some(1.);
                        // Resolve provisional original dimensions against the
                        // same actual contained geometry the reader paints.
                        ui.set_pages(ModelRc::new(VecModel::from(rows.clone())));
                        advance();
                        let native = capture_reader(ui).unwrap();
                        let (rendered, naming) =
                            render_capture(&capture, &CancelToken::default(), None).unwrap();
                        assert_eq!((rendered.width, rendered.height), (320, 240));
                        assert_eq!(naming.page, reference as usize + 1);
                        for (x, y) in [
                            (160, 0),
                            (160, 39),
                            (160, 40),
                            (160, 120),
                            (160, 199),
                            (160, 200),
                            (160, 239),
                        ] {
                            let offset = (y * 320 + x) * 4;
                            assert_eq!(
                                &rendered.pixels[offset..offset + 4],
                                &native.pixels[offset..offset + 4],
                                "mode={mode} reference={reference} known={known} advanced={advanced} at({x},{y})"
                            );
                            let expected = if (40..200).contains(&y) {
                                colors[reference as usize]
                            } else {
                                [0, 0, 0, 255]
                            };
                            assert_eq!(&rendered.pixels[offset..offset + 4], &expected);
                        }
                        let mut resolved = capture.clone();
                        let mut selection = capture_selection(&resolved).unwrap();
                        prepare(
                            &mut resolved,
                            &mut selection,
                            None,
                            &CancelToken::default(),
                            None,
                        )
                        .unwrap();
                        assert_eq!(
                            selection.crop, capture.viewport,
                            "advanced default keeps the full letterbox"
                        );
                        assert_eq!(
                            resolved
                                .pages
                                .iter()
                                .find(|page| page.index == reference as usize)
                                .unwrap()
                                .rect
                                .y,
                            40.
                        );
                        if advanced {
                            let mut contextual = capture.clone();
                            let mut selected = capture_selection(&contextual).unwrap();
                            selected.reveal_context(contextual.viewport.height * 0.2);
                            let originals = prepare(
                                &mut contextual,
                                &mut selected,
                                None,
                                &CancelToken::default(),
                                None,
                            )
                            .unwrap();
                            assert_eq!(
                                selected.crop, capture.viewport,
                                "context never enters the default export"
                            );
                            contextual.viewport = selected.canvas;
                            let (context_image, _) = render_prepared(
                                &contextual,
                                &CancelToken::default(),
                                None,
                                originals,
                            )
                            .unwrap();
                            let row = if reference == 0 {
                                context_image.height as usize - 4
                            } else {
                                4
                            };
                            let offset = (row * context_image.width as usize + 160) * 4;
                            assert_eq!(
                                &context_image.pixels[offset..offset + 4],
                                &colors[1 - reference as usize],
                                "the editor context reveals real adjacent source pixels"
                            );
                        }
                    }
                }
            }
        }
    }
}

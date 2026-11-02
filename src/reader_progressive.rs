//! Provisional JPEG XL pixels. Only the ordinary strict completion owns loaded
//! state, retention, checkpoints and cache publication.
use super::*;
use std::time::{Duration, Instant};

// At most a resident, pending and conversion copy (plus the renderer's previous
// frame) fit in the separate 8 MiB provisional allowance. No encoded body lives
// here: Source owns the sole complete response.
const FRAME_BYTES: usize = 2 * 1024 * 1024;
const INTERVAL: Duration = Duration::from_millis(200);
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

fn identity(options: reader_engine::DisplayOptions) -> bool {
    !options.crop_borders
        && !options.split_wide
        && !options.rotate_wide
        && !options.reverse_page
        && !options.grayscale
        && !options.invert_colors
        && !(options.color_filter && options.filter_intensity > 0)
}
struct Frame {
    source: reader_engine::PageDimensions,
    tiles: Vec<PreparedTile>,
    bytes: usize,
}
#[derive(Default)]
struct Pending {
    frame: Option<Frame>,
    queued: bool,
    stopped: bool,
}
struct Mailbox {
    pending: Mutex<Pending>,
    app: Weak<Controller>,
    ui: slint::Weak<AppWindow>,
    generation: u64,
    index: usize,
    job: u64,
    page_id: String,
    options: reader_engine::DisplayOptions,
    cancel: CancelToken,
    request: crate::chapter_cache::Request,
}
impl Mailbox {
    fn current(&self) -> bool {
        if self.cancel.is_cancelled() || self.request.validate_active().is_err() {
            return false;
        }
        self.app.upgrade().is_some_and(|owner| {
            if !owner.image_worker_live.load(Ordering::Acquire)
                || owner.generation.load(Ordering::Acquire) != self.generation
            {
                return false;
            }
            let view = owner.view();
            view.section == 6
                && view.reader.as_ref().is_some_and(|reader| {
                    reader.anchor.page_index == self.index
                        && reader.display_options == self.options
                        && reader.pages.get(self.index).is_some()
                        && reader.row_page_id(self.index) == self.page_id
                        && reader.desired.contains(&self.index)
                })
        })
    }
    fn busy(&self) -> bool {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.queued || pending.stopped
    }
    fn publish(self: &Arc<Self>, frame: Frame) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if pending.stopped || pending.queued {
                return;
            }
            pending.frame = Some(frame);
            pending.queued = true;
        }
        let mailbox = self.clone();
        if self
            .ui
            .upgrade_in_event_loop(move |ui| {
                let frame = {
                    let mut pending = mailbox.pending.lock().unwrap_or_else(|e| e.into_inner());
                    if pending.stopped {
                        pending.frame.take();
                        None
                    } else {
                        pending.frame.take()
                    }
                };
                if let Some(frame) = frame
                    && mailbox.current()
                    && mailbox.request.validate_scope().is_ok()
                {
                    mailbox.apply(&ui, frame);
                }
                let mut pending = mailbox.pending.lock().unwrap_or_else(|e| e.into_inner());
                pending.queued = false;
            })
            .is_err()
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.frame.take();
            pending.queued = false;
        }
    }
    fn apply(&self, ui: &AppWindow, frame: Frame) {
        // Every job owns its mailbox. A stopped old job cannot publish into a
        // same-generation/index retry, even if its UI closure runs later.
        let pending_guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending_guard.stopped {
            return;
        }
        let Some(owner) = self.app.upgrade() else {
            return;
        };
        let mut view = owner.view();
        if owner.generation.load(Ordering::Acquire) != self.generation || self.cancel.is_cancelled()
        {
            return;
        }
        let Some(reader) = view.reader.as_mut() else {
            return;
        };
        if reader.anchor.page_index != self.index
            || reader.display_options != self.options
            || !reader.desired.contains(&self.index)
            || reader.row_page_id(self.index) != self.page_id
        {
            return;
        }
        let model = ui.get_pages();
        let Some(mut row) = model.row_data(self.index) else {
            return;
        };
        // A genuine old final window is always preferable to provisional data.
        if row.loaded
            || frame.bytes > FRAME_BYTES
            || reader
                .retained_bytes_without(self.index)
                .saturating_add(frame.bytes)
                .saturating_add(FRAME_BYTES)
                > READER_MODEL_BYTES
        {
            return;
        }
        let geometry_changed = reader.native_dimensions.get(&self.index) != Some(&frame.source);
        if geometry_changed {
            reader.native_dimensions.insert(self.index, frame.source);
            reader.ratios[self.index] = frame.source.height as f32 / frame.source.width as f32;
            for i in 0..reader.ratios.len() {
                if let Some(mut other) = model.row_data(i) {
                    other.ratio = reader.ratios[i];
                    other.position = reader.page_position(i);
                    model.set_row_data(i, other);
                }
            }
            ui.set_total_ratio(reader.total_extent());
            row = model.row_data(self.index).unwrap();
        }
        row.tiles = ModelRc::new(VecModel::from(
            frame.tiles.into_iter().map(mount_tile).collect::<Vec<_>>(),
        ));
        row.image = slint::Image::default();
        row.preview = true;
        reader.progressive_job = Some((self.index, self.job));
        row.error = "".into();
        model.set_row_data(self.index, row);
        drop(view);
        if geometry_changed {
            owner.restore_position(ui);
        }
    }
    fn stop(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.stopped = true;
        pending.frame.take();
        // A queued callback holds no pixels after frame.take(). A callback
        // already holding a frame must pass the same mutex in apply(), so a
        // stopped job cannot paint later. Do not wait for a paused UI loop.
    }
}

enum Decoder {
    Probe(Vec<u8>),
    Active(Box<nova_image::ProgressiveJxlDecoder>),
    Disabled,
}
pub(super) struct Cleanup(Arc<Mailbox>);
impl Cleanup {
    pub(super) fn clear(self) {
        let mailbox = self.0;
        let ui = mailbox.ui.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let Some(owner) = mailbox.app.upgrade() else {
                return;
            };
            let mut view = owner.view();
            let Some(reader) = view.reader.as_mut() else {
                return;
            };
            if reader.progressive_job != Some((mailbox.index, mailbox.job)) {
                return;
            }
            reader.progressive_job = None;
            let model = ui.get_pages();
            if let Some(mut row) = model.row_data(mailbox.index) {
                clear_preview(&mut row);
                model.set_row_data(mailbox.index, row);
            }
        });
    }
}
pub(super) struct Stream {
    decoder: Decoder,
    mailbox: Arc<Mailbox>,
    width: u32,
    last_render: Option<Instant>,
    render_disabled: bool,
    failure: Option<String>,
}
impl Stream {
    pub(super) fn new(
        app: &Arc<Controller>,
        ui: slint::Weak<AppWindow>,
        task: &ImageTask,
        cancel: CancelToken,
        request: crate::chapter_cache::Request,
    ) -> Option<Self> {
        let view = app.view();
        let reader = view.reader.as_ref()?;
        if reader.anchor.page_index != task.index
            || reader.retained.contains(&task.index)
            || !identity(reader.display_options)
        {
            return None;
        }
        Some(Self {
            decoder: Decoder::Probe(Vec::with_capacity(12)),
            mailbox: Arc::new(Mailbox {
                pending: Mutex::new(Pending::default()),
                app: Arc::downgrade(app),
                ui,
                generation: task.generation,
                index: task.index,
                job: NEXT_JOB.fetch_add(1, Ordering::Relaxed),
                page_id: reader.row_page_id(task.index),
                options: reader.display_options,
                cancel,
                request,
            }),
            width: reader.target_width.clamp(1, 2048),
            last_render: None,
            render_disabled: false,
            failure: None,
        })
    }
    pub(super) fn cleanup(&self) -> Cleanup {
        Cleanup(self.mailbox.clone())
    }
    pub(super) fn feed(&mut self, chunk: &[u8], before_start: impl FnOnce()) {
        if !self.mailbox.current() {
            self.mailbox.cancel.cancel();
            return;
        }
        let mut rest = chunk;
        if let Decoder::Probe(prefix) = &mut self.decoder {
            let take = (12 - prefix.len()).min(rest.len());
            prefix.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if !nova_image::is_jpeg_xl(prefix) {
                if prefix.len() >= 12 {
                    self.decoder = Decoder::Disabled;
                }
                return;
            }
            before_start();
            let options = nova_image::JxlPreviewOptions {
                max_width: self.width,
                max_height: 8192,
                max_bytes: FRAME_BYTES,
            };
            let created = nova_image::ProgressiveJxlDecoder::new(
                reader_engine::DecodeLimits::default(),
                options,
            )
            .and_then(|mut decoder| {
                decoder.feed(prefix)?;
                Ok(decoder)
            });
            match created {
                Ok(decoder) => self.decoder = Decoder::Active(Box::new(decoder)),
                Err(nova_image::Error::JxlUnsupported(_) | nova_image::Error::Limit) => {
                    self.decoder = Decoder::Disabled;
                    return;
                }
                Err(error) => {
                    self.failure = Some(error.to_string());
                    self.decoder = Decoder::Disabled;
                    return;
                }
            }
        }
        let Decoder::Active(decoder) = &mut self.decoder else {
            return;
        };
        if let Err(error) = decoder.feed(rest) {
            if !matches!(
                error,
                nova_image::Error::JxlUnsupported(_) | nova_image::Error::Limit
            ) {
                self.failure = Some(error.to_string());
            }
            self.decoder = Decoder::Disabled;
            return;
        }
        if self.render_disabled
            || self.mailbox.busy()
            || self.last_render.is_some_and(|at| at.elapsed() < INTERVAL)
        {
            return;
        }
        self.last_render = Some(Instant::now());
        match decoder.render_preview() {
            Ok(Some(preview)) => {
                let bytes = preview.image.as_raw().len();
                if bytes > FRAME_BYTES {
                    self.render_disabled = true;
                    return;
                }
                let tiles = prepare_display_tiles(
                    preview.image.as_raw(),
                    preview.image.width(),
                    preview.image.height(),
                    0,
                    preview.image.height(),
                    None,
                );
                let source = preview.source;
                drop(preview);
                self.mailbox.publish(Frame {
                    source,
                    tiles,
                    bytes,
                });
            }
            Ok(None) => {}
            Err(_) => self.render_disabled = true,
        }
    }
    /// EOF is checked without allocating a final source RGBA image. Drop the
    /// incremental native grids and drain pending preview pixels before the
    /// ordinary strict DisplayDecoder allocates its final page/window.
    pub(super) fn finish(&mut self) -> Result<()> {
        let validation = match &mut self.decoder {
            Decoder::Active(decoder) => match decoder.validate_complete() {
                Ok(()) | Err(nova_image::Error::JxlUnsupported(_) | nova_image::Error::Limit) => {
                    Ok(())
                }
                // Optional preview rendering can consume a different native
                // allocation state from a fresh final decoder. The strict full
                // path independently validates identical container EOF/framing.
                Err(_) if self.render_disabled => Ok(()),
                Err(failure) => Err(error(failure)),
            },
            _ => Ok(()),
        };
        self.decoder = Decoder::Disabled;
        self.mailbox.stop();
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        validation
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.decoder = Decoder::Disabled;
        self.mailbox.stop();
    }
}

pub(super) fn clear_preview(row: &mut PageRow) {
    if row.preview {
        row.preview = false;
        if !row.loaded {
            row.tiles = ModelRc::default();
            row.image = slint::Image::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_sources::{HttpRequest, HttpResponse, SourceTransport};
    use std::{
        cell::{Cell, RefCell},
        io::{Cursor, Write},
        rc::Rc,
        sync::atomic::AtomicUsize,
    };
    const JXL: &[u8] =
        include_bytes!("../crates/reader-engine/tests/fixtures/jxl/progressive-rgb.jxl");

    fn boxed_jxl() -> (Vec<u8>, usize) {
        let mut bytes = b"\0\0\0\x0cJXL \r\n\x87\n\0\0\0\x14ftypjxl \0\0\0\0jxl ".to_vec();
        bytes.extend_from_slice(&((JXL.len() + 8) as u32).to_be_bytes());
        bytes.extend_from_slice(b"jxlc");
        bytes.extend_from_slice(JXL);
        bytes.extend_from_slice(&72u32.to_be_bytes());
        bytes.extend_from_slice(b"free");
        let boundary = bytes.len();
        bytes.extend_from_slice(&[0; 64]);
        (bytes, boundary)
    }
    fn package() -> Vec<u8> {
        let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"fixture","name":"Neutral progressive fixture","version":"1.0.0","language":"en","entry":"main.js","scripts":[],"assets":[],"permissions":{"domains":["example.test"]},"capabilities":["pages"]});
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            (
                "main.js",
                b"globalThis.novaProvider={handle:()=>({})};".to_vec(),
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    struct Transport {
        body: Vec<u8>,
        boundary: usize,
        mode: String,
        ready: AtomicBool,
        calls: AtomicUsize,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl SourceTransport for Transport {
        fn page_cache_partition(&self, _: &str) -> Option<String> {
            Some("neutral-progressive".into())
        }
        fn request(
            &self,
            request: &HttpRequest,
            _: usize,
            _: &CancelToken,
        ) -> reader_sources::Result<HttpResponse> {
            Ok(HttpResponse {
                status: 200,
                url: request.url.clone(),
                headers: BTreeMap::new(),
                body: self.body.clone(),
            })
        }
        fn request_with_chunks(
            &self,
            request: &HttpRequest,
            _: usize,
            cancel: &CancelToken,
            on_chunk: &mut dyn FnMut(&[u8]) -> reader_sources::Result<()>,
        ) -> reader_sources::Result<HttpResponse> {
            let call = self.calls.fetch_add(1, Ordering::AcqRel);
            let boundary = if call == 0 {
                self.boundary
            } else {
                self.body.len()
            };
            for part in self.body[..boundary].chunks(8192) {
                cancel.check()?;
                on_chunk(part)?;
                // Model a slow network rather than depending on test-only
                // decoder behavior; the 200ms production throttle remains real.
                if call == 0 && self.mode != "png" {
                    std::thread::sleep(Duration::from_millis(210));
                }
            }
            self.ready.store(true, Ordering::Release);
            if call == 0 {
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(8))
                    .expect("UI never released the simulated HTTP tail");
            }
            cancel.check()?;
            let invalid = call == 0 && self.mode == "invalid";
            let end = if invalid {
                boundary + 8
            } else {
                self.body.len()
            };
            for part in self.body[boundary..end].chunks(8192) {
                cancel.check()?;
                on_chunk(part)?;
            }
            Ok(HttpResponse {
                status: 200,
                url: request.url.clone(),
                headers: BTreeMap::new(),
                body: self.body[..end].to_vec(),
            })
        }
    }
    #[test]
    fn progressive_streaming_keeps_previews_provisional_and_final_eof_authoritative() {
        for mode in [
            "valid",
            "rtl",
            "invalid",
            "cancel",
            "settings",
            "png",
            "transformed",
            "unpump",
            "resource",
        ] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "--ignored",
                    "app::reader_progressive::tests::streamed_jxl_child",
                ])
                .env("NOVA_READER_PROGRESSIVE_CASE", mode)
                .status()
                .unwrap();
            assert!(status.success(), "progressive case {mode} failed");
        }
    }
    #[test]
    #[ignore = "isolated Slint event loop and storage"]
    fn streamed_jxl_child() {
        let mode = std::env::var("NOVA_READER_PROGRESSIVE_CASE").unwrap();
        i_slint_backend_testing::init_integration_test_with_system_time();
        let directory = tempfile::tempdir().unwrap();
        let ui = AppWindow::new().unwrap();
        ui.set_section(6);
        ui.window().set_size(slint::LogicalSize::new(390., 844.));
        let services = Services::open(&directory.path().join("reader")).unwrap();
        services.sources.install_zip(&package()).unwrap();
        services.sources.set_enabled("fixture", true).unwrap();
        let (release, tail) = mpsc::channel();
        let (body, boundary) = if mode == "png" {
            use image::ImageEncoder;
            let mut bytes = Vec::new();
            image::codecs::png::PngEncoder::new(&mut bytes)
                .write_image(
                    &[10, 50, 90, 255].repeat(6),
                    2,
                    3,
                    image::ExtendedColorType::Rgba8,
                )
                .unwrap();
            let len = bytes.len();
            (bytes, len)
        } else {
            boxed_jxl()
        };
        let transport = Arc::new(Transport {
            body,
            boundary,
            mode: mode.clone(),
            ready: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            release: Mutex::new(tail),
        });
        let source = services
            .sources
            .source("fixture")
            .unwrap()
            .with_transport(transport.clone());
        let mut reader = super::super::tests::tall_reader();
        let page = reader_sources::Page {
            id: "page-0".into(),
            url: "https://example.test/page.jxl".into(),
            headers: BTreeMap::new(),
            width: None,
            height: None,
        };
        reader.pages = vec![PageInput::Remote(page)];
        reader.source = Some(source);
        reader.ratios = vec![1.5];
        reader.viewport_width = 390.;
        reader.viewport_height = 844.;
        reader.target_width = 512;
        if mode == "rtl" {
            reader.mode = ReadingMode::RightToLeft;
            reader.display_options.rtl = true;
        }
        if mode == "transformed" {
            reader.display_options.grayscale = true;
        }
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
            services,
            chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
                directory.path().join("viewed"),
            )),
            capture_originals: Mutex::default(),
            image_worker_live: AtomicBool::new(true),
            view: Mutex::new(View {
                section: 6,
                reader: Some(reader),
                ..View::default()
            }),
            generation: AtomicU64::new(1),
            cancel: Mutex::new(CancelToken::default()),
            images,
            download_serial: Mutex::new(()),
            active_image: Mutex::new(None),
            metadata_refresh: Arc::default(),
        });
        app.mount_pages(&ui);
        if mode == "unpump" || mode == "resource" {
            app.view().reader.as_mut().unwrap().desired.insert(0);
            let source = app.view().reader.as_ref().unwrap().source.clone().unwrap();
            let page = match &app.view().reader.as_ref().unwrap().pages[0] {
                PageInput::Remote(page) => page.clone(),
                _ => unreachable!(),
            };
            let cancel = CancelToken::default();
            let request = app
                .chapter_cache
                .prepare(
                    &app.view().reader.as_ref().unwrap().chapter,
                    &page,
                    &source,
                    cancel.clone(),
                    Arc::new(|| true),
                )
                .unwrap();
            let mut stream = Stream::new(
                &app,
                ui.as_weak(),
                &ImageTask {
                    generation: 1,
                    index: 0,
                },
                cancel,
                request,
            )
            .unwrap();
            if mode == "resource" {
                stream.decoder = Decoder::Active(Box::new(
                    nova_image::ProgressiveJxlDecoder::new(
                        reader_engine::DecodeLimits {
                            max_decoded_bytes: 1,
                            ..Default::default()
                        },
                        nova_image::JxlPreviewOptions::default(),
                    )
                    .unwrap(),
                ));
                stream.feed(JXL, || {});
                assert!(matches!(stream.decoder, Decoder::Disabled));
                assert!(stream.failure.is_none());
                stream.finish().unwrap();
                assert!(
                    reader_engine::decode_image(JXL, reader_engine::DecodeLimits::default())
                        .is_ok()
                );
            } else {
                stream.feed(JXL, || {});
                assert!(stream.mailbox.pending.lock().unwrap().queued);
                let before = Instant::now();
                stream.finish().unwrap();
                assert!(
                    before.elapsed() < Duration::from_secs(1),
                    "teardown waited for an unpumped UI loop"
                );
                assert!(stream.mailbox.pending.lock().unwrap().frame.is_none());
                let newer = stream.mailbox.job + 1;
                app.view().reader.as_mut().unwrap().progressive_job = Some((0, newer));
                let mut row = ui.get_pages().row_data(0).unwrap();
                row.preview = true;
                ui.get_pages().set_row_data(0, row);
                stream.cleanup().clear();
                slint::invoke_from_event_loop(|| slint::quit_event_loop().unwrap()).unwrap();
                slint::run_event_loop().unwrap();
                assert!(
                    ui.get_pages().row_data(0).unwrap().preview,
                    "old cleanup blanked a new same-index job"
                );
                let mut row = ui.get_pages().row_data(0).unwrap();
                row.loaded = true;
                row.preview = true;
                row.image = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(2, 3));
                clear_preview(&mut row);
                assert!(row.loaded);
                assert_eq!(row.image.size().width, 2);
            }
            app.image_worker_live.store(false, Ordering::Release);
            return;
        }
        let weak = Arc::downgrade(&app);
        let weak_ui = ui.as_weak();
        let worker = std::thread::spawn(move || Controller::image_worker(weak, receiver, weak_ui));
        let seen = Rc::new(Cell::new(false));
        let released = Rc::new(Cell::new(false));
        let retried = Rc::new(Cell::new(false));
        let failure = Rc::new(RefCell::new(None::<String>));
        let started = Instant::now();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(10), {
            let app = app.clone();
            let weak_ui = ui.as_weak();
            let mode = mode.clone();
            let seen = seen.clone();
            let released = released.clone();
            let retried = retried.clone();
            let failure = failure.clone();
            move || {
                let Some(ui) = weak_ui.upgrade() else { return };
                let row = ui.get_pages().row_data(0).unwrap();
                let fallback = mode == "png" || mode == "transformed";
                if row.preview && !seen.get() {
                    assert!(!fallback);
                    assert!(!row.loaded);
                    assert!(row.tiles.row_count() > 0);
                    assert_eq!(row.ratio, 0.75);
                    let view = app.view();
                    let reader = view.reader.as_ref().unwrap();
                    assert!(reader.retained.is_empty());
                    assert!(reader.decoded_bytes.is_empty());
                    assert!(reader.decoded_windows.is_empty());
                    assert!(!reader.completed);
                    assert!(view.pending.is_none());
                    assert!(app.services.downloads.jobs().unwrap().is_empty());
                    seen.set(true);
                }
                if !released.get()
                    && (seen.get() || fallback && transport.ready.load(Ordering::Acquire))
                {
                    if fallback {
                        assert!(!row.preview);
                        assert!(!row.loaded);
                    }
                    if mode == "cancel" {
                        if let Some((_, cancel)) = app.active_image.lock().unwrap().as_ref() {
                            cancel.cancel();
                        }
                    }
                    if mode == "settings" {
                        app.view()
                            .reader
                            .as_mut()
                            .unwrap()
                            .display_options
                            .grayscale = true;
                    }
                    released.set(true);
                    release.send(()).unwrap();
                }
                if mode == "invalid" && !row.error.is_empty() && !retried.get() {
                    assert!(!row.preview);
                    assert!(!row.loaded);
                    assert_eq!(row.tiles.row_count(), 0);
                    let view = app.view();
                    assert!(view.reader.as_ref().unwrap().retained.is_empty());
                    assert!(!view.reader.as_ref().unwrap().completed);
                    assert!(view.pending.is_none());
                    drop(view);
                    retried.set(true);
                    app.step_page(&ui, 0);
                }
                if row.loaded {
                    assert!(!row.preview);
                    assert_eq!(row.ratio, if mode == "png" { 1.5 } else { 0.75 });
                    assert!(app.view().reader.as_ref().unwrap().retained.contains(&0));
                    if mode == "invalid" || mode == "cancel" || mode == "settings" {
                        assert_eq!(transport.calls.load(Ordering::Acquire), 2);
                    }
                    if mode == "transformed" || mode == "settings" {
                        let tile = row.tiles.row_data(0).unwrap();
                        let pixels = tile.image.to_rgba8().unwrap();
                        assert!(
                            pixels
                                .as_bytes()
                                .chunks_exact(4)
                                .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2])
                        );
                    }
                    slint::quit_event_loop().unwrap();
                } else if started.elapsed() > Duration::from_secs(12) {
                    *failure.borrow_mut() = Some(format!(
                        "timed out: mode={mode},preview={},error={}",
                        row.preview, row.error
                    ));
                    slint::quit_event_loop().unwrap();
                }
            }
        });
        app.request_images(&ui, 0);
        slint::run_event_loop().unwrap();
        timer.stop();
        app.image_worker_live.store(false, Ordering::Release);
        app.invalidate();
        worker.join().unwrap();
        assert!(failure.borrow().is_none(), "{:?}", failure.borrow());
        assert_eq!(seen.get(), mode != "png" && mode != "transformed");
    }
}

//! Reader image encoding and delivery. Completion can run on a worker thread; UI callers
//! must return through Slint's event loop before updating UI state.

use crate::capture_settings::{CaptureFormat, CapturePreferences, ConflictPolicy};
use crate::image_storage::ImageStorageFormat;
use image::ImageEncoder;
use std::sync::Arc;

pub struct ExportImage {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub name: String,
}

impl ExportImage {
    /// Preserve the original encoded page rather than saving its display tiles.
    pub fn original(bytes: Vec<u8>, name: &str) -> Result<Self, String> {
        reader_engine::inspect_image(&bytes, reader_engine::DecodeLimits::default())
            .map_err(|error| format!("Cannot export page image: {error}"))?;
        let (mime, extension) = if reader_engine::is_jpeg_xl(&bytes) {
            ("image/jxl", "jxl")
        } else {
            match image::guess_format(&bytes).map_err(|error| error.to_string())? {
                image::ImageFormat::Png => ("image/png", "png"),
                image::ImageFormat::Jpeg => ("image/jpeg", "jpg"),
                image::ImageFormat::WebP => ("image/webp", "webp"),
                image::ImageFormat::Avif => ("image/avif", "avif"),
                _ => return Err("Unsupported page image format".into()),
            }
        };
        let stem = std::path::Path::new(name)
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("page");
        let stem = stem
            .chars()
            .take(120)
            .map(|character| {
                if character.is_control() || r#"/\:*?"<>|"#.contains(character) {
                    '_'
                } else {
                    character
                }
            })
            .collect::<String>();
        Ok(Self {
            bytes,
            mime: mime.into(),
            name: format!("{stem}.{extension}"),
        })
    }

    /// Encode an RGBA screenshot with a checked size before entering the PNG
    /// encoder. A screenshot can contain up to eight million physical pixels.
    #[cfg(test)]
    pub fn from_rgba(width: u32, height: u32, pixels: &[u8]) -> Result<Self, String> {
        Self::from_rgba_with_preferences(width, height, pixels, &CapturePreferences::default())
    }

    pub fn from_rgba_with_preferences(
        width: u32,
        height: u32,
        pixels: &[u8],
        preferences: &CapturePreferences,
    ) -> Result<Self, String> {
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .filter(|bytes| {
                *bytes > 0 && *bytes <= 8_000_000 * 4 && width <= 16384 && height <= 65536
            })
            .ok_or_else(|| "Screenshot dimensions exceed supported limits".to_string())?;
        if expected != pixels.len() as u64 {
            return Err("Screenshot pixels do not match its dimensions".into());
        }
        let quality = match preferences.format {
            CaptureFormat::Jpeg => preferences.jpeg_quality,
            CaptureFormat::Avif => preferences.avif_quality,
            _ => preferences.webp_quality,
        };
        let bytes = encode_rgba(
            width,
            height,
            pixels,
            preferences.format,
            quality,
            reader_engine::DecodeLimits::default(),
        )?;
        if preferences.format == CaptureFormat::Avif {
            reader_engine::inspect_image(&bytes, reader_engine::DecodeLimits::default())
                .map_err(|error| format!("AVIF screenshot exceeds decode admission: {error}"))?;
        }
        let (mime, extension) = match preferences.format {
            CaptureFormat::Png => ("image/png", "png"),
            CaptureFormat::Jpeg => ("image/jpeg", "jpg"),
            CaptureFormat::WebpLossless | CaptureFormat::WebpLossy => ("image/webp", "webp"),
            CaptureFormat::Avif => ("image/avif", "avif"),
        };
        Ok(Self {
            bytes,
            mime: mime.into(),
            name: format!("screencap.{extension}"),
        })
    }
}

/// Reencode only newly fetched storage bytes. Source dimensions are preserved;
/// callers choose their admission limits and retain existing stored files.
pub(crate) fn reencode_storage_image(
    bytes: &[u8],
    compression: crate::image_storage::ImageCompression,
    limits: reader_engine::DecodeLimits,
) -> Result<Vec<u8>, String> {
    if compression.jxl_lossless_jpeg
        && matches!(
            compression.format,
            ImageStorageFormat::JxlLossless | ImageStorageFormat::JxlLossy
        )
        && let Ok(encoded) = nova_image::transcode_jpeg_to_jxl(bytes, limits)
    {
        // A valid JXL header does not establish that its float grids fit the
        // reader's decode budget. Never commit an unreadable offline variant.
        nova_image::decode_jxl(&encoded, limits).map_err(|error| error.to_string())?;
        return Ok(encoded);
    }
    reencode_image(bytes, compression.format, compression.quality, limits)
}

pub(crate) fn reencode_image(
    bytes: &[u8],
    format: ImageStorageFormat,
    quality: u8,
    limits: reader_engine::DecodeLimits,
) -> Result<Vec<u8>, String> {
    let image = reader_engine::decode_image(bytes, limits)
        .map_err(|error| format!("Cannot decode image for compression: {error}"))?;
    // Keep transparent artwork readable: callers retain the original when
    // JPEG cannot represent it. Screenshot exports use Nova's separate rules.
    if format == ImageStorageFormat::Jpeg && image.pixels().any(|pixel| pixel[3] != 255) {
        return Err("JPEG compression cannot preserve image transparency".into());
    }
    let encoded = match format {
        ImageStorageFormat::JxlLossless | ImageStorageFormat::JxlLossy => nova_image::encode_jxl(
            image.width(),
            image.height(),
            image.as_raw(),
            if format == ImageStorageFormat::JxlLossless {
                nova_image::JxlEncoding::Lossless
            } else {
                nova_image::JxlEncoding::Lossy { quality }
            },
            limits,
        )
        .map_err(|error| error.to_string())?,
        ImageStorageFormat::Jpeg => encode_rgba(
            image.width(),
            image.height(),
            image.as_raw(),
            CaptureFormat::Jpeg,
            quality,
            limits,
        )?,
        ImageStorageFormat::Webp => encode_rgba(
            image.width(),
            image.height(),
            image.as_raw(),
            CaptureFormat::WebpLossy,
            quality,
            limits,
        )?,
        ImageStorageFormat::Avif => encode_rgba(
            image.width(),
            image.height(),
            image.as_raw(),
            CaptureFormat::Avif,
            quality,
            limits,
        )?,
    };
    if encoded.len() > limits.max_encoded_bytes {
        return Err("Compressed image exceeds encoded byte limit".into());
    }
    drop(image);
    if matches!(
        format,
        ImageStorageFormat::JxlLossless | ImageStorageFormat::JxlLossy
    ) {
        nova_image::decode_jxl(&encoded, limits)
            .map_err(|error| format!("Compressed image exceeds decode admission: {error}"))?;
    } else {
        reader_engine::inspect_image(&encoded, limits)
            .map_err(|error| format!("Compressed image exceeds decode admission: {error}"))?;
    }
    Ok(encoded)
}

fn encode_rgba(
    width: u32,
    height: u32,
    pixels: &[u8],
    format: CaptureFormat,
    quality: u8,
    limits: reader_engine::DecodeLimits,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let quality = quality.clamp(1, 100);
    match format {
        CaptureFormat::Png => {
            image::codecs::png::PngEncoder::new(&mut bytes)
                .write_image(pixels, width, height, image::ExtendedColorType::Rgba8)
                .map_err(|error| format!("Cannot encode image: {error}"))?;
        }
        CaptureFormat::Jpeg => {
            // Match Nova: JPEG stores the source RGB channels and has no alpha.
            let rgb = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|pixel| pixel[..3].iter().copied())
                .collect::<Vec<_>>();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
                .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
                .map_err(|error| format!("Cannot encode image: {error}"))?;
        }
        CaptureFormat::WebpLossless | CaptureFormat::WebpLossy => {
            if width > 16383 || height > 16383 {
                return Err("WebP supports image dimensions up to 16383 pixels".into());
            }
            bytes = webp::Encoder::from_rgba(pixels, width, height)
                .encode_simple(format == CaptureFormat::WebpLossless, f32::from(quality))
                .map_err(|error| format!("Cannot encode image: {error:?}"))?
                .to_vec();
        }
        CaptureFormat::Avif => {
            bytes = nova_image::encode_avif(width, height, pixels, quality, limits)
                .map_err(|error| format!("Cannot encode AVIF image: {error}"))?;
        }
    }
    Ok(bytes)
}

type Callback = Box<dyn FnOnce(Result<(), String>) + Send + 'static>;

/// Nonblocking delivery, with explicit completion for errors and cancellation.
#[cfg(all(test, target_os = "linux"))]
pub fn deliver(
    image: ExportImage,
    clipboard: bool,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    let name = image.name.clone();
    deliver_with_context(
        image,
        clipboard,
        SaveContext::new(
            name,
            "",
            &CapturePreferences::default(),
            Arc::new(|_, _, reply| reply(ConflictChoice::Cancel)),
            Arc::new(|| true),
        ),
        callback,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictChoice {
    Overwrite,
    AddNumber,
    Cancel,
}
pub type ConflictReply = Box<dyn FnOnce(ConflictChoice) + Send + 'static>;
pub type ConflictResolver = Arc<dyn Fn(String, ExportCurrent, ConflictReply) + Send + Sync>;
pub type ExportCurrent = Arc<dyn Fn() -> bool + Send + Sync>;

pub fn deliver_with_context(
    image: ExportImage,
    clipboard: bool,
    context: SaveContext,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    if !matches!(
        image.mime.as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/avif" | "image/jxl"
    ) {
        callback(Err("Unsupported image export format".into()));
        return;
    }
    platform::deliver(image, clipboard, context, Box::new(callback));
}

pub struct SaveContext {
    pub filename: String,
    pub folder: Option<String>,
    pub policy: ConflictPolicy,
    pub resolver: ConflictResolver,
    pub current: ExportCurrent,
}

impl SaveContext {
    pub fn new(
        filename: String,
        series_title: &str,
        preferences: &CapturePreferences,
        resolver: ConflictResolver,
        current: ExportCurrent,
    ) -> Self {
        Self {
            filename,
            folder: preferences
                .series_folders
                .then(|| crate::export_names::folder(series_title)),
            policy: preferences.conflict,
            resolver,
            current,
        }
    }
}

pub fn request_conflict(context: &SaveContext, name: String, callback: ConflictReply) {
    if !(context.current)() {
        callback(ConflictChoice::Cancel);
        return;
    }
    (context.resolver)(name, context.current.clone(), callback);
}

#[cfg(not(target_os = "android"))]
fn publish_file(
    root: &std::path::Path,
    image: &ExportImage,
    context: &SaveContext,
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let directory = context
        .folder
        .as_ref()
        .map_or_else(|| root.to_path_buf(), |folder| root.join(folder));
    let filename = &context.filename;
    if filename.is_empty()
        || filename.len() > 160
        || std::path::Path::new(filename)
            .file_name()
            .and_then(|s| s.to_str())
            != Some(filename)
    {
        return Err("Invalid image filename".into());
    }
    let destination = directory.join(filename);
    if !(context.current)() {
        return Err("Image action cancelled".into());
    }
    std::fs::create_dir_all(&directory).map_err(|e| format!("Cannot create image folder: {e}"))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".nova-reader-image-")
        .tempfile_in(&directory)
        .map_err(|e| format!("Cannot stage saved image: {e}"))?;
    staged
        .write_all(&image.bytes)
        .and_then(|_| staged.flush())
        .and_then(|_| staged.as_file().sync_all())
        .map_err(|e| format!("Cannot write saved image: {e}"))?;
    let mut path = destination.clone();
    let mut number = 0u32;
    loop {
        if !(context.current)() {
            return Err("Image action cancelled".into());
        }
        match staged.persist_noclobber(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                staged = error.file
            }
            Err(error) => return Err(format!("Cannot publish saved image: {}", error.error)),
        }
        if number == 0 {
            let choice = match context.policy {
                ConflictPolicy::Overwrite => ConflictChoice::Overwrite,
                ConflictPolicy::DoNothing => {
                    return Err("Image already exists; nothing saved.".into());
                }
                ConflictPolicy::Ask => {
                    let (send, receive) = std::sync::mpsc::sync_channel(1);
                    request_conflict(
                        context,
                        filename.clone(),
                        Box::new(move |choice| {
                            let _ = send.try_send(choice);
                        }),
                    );
                    loop {
                        match receive.recv_timeout(std::time::Duration::from_millis(50)) {
                            Ok(choice) => break choice,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                break ConflictChoice::Cancel;
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                                if !(context.current)() =>
                            {
                                break ConflictChoice::Cancel;
                            }
                            Err(_) => {}
                        }
                    }
                }
            };
            match choice {
                ConflictChoice::Cancel => return Err("Image action cancelled".into()),
                ConflictChoice::Overwrite => {
                    if !(context.current)() {
                        return Err("Image action cancelled".into());
                    }
                    // tempfile persist replaces atomically, including on Windows.
                    return staged
                        .persist(&destination)
                        .map(|_| destination)
                        .map_err(|e| format!("Cannot replace saved image: {}", e.error));
                }
                ConflictChoice::AddNumber => {}
            }
        }
        number = number
            .checked_add(1)
            .filter(|n| *n <= 10000)
            .ok_or("Cannot allocate an image filename")?;
        path = directory.join(crate::export_names::numbered(filename, number));
    }
}

/// Called after the UI event loop exits so the clipboard owner can hand its
/// contents to a clipboard manager before the process shuts down.
pub fn shutdown() {
    platform::shutdown();
}

#[cfg(not(target_os = "android"))]
mod platform {
    use super::{Callback, ExportImage, SaveContext};
    use std::sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    static BUSY: AtomicBool = AtomicBool::new(false);

    struct Job {
        image: ExportImage,
        clipboard: bool,
        context: SaveContext,
        callback: Callback,
    }
    struct Worker {
        sender: mpsc::SyncSender<Job>,
        thread: std::thread::JoinHandle<()>,
        done: mpsc::Receiver<()>,
    }
    static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();

    fn complete(callback: Callback, result: Result<(), String>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(result)));
    }

    pub(super) fn deliver(
        image: ExportImage,
        clipboard: bool,
        context: SaveContext,
        callback: Callback,
    ) {
        if BUSY.swap(true, Ordering::AcqRel) {
            complete(callback, Err("Another image export is active".into()));
            return;
        }
        let callback: Callback = Box::new(move |result| {
            BUSY.store(false, Ordering::Release);
            callback(result);
        });
        let mut worker = WORKER
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if worker.is_none() {
            let (sender, receiver) = mpsc::sync_channel::<Job>(1);
            let (finished, done) = mpsc::sync_channel(1);
            let thread = match std::thread::Builder::new()
                .name("reader-image-export".into())
                .spawn(move || {
                    // Keep the clipboard alive while the user pastes. Linux
                    // selections are served by their owner after set_image.
                    let mut clipboard = None;
                    while let Ok(job) = receiver.recv() {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            if job.clipboard {
                                copy(&job.image, &mut clipboard)
                            } else {
                                save(&job.image, &job.context)
                            }
                        }))
                        .unwrap_or_else(|_| Err("Image export worker failed".into()));
                        drop(job.image);
                        complete(job.callback, result);
                    }
                    drop(clipboard);
                    let _ = finished.send(());
                }) {
                Ok(thread) => thread,
                Err(error) => {
                    drop(worker);
                    complete(callback, Err(format!("Cannot start image export: {error}")));
                    return;
                }
            };
            *worker = Some(Worker {
                sender,
                thread,
                done,
            });
        }
        let result = worker.as_ref().unwrap().sender.try_send(Job {
            image,
            clipboard,
            context,
            callback,
        });
        drop(worker);
        if let Err(error) = result {
            let (job, message) = match error {
                mpsc::TrySendError::Full(job) => (job, "Another image export is active"),
                mpsc::TrySendError::Disconnected(job) => (job, "Image export worker stopped"),
            };
            complete(job.callback, Err(message.into()));
        }
    }

    fn save(image: &ExportImage, context: &SaveContext) -> Result<(), String> {
        let directories = directories::UserDirs::new()
            .ok_or_else(|| "Pictures directory is unavailable".to_string())?;
        let pictures = directories
            .picture_dir()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| directories.home_dir().join("Pictures"));
        super::publish_file(&pictures.join("Nova Reader"), image, context).map(|_| ())
    }

    fn copy(image: &ExportImage, clipboard: &mut Option<arboard::Clipboard>) -> Result<(), String> {
        let rgba = reader_engine::decode_image(
            &image.bytes,
            reader_engine::DecodeLimits {
                // Screenshots are generated locally and may exceed a source
                // page's encoded byte limit while retaining the decode limits.
                max_encoded_bytes: 128 * 1024 * 1024,
                ..reader_engine::DecodeLimits::default()
            },
        )
        .map_err(|error| format!("Cannot decode clipboard image: {error}"))?;
        if clipboard.is_none() {
            *clipboard = Some(
                arboard::Clipboard::new()
                    .map_err(|error| format!("Cannot access image clipboard: {error}"))?,
            );
        }
        clipboard
            .as_mut()
            .unwrap()
            .set_image(arboard::ImageData {
                width: rgba.width() as usize,
                height: rgba.height() as usize,
                bytes: std::borrow::Cow::Owned(rgba.into_raw()),
            })
            .map_err(|error| format!("Cannot copy image: {error}"))
    }

    pub(super) fn shutdown() {
        let worker = WORKER.get().and_then(|worker| {
            worker
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
        });
        if let Some(Worker {
            sender,
            thread,
            done,
        }) = worker
        {
            drop(sender);
            // A save dialog or clipboard manager can outlive the main window.
            // Allow a brief clean handover without blocking application exit.
            if done
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_ok()
            {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(target_os = "android")]
mod platform {
    use super::{Callback, ConflictPolicy, ExportImage, SaveContext};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    static BUSY: AtomicBool = AtomicBool::new(false);
    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
    type Completion = Arc<Mutex<Option<Callback>>>;

    fn finish(completion: Completion, result: Result<(), String>) {
        let callback = completion
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(callback) = callback {
            BUSY.store(false, Ordering::Release);
            callback(result);
        }
    }

    pub(super) fn deliver(
        image: ExportImage,
        clipboard: bool,
        context: SaveContext,
        callback: Callback,
    ) {
        if BUSY.swap(true, Ordering::AcqRel) {
            callback(Err("Another image export is active".into()));
            return;
        }
        let completion = Arc::new(Mutex::new(Some(callback)));
        let on_spawn_error = completion.clone();
        // File staging also runs off the UI thread. Java copies clipboard data
        // to its provider root before reporting success, so this temporary file
        // can be removed after either destination finishes.
        let thread = std::thread::Builder::new()
            .name("reader-image-export".into())
            .spawn(move || {
                if !(context.current)() {finish(completion,Err("Image action cancelled".into()));return;}
                let name = context.filename.clone();
                let staged = (|| {
                    let root = crate::android_documents::cache_dir()?.join("image-export");
                    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
                    // Only one export can be active. Any previous stage was
                    // abandoned by process death; provider-owned copies live
                    // in Java's separate root and never need this directory.
                    for entry in std::fs::read_dir(&root).map_err(|error| error.to_string())? {
                        let entry = entry.map_err(|error| error.to_string())?;
                        if entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "image")
                        {
                            std::fs::remove_file(entry.path())
                                .map_err(|error| format!("Cannot clean staged image: {error}"))?;
                        }
                    }
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_nanos();
                    let path = root.join(format!(
                        "{}-{timestamp}-{}.image",
                        std::process::id(),
                        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
                    ));
                    std::fs::write(&path, image.bytes).map_err(|error| {
                        let _ = std::fs::remove_file(&path);
                        format!("Cannot stage image: {error}")
                    })?;
                    Ok::<_, String>(path)
                })();
                let path = match staged {
                    Ok(path) => path,
                    Err(error) => {
                        finish(completion, Err(error));
                        return;
                    }
                };
                let cleanup = path.clone();
                if !(context.current)() {let _=std::fs::remove_file(&cleanup);finish(completion,Err("Image action cancelled".into()));return;}
                let finished = move |result| {
                    let _ = std::fs::remove_file(cleanup);
                    finish(completion, result);
                };
                if clipboard {
                    crate::android_documents::copy_image(path, &image.mime, finished);
                } else {
                    let policy=match context.policy {
                        ConflictPolicy::Ask=>crate::android_documents::ImageConflictPolicy::Ask,
                        ConflictPolicy::Overwrite=>crate::android_documents::ImageConflictPolicy::Overwrite,
                        ConflictPolicy::DoNothing=>crate::android_documents::ImageConflictPolicy::DoNothing,
                    };
                    let context=Arc::new(context);
                    let resolver_context=context.clone();
                    let resolver:crate::android_documents::ImageConflictResolver=Arc::new(move |name,reply| {
                        let current=resolver_context.current.clone();
                        super::request_conflict(&resolver_context,name,Box::new(move |choice| {
                            let choice=if !current() {crate::android_documents::ImageConflictChoice::Cancel} else {match choice {
                                super::ConflictChoice::Overwrite=>crate::android_documents::ImageConflictChoice::Overwrite,
                                super::ConflictChoice::AddNumber=>crate::android_documents::ImageConflictChoice::AddNumber,
                                super::ConflictChoice::Cancel=>crate::android_documents::ImageConflictChoice::Cancel,
                            }};
                            reply(choice);
                        }));
                    });
                    crate::android_documents::publish_image_with_policy(
                        path,
                        &name,
                        &image.mime,
                        context.folder.as_deref(),
                        crate::android_documents::ImageSaveOptions {policy,resolver,current:context.current.clone()},
                        finished,
                    );
                }
            });
        if let Err(error) = thread {
            finish(
                on_spawn_error,
                Err(format!("Cannot start image export: {error}")),
            );
        }
    }

    pub(super) fn shutdown() {
        if BUSY.load(Ordering::Acquire) {
            crate::android_documents::cancel_pending();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_png_preserves_physical_pixels_and_alpha() {
        let pixels = [
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 17, 31, 47, 255,
        ];
        let screenshot = ExportImage::from_rgba(2, 2, &pixels).unwrap();
        assert_eq!(screenshot.mime, "image/png");
        assert_eq!(screenshot.name, "screencap.png");
        let decoded = image::load_from_memory(&screenshot.bytes)
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(decoded.as_raw(), &pixels);
    }

    #[test]
    fn original_keeps_bytes_and_uses_actual_format() {
        let screenshot = ExportImage::from_rgba(1, 1, &[23, 37, 51, 255]).unwrap();
        let expected = screenshot.bytes.clone();
        let image = ExportImage::original(screenshot.bytes, "../chapter/page.jpg").unwrap();
        assert_eq!(image.bytes, expected);
        assert_eq!(image.mime, "image/png");
        assert_eq!(image.name, "page.png");
        assert!(ExportImage::original(vec![1, 2, 3], "bad.png").is_err());
    }

    #[test]
    fn malformed_screenshot_dimensions_fail_before_encoding() {
        assert!(ExportImage::from_rgba(0, 1, &[]).is_err());
        assert!(ExportImage::from_rgba(2, 1, &[0; 4]).is_err());
        assert!(ExportImage::from_rgba(u32::MAX, u32::MAX, &[]).is_err());
        assert!(ExportImage::from_rgba(8192, 8192, &[]).is_err());
    }

    fn detailed_pixels() -> Vec<u8> {
        (0..64 * 64)
            .flat_map(|index| {
                [
                    ((index * 37) % 256) as u8,
                    ((index * 61) % 256) as u8,
                    ((index * 97) % 256) as u8,
                    255,
                ]
            })
            .collect()
    }

    #[test]
    fn capture_formats_and_quality_preferences_reach_the_encoder() {
        let pixels = detailed_pixels();
        for (format, mime, extension) in [
            (CaptureFormat::Png, "image/png", "png"),
            (CaptureFormat::Jpeg, "image/jpeg", "jpg"),
            (CaptureFormat::WebpLossless, "image/webp", "webp"),
            (CaptureFormat::WebpLossy, "image/webp", "webp"),
            (CaptureFormat::Avif, "image/avif", "avif"),
        ] {
            let preferences = CapturePreferences {
                format,
                ..CapturePreferences::default()
            };
            let capture =
                ExportImage::from_rgba_with_preferences(64, 64, &pixels, &preferences).unwrap();
            assert_eq!(capture.mime, mime);
            assert!(capture.name.ends_with(extension));
            let decoded =
                reader_engine::decode_image(&capture.bytes, reader_engine::DecodeLimits::default())
                    .unwrap();
            assert_eq!(decoded.dimensions(), (64, 64));
            if matches!(format, CaptureFormat::Png | CaptureFormat::WebpLossless) {
                assert_eq!(decoded.as_raw(), &pixels);
            }
        }
        for format in [
            CaptureFormat::Jpeg,
            CaptureFormat::WebpLossy,
            CaptureFormat::Avif,
        ] {
            let encode = |quality| {
                ExportImage::from_rgba_with_preferences(
                    64,
                    64,
                    &pixels,
                    &CapturePreferences {
                        format,
                        jpeg_quality: quality,
                        webp_quality: quality,
                        avif_quality: quality,
                        ..CapturePreferences::default()
                    },
                )
                .unwrap()
                .bytes
            };
            let low = encode(1);
            let high = encode(100);
            assert_ne!(low, high);
            assert!(low.len() < high.len());
            assert_eq!(encode(0), low);
            assert_eq!(encode(255), high);
        }
    }

    #[test]
    fn storage_compression_keeps_dimensions_format_and_quality() {
        let pixels = detailed_pixels();
        let source = ExportImage::from_rgba(64, 64, &pixels).unwrap().bytes;
        for (format, expected) in [
            (ImageStorageFormat::Jpeg, image::ImageFormat::Jpeg),
            (ImageStorageFormat::Webp, image::ImageFormat::WebP),
            (ImageStorageFormat::Avif, image::ImageFormat::Avif),
        ] {
            let encode = |quality| {
                reencode_image(
                    &source,
                    format,
                    quality,
                    reader_engine::DecodeLimits::default(),
                )
                .unwrap()
            };
            let low = encode(1);
            let high = encode(100);
            assert_eq!(image::guess_format(&high).unwrap(), expected);
            let dimensions =
                reader_engine::inspect_image(&high, reader_engine::DecodeLimits::default())
                    .unwrap();
            assert_eq!((dimensions.width, dimensions.height), (64, 64));
            assert_ne!(low, high);
            assert!(low.len() < high.len());
            assert_eq!(encode(0), low);
            assert_eq!(encode(255), high);
        }
    }

    #[test]
    fn storage_compression_checks_source_decode_and_encoded_output_limits() {
        let source = ExportImage::from_rgba(16, 16, &vec![255; 16 * 16 * 4])
            .unwrap()
            .bytes;
        let limits = reader_engine::DecodeLimits::default();
        assert!(reencode_image(&[1, 2, 3], ImageStorageFormat::Jpeg, 85, limits).is_err());
        for limits in [
            reader_engine::DecodeLimits {
                max_pixels: 255,
                ..limits
            },
            reader_engine::DecodeLimits {
                max_width: 15,
                ..limits
            },
            reader_engine::DecodeLimits {
                max_decoded_bytes: 512,
                ..limits
            },
            reader_engine::DecodeLimits {
                max_encoded_bytes: source.len() - 1,
                ..limits
            },
        ] {
            assert!(reencode_image(&source, ImageStorageFormat::Jpeg, 85, limits).is_err());
        }
        // The small solid PNG fits the input budget, while JPEG's header and
        // data exceed it; output limits apply after encoding as well.
        let error = reencode_image(
            &source,
            ImageStorageFormat::Jpeg,
            85,
            reader_engine::DecodeLimits {
                max_encoded_bytes: source.len(),
                ..limits
            },
        )
        .unwrap_err();
        assert!(error.contains("encoded byte limit"));
    }

    #[test]
    fn avif_capture_and_storage_roundtrip_rgb_edges_and_alpha() {
        let mut pixels = Vec::new();
        for row in 0..16 {
            for column in 0..16 {
                pixels.extend_from_slice(match (column < 8, row < 8) {
                    (true, true) => &[255, 0, 0, 255],
                    (false, true) => &[0, 255, 0, 128],
                    (true, false) => &[0, 0, 255, 64],
                    (false, false) => &[255, 255, 255, 0],
                });
            }
        }
        let preferences = CapturePreferences {
            format: CaptureFormat::Avif,
            avif_quality: 100,
            ..CapturePreferences::default()
        };
        let capture =
            ExportImage::from_rgba_with_preferences(16, 16, &pixels, &preferences).unwrap();
        assert_eq!(capture.mime, "image/avif");
        assert_eq!(capture.name, "screencap.avif");
        assert_eq!(
            image::guess_format(&capture.bytes).unwrap(),
            image::ImageFormat::Avif
        );
        let source = ExportImage::from_rgba(16, 16, &pixels).unwrap().bytes;
        let stored = reencode_image(
            &source,
            ImageStorageFormat::Avif,
            100,
            reader_engine::DecodeLimits::default(),
        )
        .unwrap();
        for encoded in [&capture.bytes, &stored] {
            let decoded =
                reader_engine::decode_image(encoded, reader_engine::DecodeLimits::default())
                    .unwrap();
            assert_eq!(decoded.dimensions(), (16, 16));
            for (actual, expected) in decoded.pixels().zip(pixels.as_chunks::<4>().0) {
                assert!(actual[3].abs_diff(expected[3]) <= 1);
                if expected[3] != 0 {
                    assert!(
                        actual.0[..3]
                            .iter()
                            .zip(&expected[..3])
                            .all(|(actual, expected)| actual.abs_diff(*expected) <= 12)
                    );
                }
            }
        }
        let original = ExportImage::original(stored, "page.png").unwrap();
        assert_eq!(original.mime, "image/avif");
        assert_eq!(original.name, "page.avif");
    }

    #[test]
    fn jpeg_xl_original_export_preserves_codec_and_bytes() {
        let bytes = include_bytes!("../crates/reader-engine/tests/fixtures/jxl/rgba.jxl");
        let image =
            ExportImage::original(bytes.to_vec(), "Story - Ch. 0001 - P. 0001.jpg").unwrap();
        assert_eq!(image.mime, "image/jxl");
        assert!(image.name.ends_with(".jxl"));
        assert_eq!(image.bytes, bytes);
    }

    #[test]
    fn storage_webp_retains_alpha_and_jpeg_rejects_transparency() {
        let pixels = [
            128, 64, 32, 0, 128, 64, 32, 64, 128, 64, 32, 128, 128, 64, 32, 255,
        ];
        let source = ExportImage::from_rgba(4, 1, &pixels).unwrap().bytes;
        let encoded = reencode_image(
            &source,
            ImageStorageFormat::Webp,
            85,
            reader_engine::DecodeLimits::default(),
        )
        .unwrap();
        let decoded = image::load_from_memory(&encoded).unwrap().to_rgba8();
        assert_eq!(
            decoded.pixels().map(|pixel| pixel[3]).collect::<Vec<_>>(),
            vec![0, 64, 128, 255]
        );

        let lineart = [0, 0, 0, 0, 0, 0, 0, 255];
        let transparent = ExportImage::from_rgba(2, 1, &lineart).unwrap().bytes;
        let error = reencode_image(
            &transparent,
            ImageStorageFormat::Jpeg,
            100,
            reader_engine::DecodeLimits::default(),
        )
        .unwrap_err();
        assert!(error.contains("transparency"));
        assert_eq!(
            image::load_from_memory(&transparent)
                .unwrap()
                .to_rgba8()
                .as_raw(),
            &lineart
        );

        let opaque = ExportImage::from_rgba(2, 1, &[0, 0, 0, 255, 255, 255, 255, 255])
            .unwrap()
            .bytes;
        let jpeg = reencode_image(
            &opaque,
            ImageStorageFormat::Jpeg,
            100,
            reader_engine::DecodeLimits::default(),
        )
        .unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgba8();
        assert!(decoded.get_pixel(0, 0)[0] < 20);
        assert!(decoded.get_pixel(1, 0)[0] > 235);
        assert!(decoded.pixels().all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn storage_webp_reports_unsupported_tall_pages_without_resizing() {
        let pixels = [32, 64, 128, 255].repeat(16384);
        let source = ExportImage::from_rgba(1, 16384, &pixels).unwrap().bytes;
        let error = reencode_image(
            &source,
            ImageStorageFormat::Webp,
            85,
            reader_engine::DecodeLimits::default(),
        )
        .unwrap_err();
        assert!(error.contains("16383"));
    }

    #[test]
    fn storage_compression_allows_pages_larger_than_screencaps_without_resizing() {
        // Nine million source pixels exceed the screenshot cap but fit the
        // download decoder's supplied allocation and pixel limits.
        let source = {
            let pixels = [32, 64, 128, 255].repeat(3000 * 3000);
            let mut encoded = Vec::new();
            image::codecs::png::PngEncoder::new(&mut encoded)
                .write_image(&pixels, 3000, 3000, image::ExtendedColorType::Rgba8)
                .unwrap();
            encoded
        };
        let encoded = reencode_image(
            &source,
            ImageStorageFormat::Jpeg,
            20,
            reader_engine::DecodeLimits::default(),
        )
        .unwrap();
        let dimensions =
            reader_engine::inspect_image(&encoded, reader_engine::DecodeLimits::default()).unwrap();
        assert_eq!((dimensions.width, dimensions.height), (3000, 3000));
    }

    #[cfg(not(target_os = "android"))]
    fn save_context(policy: ConflictPolicy, choice: ConflictChoice) -> SaveContext {
        SaveContext {
            filename: crate::export_names::filename("My Series", "1", 17, None, "png"),
            folder: Some(crate::export_names::folder("My Series")),
            policy,
            resolver: Arc::new(move |_, _, reply| reply(choice)),
            current: Arc::new(|| true),
        }
    }
    #[cfg(not(target_os = "android"))]
    #[test]
    fn image_conflicts_ask_before_overwrite_and_auto_skip_preserves_existing() {
        let root = tempfile::tempdir().unwrap();
        let image = ExportImage::from_rgba(1, 1, &[23, 37, 51, 255]).unwrap();
        let context = save_context(ConflictPolicy::DoNothing, ConflictChoice::Cancel);
        let path = publish_file(root.path(), &image, &context).unwrap();
        std::fs::write(&path, b"previous bytes").unwrap();
        assert!(
            publish_file(root.path(), &image, &context)
                .unwrap_err()
                .contains("nothing saved")
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"previous bytes");
        let mut context = save_context(ConflictPolicy::Ask, ConflictChoice::Cancel);
        let (prompt, receive) = std::sync::mpsc::channel();
        context.resolver = Arc::new(move |name, _, reply| {
            prompt.send((name, reply)).unwrap();
        });
        let directory = root.path().to_path_buf();
        let expected = image.bytes.clone();
        let worker = std::thread::spawn(move || publish_file(&directory, &image, &context));
        let (name, reply) = receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(name, path.file_name().unwrap().to_str().unwrap());
        assert!(!worker.is_finished());
        assert_eq!(std::fs::read(&path).unwrap(), b"previous bytes");
        reply(ConflictChoice::Overwrite);
        assert_eq!(worker.join().unwrap().unwrap(), path);
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        let image = ExportImage::from_rgba(1, 1, &[10, 20, 30, 255]).unwrap();
        let cancelled = save_context(ConflictPolicy::Ask, ConflictChoice::Cancel);
        assert!(publish_file(root.path(), &image, &cancelled).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        let overwrite = save_context(ConflictPolicy::Overwrite, ConflictChoice::Cancel);
        assert_eq!(publish_file(root.path(), &image, &overwrite).unwrap(), path);
        assert_eq!(std::fs::read(&path).unwrap(), image.bytes);
    }
    #[cfg(not(target_os = "android"))]
    #[test]
    fn numbered_exports_are_race_safe_and_cancelled_prompt_releases_image() {
        let root = tempfile::tempdir().unwrap();
        let mut workers = Vec::new();
        for color in 0..4 {
            let directory = root.path().to_path_buf();
            workers.push(std::thread::spawn(move || {
                let image = ExportImage::from_rgba(1, 1, &[color, 0, 0, 255]).unwrap();
                let expected = image.bytes.clone();
                let path = publish_file(
                    &directory,
                    &image,
                    &save_context(ConflictPolicy::Ask, ConflictChoice::AddNumber),
                )
                .unwrap();
                assert!(path.file_name().unwrap().len() <= 160);
                assert_eq!(std::fs::read(&path).unwrap(), expected);
                path
            }));
        }
        let paths = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(paths.len(), 4);
        let mut context = save_context(ConflictPolicy::Ask, ConflictChoice::Cancel);
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let current = live.clone();
        context.current = Arc::new(move || current.load(std::sync::atomic::Ordering::Acquire));
        let (prompt, receive) = std::sync::mpsc::channel();
        context.resolver = Arc::new(move |_, _, reply| {
            prompt.send(reply).unwrap();
        });
        let directory = root.path().to_path_buf();
        let worker = std::thread::spawn(move || {
            let image = ExportImage::from_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
            publish_file(&directory, &image, &context)
        });
        let reply = receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        live.store(false, std::sync::atomic::Ordering::Release);
        assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
        drop(reply);
        assert!(
            std::fs::read_dir(root.path().join(crate::export_names::folder("My Series")))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".nova-reader-image-"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a running X11 display, for example DISPLAY=:99 under Xvfb"]
    fn image_clipboard_remains_readable_after_delivery_callback() {
        let pixels = [255, 0, 0, 255, 0, 128, 255, 255];
        for format in [CaptureFormat::Png, CaptureFormat::Avif] {
            let image = ExportImage::from_rgba_with_preferences(
                2,
                1,
                &pixels,
                &CapturePreferences {
                    format,
                    avif_quality: 100,
                    ..CapturePreferences::default()
                },
            )
            .unwrap();
            let expected =
                reader_engine::decode_image(&image.bytes, reader_engine::DecodeLimits::default())
                    .unwrap()
                    .into_raw();
            let (finished, completion) = std::sync::mpsc::channel();
            deliver(image, true, move |result| finished.send(result).unwrap());
            completion
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("clipboard worker must complete")
                .expect("clipboard image must be accepted");

            // A separate clipboard client reads after the delivery callback
            // returned. AVIF passes through Reader's bounded decoder first.
            let mut observer = arboard::Clipboard::new().unwrap();
            let copied = observer.get_image().unwrap();
            assert_eq!((copied.width, copied.height), (2, 1));
            assert_eq!(copied.bytes.as_ref(), expected);
            drop(observer);
        }
        shutdown();
    }
}

#[cfg(test)]
mod jxl_storage_tests {
    use super::*;
    use crate::image_storage::ImageCompression;
    #[test]
    fn jpeg_transcoding_bypasses_quality_only_for_jpeg_and_is_opt_in() {
        let pixels: Vec<u8> = (0..32 * 24 * 3).map(|i| (i * 17) as u8).collect();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
            .encode(&pixels, 32, 24, image::ExtendedColorType::Rgb8)
            .unwrap();
        let limits = reader_engine::DecodeLimits::default();
        let compression = ImageCompression {
            format: ImageStorageFormat::JxlLossy,
            quality: 20,
            jxl_lossless_jpeg: true,
            ..ImageCompression::default()
        };
        let low = reencode_storage_image(&jpeg, compression, limits).unwrap();
        let high = reencode_storage_image(
            &jpeg,
            ImageCompression {
                quality: 95,
                ..compression
            },
            limits,
        )
        .unwrap();
        assert!(
            reencode_storage_image(
                &jpeg,
                compression,
                reader_engine::DecodeLimits {
                    max_decoded_bytes: 32 * 24 * 4,
                    ..limits
                }
            )
            .is_err()
        );
        assert_eq!(low, high);
        assert_eq!(
            low,
            nova_image::transcode_jpeg_to_jxl(&jpeg, limits).unwrap()
        );
        assert_ne!(
            low,
            reencode_storage_image(
                &jpeg,
                ImageCompression {
                    jxl_lossless_jpeg: false,
                    ..compression
                },
                limits
            )
            .unwrap()
        );
        let image = image::RgbImage::from_raw(32, 24, pixels).unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let png = png.into_inner();
        assert_ne!(
            reencode_storage_image(&png, compression, limits).unwrap(),
            reencode_storage_image(
                &png,
                ImageCompression {
                    quality: 95,
                    ..compression
                },
                limits
            )
            .unwrap()
        );
        assert_eq!(
            reader_engine::inspect_image(&low, limits).unwrap(),
            reader_engine::inspect_image(&jpeg, limits).unwrap()
        );
    }
}

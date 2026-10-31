//! Reader image encoding and delivery. Completion can run on a worker thread; UI callers
//! must return through Slint's event loop before updating UI state.

use crate::capture_settings::{CaptureFormat, CapturePreferences};
use crate::image_storage::ImageStorageFormat;
use image::ImageEncoder;

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
        let (mime, extension) =
            match image::guess_format(&bytes).map_err(|error| error.to_string())? {
                image::ImageFormat::Png => ("image/png", "png"),
                image::ImageFormat::Jpeg => ("image/jpeg", "jpg"),
                image::ImageFormat::WebP => ("image/webp", "webp"),
                _ => return Err("Unsupported page image format".into()),
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
        let quality = if preferences.format == CaptureFormat::Jpeg {
            preferences.jpeg_quality
        } else {
            preferences.webp_quality
        };
        let bytes = encode_rgba(width, height, pixels, preferences.format, quality)?;
        let (mime, extension) = match preferences.format {
            CaptureFormat::Png => ("image/png", "png"),
            CaptureFormat::Jpeg => ("image/jpeg", "jpg"),
            CaptureFormat::WebpLossless | CaptureFormat::WebpLossy => ("image/webp", "webp"),
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
    let format = match format {
        ImageStorageFormat::Jpeg => CaptureFormat::Jpeg,
        ImageStorageFormat::Webp => CaptureFormat::WebpLossy,
    };
    let encoded = encode_rgba(
        image.width(),
        image.height(),
        image.as_raw(),
        format,
        quality,
    )?;
    if encoded.len() > limits.max_encoded_bytes {
        return Err("Compressed image exceeds encoded byte limit".into());
    }
    Ok(encoded)
}

fn encode_rgba(
    width: u32,
    height: u32,
    pixels: &[u8],
    format: CaptureFormat,
    quality: u8,
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
    deliver_with_context(
        image,
        clipboard,
        "",
        &CapturePreferences::default(),
        callback,
    );
}

pub fn deliver_with_context(
    image: ExportImage,
    clipboard: bool,
    series_title: &str,
    preferences: &CapturePreferences,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    let series = safe_component(series_title, 40);
    let context = SaveContext {
        folder: preferences.series_folders.then(|| series.clone()),
        series,
    };
    platform::deliver(image, clipboard, context, Box::new(callback));
}

struct SaveContext {
    series: String,
    folder: Option<String>,
}

fn safe_component(value: &str, max_bytes: usize) -> String {
    let mut result = String::new();
    for character in value.chars() {
        let character = if character.is_alphanumeric() || matches!(character, '-' | '_') {
            character
        } else {
            '_'
        };
        if character == '_' && (result.is_empty() || result.ends_with('_')) {
            continue;
        }
        if result.len() + character.len_utf8() > max_bytes {
            break;
        }
        result.push(character);
    }
    let mut result = result.trim_matches('_').to_string();
    let upper = result.to_ascii_uppercase();
    let device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            upper.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
    if result.is_empty() {
        result = "Series".into();
    }
    if device {
        result.insert(0, '_');
    }
    result
}

fn unique_filename(image: &ExportImage, context: &SaveContext) -> Result<String, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let extension = match image.mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => return Err("Unsupported image export format".into()),
    };
    let stem = std::path::Path::new(&image.name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("image");
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    Ok(format!(
        "{}_{}_{timestamp:x}-{:x}.{extension}",
        context.series,
        safe_component(stem, 60),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
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
    let mut path = directory.join(unique_filename(image, context)?);
    let validate = |path: &std::path::Path| -> Result<(), String> {
        let text = path.to_string_lossy();
        if text.len() > 240 || text.encode_utf16().count() > 240 {
            return Err("Image destination path is too long".into());
        }
        Ok(())
    };
    validate(&path)?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Cannot create image folder: {error}"))?;
    // Stage beside the destination so publication is atomic on the same
    // filesystem. Dropping an unfinished stage removes its hidden file.
    let mut staged = tempfile::Builder::new()
        .prefix(".nova-reader-image-")
        .tempfile_in(&directory)
        .map_err(|error| format!("Cannot stage saved image: {error}"))?;
    staged
        .write_all(&image.bytes)
        .and_then(|_| staged.flush())
        .and_then(|_| staged.as_file().sync_all())
        .map_err(|error| format!("Cannot write saved image: {error}"))?;
    for attempt in 0..32 {
        if attempt > 0 {
            path = directory.join(unique_filename(image, context)?);
            validate(&path)?;
        }
        match staged.persist_noclobber(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                staged = error.file;
            }
            Err(error) => return Err(format!("Cannot publish saved image: {}", error.error)),
        }
    }
    Err("Cannot allocate an image filename".into())
}

/// Called after the UI event loop exits so the clipboard owner can hand its
/// contents to a clipboard manager before the process shuts down.
pub fn shutdown() {
    platform::shutdown();
}

#[cfg(not(target_os = "android"))]
mod platform {
    use super::{Callback, ExportImage, SaveContext};
    use std::sync::{Mutex, OnceLock, mpsc};

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
                        let result = if job.clipboard {
                            copy(&job.image, &mut clipboard)
                        } else {
                            save(&job.image, &job.context)
                        };
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
    use super::{Callback, ExportImage, SaveContext};
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
                let name = match super::unique_filename(&image, &context) {
                    Ok(name) => name,
                    Err(error) => {
                        finish(completion, Err(error));
                        return;
                    }
                };
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
                let finished = move |result| {
                    let _ = std::fs::remove_file(cleanup);
                    finish(completion, result);
                };
                if clipboard {
                    crate::android_documents::copy_image(path, &image.mime, finished);
                } else {
                    crate::android_documents::publish_image(
                        path,
                        &name,
                        &image.mime,
                        context.folder.as_deref(),
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

    pub(super) fn shutdown() {}
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
        ] {
            let preferences = CapturePreferences {
                format,
                ..CapturePreferences::default()
            };
            let capture =
                ExportImage::from_rgba_with_preferences(64, 64, &pixels, &preferences).unwrap();
            assert_eq!(capture.mime, mime);
            assert!(capture.name.ends_with(extension));
            let decoded = image::load_from_memory(&capture.bytes).unwrap().to_rgba8();
            assert_eq!(decoded.dimensions(), (64, 64));
            if matches!(format, CaptureFormat::Png | CaptureFormat::WebpLossless) {
                assert_eq!(decoded.as_raw(), &pixels);
            }
        }
        for format in [CaptureFormat::Jpeg, CaptureFormat::WebpLossy] {
            let encode = |quality| {
                ExportImage::from_rgba_with_preferences(
                    64,
                    64,
                    &pixels,
                    &CapturePreferences {
                        format,
                        jpeg_quality: quality,
                        webp_quality: quality,
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
    #[test]
    fn autosave_uses_series_folders_and_preserves_previous_images() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("Pictures/Nova Reader");
        let image = ExportImage::from_rgba(1, 1, &[23, 37, 51, 255]).unwrap();
        let series = safe_component("My Series/Name", 40);
        for folders in [false, true] {
            let context = SaveContext {
                series: series.clone(),
                folder: folders.then(|| series.clone()),
            };
            let first = publish_file(&root, &image, &context).unwrap();
            let second = publish_file(&root, &image, &context).unwrap();
            assert_ne!(first, second);
            let expected = if folders {
                root.join(&series)
            } else {
                root.clone()
            };
            assert_eq!(first.parent().unwrap(), expected);
            assert_eq!(first.extension().unwrap(), "png");
            assert_eq!(std::fs::read(first).unwrap(), image.bytes);
            assert_eq!(std::fs::read(second).unwrap(), image.bytes);
            assert!(std::fs::read_dir(expected).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".nova-reader-image-")
            }));
        }
        for value in [
            "../CON",
            "AUX",
            "COM¹",
            "../\\<>:\"|?*\n\0",
            "",
            "長い題名".repeat(50).as_str(),
        ] {
            let safe = safe_component(value, 40);
            assert!(!safe.is_empty());
            assert!(safe.len() <= 41);
            assert!(
                !safe
                    .chars()
                    .any(|character| character.is_control() || "<>:\"/\\|?*. ".contains(character))
            );
            assert!(!["CON", "AUX", "COM¹"].contains(&safe.to_ascii_uppercase().as_str()));
        }
        let context = SaveContext {
            series,
            folder: None,
        };
        let too_long = temporary.path().join("a".repeat(240));
        assert!(publish_file(&too_long, &image, &context).is_err());
        assert!(!too_long.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a running X11 display, for example DISPLAY=:99 under Xvfb"]
    fn image_clipboard_remains_readable_after_delivery_callback() {
        let pixels = [255, 0, 0, 255, 0, 128, 255, 255];
        let image = ExportImage::from_rgba(2, 1, &pixels).unwrap();
        let (finished, completion) = std::sync::mpsc::channel();
        deliver(image, true, move |result| finished.send(result).unwrap());
        completion
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("clipboard worker must complete")
            .expect("clipboard image must be accepted");

        // A separate clipboard client reads after the delivery callback has
        // returned, exercising the persistent owner and real X11 selection.
        let mut observer = arboard::Clipboard::new().unwrap();
        let copied = observer.get_image().unwrap();
        assert_eq!((copied.width, copied.height), (2, 1));
        assert_eq!(copied.bytes.as_ref(), pixels);
        drop(observer);
        shutdown();
    }
}

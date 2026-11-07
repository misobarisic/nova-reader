//! Reader image encoding and delivery. Completion can run on a worker thread; UI callers
//! must return through Slint's event loop before updating UI state.

use crate::capture_settings::{CaptureFormat, CapturePreferences, ConflictPolicy};
use crate::image_storage::ImageStorageFormat;
use image::ImageEncoder;
use nova_reader_ui::backend_text as text;
use std::sync::Arc;

pub struct ExportImage {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub name: String,
    /// Something the person should hear after a successful delivery.
    pub notice: Option<String>,
    #[cfg(not(target_os = "android"))]
    clipboard_rgba: Option<image::RgbaImage>,
}

impl ExportImage {
    /// Preserve the original encoded page rather than saving its display tiles.
    pub fn original(bytes: Vec<u8>, name: &str) -> Result<Self, String> {
        reader_engine::inspect_image(&bytes, reader_engine::current_limits())
            .map_err(|error| format!("{}: {error}", text::tr("Cannot export page image")))?;
        let (mime, extension) = if reader_engine::is_jpeg_xl(&bytes) {
            ("image/jxl", "jxl")
        } else {
            match image::guess_format(&bytes).map_err(|error| error.to_string())? {
                image::ImageFormat::Png => ("image/png", "png"),
                image::ImageFormat::Jpeg => ("image/jpeg", "jpg"),
                image::ImageFormat::WebP => ("image/webp", "webp"),
                image::ImageFormat::Avif => ("image/avif", "avif"),
                _ => return Err(text::tr("Unsupported page image format")),
            }
        };
        let stem = file_stem(name);
        Ok(Self {
            bytes,
            mime: mime.into(),
            name: format!("{stem}.{extension}"),
            notice: None,
            #[cfg(not(target_os = "android"))]
            clipboard_rgba: None,
        })
    }

    /// Export a page. One stored as a single file keeps its original bytes. A
    /// page stored in another layout has no such file: its strips are put back
    /// together and encoded with the screenshot format and quality, or as PNG
    /// (with a notice) when that format cannot hold the page. The desktop
    /// clipboard receives the pixels themselves.
    pub fn from_page(
        page: &crate::page_store::PageImage,
        name: &str,
        preferences: &CapturePreferences,
        clipboard: bool,
    ) -> Result<Self, String> {
        reader_engine::with_current_limits(|| {
            let limits = reader_engine::current_limits();
            match page.export(limits)? {
                crate::page_store::Export::Original(bytes) => Self::original(bytes, name),
                crate::page_store::Export::Pixels(pixels) => {
                    Self::from_page_pixels(pixels, name, preferences, clipboard, limits)
                }
            }
        })
    }

    fn from_page_pixels(
        pixels: image::RgbaImage,
        name: &str,
        preferences: &CapturePreferences,
        clipboard: bool,
        limits: reader_engine::DecodeLimits,
    ) -> Result<Self, String> {
        let stem = file_stem(name);
        #[cfg(not(target_os = "android"))]
        if clipboard {
            return Ok(Self {
                bytes: Vec::new(),
                mime: "image/png".into(),
                name: format!("{stem}.png"),
                notice: None,
                clipboard_rgba: Some(pixels),
            });
        }
        #[cfg(target_os = "android")]
        let _ = clipboard;
        let (width, height) = pixels.dimensions();
        let (bytes, mime, extension, notice) =
            match encode_with_preferences(width, height, pixels.as_raw(), preferences, limits) {
                Ok((bytes, mime, extension)) => (bytes, mime, extension, None),
                Err(_) => (
                    encode_rgba(
                        width,
                        height,
                        pixels.as_raw(),
                        CaptureFormat::Png,
                        0,
                        limits,
                    )?,
                    "image/png",
                    "png",
                    Some(text::tr(
                        "The screenshot format cannot hold this page, so it was written as PNG.",
                    )),
                ),
            };
        Ok(Self {
            bytes,
            mime: mime.into(),
            name: format!("{stem}.{extension}"),
            notice,
            #[cfg(not(target_os = "android"))]
            clipboard_rgba: None,
        })
    }

    /// Encode an RGBA screenshot with a checked size before entering the PNG
    /// encoder. A screenshot can contain up to eight million physical pixels.
    #[cfg(test)]
    pub fn from_rgba(width: u32, height: u32, pixels: &[u8]) -> Result<Self, String> {
        Self::from_rgba_with_preferences(width, height, pixels, &CapturePreferences::default())
    }

    /// Desktop PNG clipboard ownership consumes the existing composed pixels.
    /// PNG is lossless, so its encode/decode round trip cannot change them.
    /// File delivery and other codecs retain their selected encoding semantics.
    pub fn from_rgba_for_delivery(
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        preferences: &CapturePreferences,
        clipboard: bool,
    ) -> Result<Self, String> {
        #[cfg(not(target_os = "android"))]
        if clipboard && preferences.format == CaptureFormat::Png {
            validate_screencap(width, height, pixels.len())?;
            return Ok(Self {
                bytes: Vec::new(),
                mime: "image/png".into(),
                name: "screencap.png".into(),
                notice: None,
                clipboard_rgba: Some(
                    image::RgbaImage::from_raw(width, height, pixels)
                        .ok_or_else(|| text::tr("Screenshot pixels do not match its dimensions"))?,
                ),
            });
        }
        #[cfg(target_os = "android")]
        let _ = clipboard;
        Self::from_rgba_with_preferences(width, height, &pixels, preferences)
    }

    pub fn from_rgba_with_preferences(
        width: u32,
        height: u32,
        pixels: &[u8],
        preferences: &CapturePreferences,
    ) -> Result<Self, String> {
        reader_engine::with_current_limits(|| {
            validate_screencap(width, height, pixels.len())?;
            let (bytes, mime, extension) = encode_with_preferences(
                width,
                height,
                pixels,
                preferences,
                reader_engine::current_limits(),
            )?;
            Ok(Self {
                bytes,
                mime: mime.into(),
                name: format!("screencap.{extension}"),
                notice: None,
                #[cfg(not(target_os = "android"))]
                clipboard_rgba: None,
            })
        })
    }
}

/// A safe file stem for an exported page: no separators or control characters.
fn file_stem(name: &str) -> String {
    std::path::Path::new(name)
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("page")
        .chars()
        .take(120)
        .map(|character| {
            if character.is_control() || r#"/\:*?"<>|"#.contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect()
}

/// Encode RGBA with the screenshot format and quality, then check that the
/// result is something this app can open again.
fn encode_with_preferences(
    width: u32,
    height: u32,
    pixels: &[u8],
    preferences: &CapturePreferences,
    limits: reader_engine::DecodeLimits,
) -> Result<(Vec<u8>, &'static str, &'static str), String> {
    let quality = match preferences.format {
        CaptureFormat::Jpeg => preferences.jpeg_quality,
        CaptureFormat::Avif => preferences.avif_quality,
        CaptureFormat::JxlLossless | CaptureFormat::JxlLossy => preferences.jxl_quality,
        _ => preferences.webp_quality,
    };
    let bytes = encode_rgba(width, height, pixels, preferences.format, quality, limits)?;
    if preferences.format == CaptureFormat::Avif {
        reader_engine::inspect_image(&bytes, limits).map_err(|error| {
            format!(
                "{}: {error}",
                text::tr("AVIF screenshot exceeds decode admission")
            )
        })?;
    }
    if matches!(
        preferences.format,
        CaptureFormat::JxlLossless | CaptureFormat::JxlLossy
    ) {
        // Clipboard decoding and later Reader imports use the same bounds.
        // Header admission plus an exact dimension check replaces a full
        // decode of the bytes this process just encoded (~17% of export).
        let inspected = reader_engine::inspect_image(&bytes, limits).map_err(|error| {
            format!(
                "{}: {error}",
                text::tr("JPEG XL screencap exceeds decode admission")
            )
        })?;
        if (inspected.width, inspected.height) != (width, height) {
            return Err(format!(
                "{}: {}x{}",
                text::tr("JPEG XL screencap exceeds decode admission"),
                inspected.width,
                inspected.height
            ));
        }
    }
    let (mime, extension) = match preferences.format {
        CaptureFormat::Png => ("image/png", "png"),
        CaptureFormat::Jpeg => ("image/jpeg", "jpg"),
        CaptureFormat::WebpLossless | CaptureFormat::WebpLossy => ("image/webp", "webp"),
        CaptureFormat::Avif => ("image/avif", "avif"),
        CaptureFormat::JxlLossless | CaptureFormat::JxlLossy => ("image/jxl", "jxl"),
    };
    Ok((bytes, mime, extension))
}

fn validate_screencap(width: u32, height: u32, bytes: usize) -> Result<(), String> {
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes > 0 && *bytes <= 8_000_000 * 4 && width <= 16384 && height <= 65536)
        .ok_or_else(|| text::tr("Screenshot dimensions exceed supported limits"))?;
    if expected != bytes as u64 {
        return Err(text::tr("Screenshot pixels do not match its dimensions"));
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn clipboard_pixels(image: &mut ExportImage) -> Result<image::RgbaImage, String> {
    if let Some(rgba) = image.clipboard_rgba.take() {
        return Ok(rgba);
    }
    reader_engine::decode_image(
        &image.bytes,
        reader_engine::DecodeLimits {
            // Locally generated PNGs can exceed a source page's encoded limit.
            max_encoded_bytes: 128 * 1024 * 1024,
            ..reader_engine::current_limits()
        },
    )
    .map_err(|error| format!("{}: {error}", text::tr("Cannot decode clipboard image")))
}

/// How `reencode_storage_image` would store a page, decided from its bytes.
/// Splitting a tall page into strips needs the same decision per strip.
pub(crate) enum StorageRecipe {
    /// Keep the source bytes: the codec cannot hold what they carry.
    Original,
    /// A lossless JPEG to JPEG XL transcode of the whole file.
    Transcoded(Vec<u8>),
    /// Decode, then encode pixels. `lossless` preserves a lossless input.
    Pixels { lossless: bool },
}

pub(crate) fn storage_recipe(
    bytes: &[u8],
    compression: crate::image_storage::ImageCompression,
    limits: reader_engine::DecodeLimits,
) -> Result<StorageRecipe, String> {
    let lossless = compression.lossless_inputs
        && matches!(
            compression.format,
            ImageStorageFormat::Webp
                | ImageStorageFormat::JxlLossless
                | ImageStorageFormat::JxlLossy
        )
        && nova_image::is_lossless_input(bytes);
    if lossless && nova_image::lossless_input_requires_original(bytes) {
        // The shared display decoder produces RGBA8. Keep high-depth source
        // samples intact until an encoder can preserve them.
        reader_engine::inspect_image(bytes, limits).map_err(|error| error.to_string())?;
        return Ok(StorageRecipe::Original);
    }
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
        return Ok(StorageRecipe::Transcoded(encoded));
    }
    Ok(StorageRecipe::Pixels { lossless })
}

/// Reencode only newly fetched storage bytes. Source dimensions are preserved;
/// callers choose their admission limits and retain existing stored files.
pub(crate) fn reencode_storage_image(
    bytes: &[u8],
    compression: crate::image_storage::ImageCompression,
    limits: reader_engine::DecodeLimits,
) -> Result<Vec<u8>, String> {
    match storage_recipe(bytes, compression, limits)? {
        StorageRecipe::Original => Ok(bytes.to_vec()),
        StorageRecipe::Transcoded(encoded) => Ok(encoded),
        StorageRecipe::Pixels { lossless } => {
            let image = reader_engine::decode_image(bytes, limits).map_err(|error| {
                format!(
                    "{}: {error}",
                    text::tr("Cannot decode image for compression")
                )
            })?;
            reencode_storage_pixels(image, compression, lossless, limits)
        }
    }
}

#[cfg(test)]
pub(crate) fn reencode_image(
    bytes: &[u8],
    format: ImageStorageFormat,
    quality: u8,
    limits: reader_engine::DecodeLimits,
) -> Result<Vec<u8>, String> {
    let image = reader_engine::decode_image(bytes, limits).map_err(|error| {
        format!(
            "{}: {error}",
            text::tr("Cannot decode image for compression")
        )
    })?;
    encode_storage_pixels(image, format, quality, limits, false)
}

/// Encode decoded pixels (a whole page, or one strip of it) with a store's
/// format and quality. This is the single pixel-to-storage path.
pub(crate) fn reencode_storage_pixels(
    image: image::RgbaImage,
    compression: crate::image_storage::ImageCompression,
    lossless: bool,
    limits: reader_engine::DecodeLimits,
) -> Result<Vec<u8>, String> {
    encode_storage_pixels(
        image,
        compression.format,
        compression.quality,
        limits,
        lossless,
    )
}

fn encode_storage_pixels(
    image: image::RgbaImage,
    format: ImageStorageFormat,
    quality: u8,
    limits: reader_engine::DecodeLimits,
    lossless: bool,
) -> Result<Vec<u8>, String> {
    // Keep transparent artwork readable: callers retain the original when
    // JPEG cannot represent it. Screenshot exports use Nova's separate rules.
    if format == ImageStorageFormat::Jpeg && image.pixels().any(|pixel| pixel[3] != 255) {
        return Err(text::tr(
            "JPEG compression cannot preserve image transparency",
        ));
    }
    let encoded = match format {
        ImageStorageFormat::JxlLossless | ImageStorageFormat::JxlLossy => nova_image::encode_jxl(
            image.width(),
            image.height(),
            image.as_raw(),
            if lossless || format == ImageStorageFormat::JxlLossless {
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
        ImageStorageFormat::Webp if lossless => {
            if image.width() > 16383 || image.height() > 16383 {
                return Err(text::tr(
                    "WebP supports image dimensions up to 16383 pixels",
                ));
            }
            let mut config =
                webp::WebPConfig::new().map_err(|_| text::tr("Cannot configure WebP encoder"))?;
            config.lossless = 1;
            config.alpha_compression = 0;
            // Keep RGB beneath transparent pixels; the basic encoder permits
            // discarding it. Fixed effort makes this independent of quality.
            config.exact = 1;
            config.quality = 75.;
            webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height())
                .encode_advanced(&config)
                .map_err(|error| format!("{}: {error:?}", text::tr("Cannot encode image")))?
                .to_vec()
        }
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
        return Err(text::tr("Compressed image exceeds encoded byte limit"));
    }
    drop(image);
    if matches!(
        format,
        ImageStorageFormat::JxlLossless | ImageStorageFormat::JxlLossy
    ) {
        nova_image::decode_jxl(&encoded, limits).map_err(|error| {
            format!(
                "{}: {error}",
                text::tr("Compressed image exceeds decode admission")
            )
        })?;
    } else {
        reader_engine::inspect_image(&encoded, limits).map_err(|error| {
            format!(
                "{}: {error}",
                text::tr("Compressed image exceeds decode admission")
            )
        })?;
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
                .map_err(|error| format!("{}: {error}", text::tr("Cannot encode image")))?;
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
                .map_err(|error| format!("{}: {error}", text::tr("Cannot encode image")))?;
        }
        CaptureFormat::WebpLossless | CaptureFormat::WebpLossy => {
            if width > 16383 || height > 16383 {
                return Err(text::tr(
                    "WebP supports image dimensions up to 16383 pixels",
                ));
            }
            bytes = webp::Encoder::from_rgba(pixels, width, height)
                .encode_simple(format == CaptureFormat::WebpLossless, f32::from(quality))
                .map_err(|error| format!("{}: {error:?}", text::tr("Cannot encode image")))?
                .to_vec();
        }
        CaptureFormat::JxlLossless | CaptureFormat::JxlLossy => {
            let encoding = if format == CaptureFormat::JxlLossless {
                nova_image::JxlEncoding::Lossless
            } else {
                nova_image::JxlEncoding::Lossy { quality }
            };
            bytes = nova_image::encode_jxl(width, height, pixels, encoding, limits)
                .map_err(|error| format!("{}: {error}", text::tr("Cannot encode JPEG XL image")))?;
        }
        CaptureFormat::Avif => {
            bytes = nova_image::encode_avif(width, height, pixels, quality, limits)
                .map_err(|error| format!("{}: {error}", text::tr("Cannot encode AVIF image")))?;
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
    if !(context.current)() {
        callback(Err(text::tr("Image action cancelled")));
        return;
    }
    if !matches!(
        image.mime.as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/avif" | "image/jxl"
    ) {
        callback(Err(text::tr("Unsupported image export format")));
        return;
    }
    #[cfg(not(target_os = "android"))]
    if !clipboard && image.clipboard_rgba.is_some() {
        callback(Err(text::tr(
            "Clipboard pixels cannot be saved without encoding",
        )));
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
        return Err(text::tr("Invalid image filename"));
    }
    let destination = directory.join(filename);
    if !(context.current)() {
        return Err(text::tr("Image action cancelled"));
    }
    std::fs::create_dir_all(&directory)
        .map_err(|e| format!("{}: {e}", text::tr("Cannot create image folder")))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".nova-reader-image-")
        .tempfile_in(&directory)
        .map_err(|e| format!("{}: {e}", text::tr("Cannot stage saved image")))?;
    staged
        .write_all(&image.bytes)
        .and_then(|_| staged.flush())
        .and_then(|_| staged.as_file().sync_all())
        .map_err(|e| format!("{}: {e}", text::tr("Cannot write saved image")))?;
    let mut path = destination.clone();
    let mut number = 0u32;
    loop {
        if !(context.current)() {
            return Err(text::tr("Image action cancelled"));
        }
        match staged.persist_noclobber(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                staged = error.file
            }
            Err(error) => {
                return Err(format!(
                    "{}: {}",
                    text::tr("Cannot publish saved image"),
                    error.error
                ));
            }
        }
        if number == 0 {
            let choice = match context.policy {
                ConflictPolicy::Overwrite => ConflictChoice::Overwrite,
                ConflictPolicy::DoNothing => {
                    return Err(text::tr("Image already exists; nothing saved."));
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
                ConflictChoice::Cancel => return Err(text::tr("Image action cancelled")),
                ConflictChoice::Overwrite => {
                    if !(context.current)() {
                        return Err(text::tr("Image action cancelled"));
                    }
                    // tempfile persist replaces atomically, including on Windows.
                    return staged
                        .persist(&destination)
                        .map(|_| destination)
                        .map_err(|e| {
                            format!("{}: {}", text::tr("Cannot replace saved image"), e.error)
                        });
                }
                ConflictChoice::AddNumber => {}
            }
        }
        number = number
            .checked_add(1)
            .filter(|n| *n <= 10000)
            .ok_or_else(|| text::tr("Cannot allocate an image filename"))?;
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
    use nova_reader_ui::backend_text as text;
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
            complete(callback, Err(text::tr("Another image export is active")));
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
                    while let Ok(mut job) = receiver.recv() {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            if job.clipboard {
                                copy(&mut job.image, &job.context, &mut clipboard)
                            } else {
                                save(&job.image, &job.context)
                            }
                        }))
                        .unwrap_or_else(|_| Err(text::tr("Image export worker failed")));
                        drop(job.image);
                        complete(job.callback, result);
                    }
                    drop(clipboard);
                    let _ = finished.send(());
                }) {
                Ok(thread) => thread,
                Err(error) => {
                    drop(worker);
                    complete(
                        callback,
                        Err(format!(
                            "{}: {error}",
                            text::tr("Cannot start image export")
                        )),
                    );
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
                mpsc::TrySendError::Full(job) => (job, text::tr("Another image export is active")),
                mpsc::TrySendError::Disconnected(job) => {
                    (job, text::tr("Image export worker stopped"))
                }
            };
            complete(job.callback, Err(message));
        }
    }

    fn save(image: &ExportImage, context: &SaveContext) -> Result<(), String> {
        let directories = directories::UserDirs::new()
            .ok_or_else(|| text::tr("Pictures directory is unavailable"))?;
        let pictures = directories
            .picture_dir()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| directories.home_dir().join("Pictures"));
        super::publish_file(&pictures.join("Nova Reader"), image, context).map(|_| ())
    }

    fn copy(
        image: &mut ExportImage,
        context: &SaveContext,
        clipboard: &mut Option<arboard::Clipboard>,
    ) -> Result<(), String> {
        if !(context.current)() {
            return Err(text::tr("Image action cancelled"));
        }
        let rgba = super::clipboard_pixels(image)?;
        if clipboard.is_none() {
            *clipboard = Some(arboard::Clipboard::new().map_err(|error| {
                format!("{}: {error}", text::tr("Cannot access image clipboard"))
            })?);
        }
        if !(context.current)() {
            return Err(text::tr("Image action cancelled"));
        }
        clipboard
            .as_mut()
            .unwrap()
            .set_image(arboard::ImageData {
                width: rgba.width() as usize,
                height: rgba.height() as usize,
                bytes: std::borrow::Cow::Owned(rgba.into_raw()),
            })
            .map_err(|error| format!("{}: {error}", text::tr("Cannot copy image")))
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
    use nova_reader_ui::backend_text as text;
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
            callback(Err(text::tr("Another image export is active")));
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
                if !(context.current)() {finish(completion,Err(text::tr("Image action cancelled")));return;}
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
                                .map_err(|error| format!("{}: {error}", text::tr("Cannot clean staged image")))?;
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
                        format!("{}: {error}", text::tr("Cannot stage image"))
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
                if !(context.current)() {let _=std::fs::remove_file(&cleanup);finish(completion,Err(text::tr("Image action cancelled")));return;}
                let finished = move |result| {
                    let _ = std::fs::remove_file(cleanup);
                    finish(completion, result);
                };
                if clipboard {
                    crate::android_documents::copy_image_current(path, &image.mime, context.current.clone(), finished);
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
                Err(format!(
                    "{}: {error}",
                    text::tr("Cannot start image export")
                )),
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

    #[cfg(not(target_os = "android"))]
    #[test]
    fn png_screencap_clipboard_moves_pixels_without_encoding_and_preserves_alpha() {
        let pixels = vec![
            17, 31, 49, 0, 120, 77, 33, 128, 2, 8, 16, 255, 255, 255, 255, 1,
        ];
        let expected = pixels.clone();
        let original_allocation = pixels.as_ptr();
        let preferences = CapturePreferences::default();
        let mut copy =
            ExportImage::from_rgba_for_delivery(2, 2, pixels, &preferences, true).unwrap();
        assert!(
            copy.bytes.is_empty(),
            "PNG clipboard must not invoke the encoder"
        );
        let rgba = clipboard_pixels(&mut copy).unwrap();
        assert_eq!(
            rgba.as_raw().as_ptr(),
            original_allocation,
            "handoff moves the composed allocation"
        );
        assert_eq!(rgba.as_raw(), &expected);
        let mut save =
            ExportImage::from_rgba_for_delivery(2, 2, expected.clone(), &preferences, false)
                .unwrap();
        assert!(!save.bytes.is_empty(), "Save must still produce a PNG file");
        assert_eq!(
            clipboard_pixels(&mut save).unwrap(),
            rgba,
            "exact parity with PNG roundtrip, including transparent RGB"
        );
        let mut jpeg = ExportImage::from_rgba_for_delivery(
            2,
            2,
            expected,
            &CapturePreferences {
                format: CaptureFormat::Jpeg,
                ..preferences
            },
            true,
        )
        .unwrap();
        assert!(!jpeg.bytes.is_empty());
        assert_eq!(jpeg.mime, "image/jpeg");
        assert!(
            clipboard_pixels(&mut jpeg)
                .unwrap()
                .pixels()
                .all(|pixel| pixel[3] == 255)
        );
        assert!(
            ExportImage::from_rgba_for_delivery(
                2,
                2,
                vec![0; 15],
                &CapturePreferences::default(),
                true
            )
            .is_err()
        );
        assert!(
            ExportImage::from_rgba_for_delivery(
                8001,
                1000,
                vec![],
                &CapturePreferences::default(),
                true
            )
            .is_err()
        );
    }

    #[test]
    fn cancelled_selection_never_enters_save_or_clipboard_delivery() {
        let revision = reader_capture::selection::Revision::default();
        for clipboard in [false, true] {
            let ticket = revision.advance();
            let current: ExportCurrent = Arc::new(move || ticket.current());
            let context = SaveContext::new(
                "cancelled.png".into(),
                "",
                &CapturePreferences::default(),
                Arc::new(|_, _, _| panic!("cancelled selection cannot prompt")),
                current,
            );
            revision.advance();
            let (sender, receiver) = std::sync::mpsc::channel();
            deliver_with_context(
                ExportImage::from_rgba(1, 1, &[1, 2, 3, 255]).unwrap(),
                clipboard,
                context,
                move |result| sender.send(result).unwrap(),
            );
            assert!(
                receiver
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
                    .unwrap_err()
                    .contains("cancelled")
            );
        }
    }

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
    fn jpeg_xl_captures_preserve_alpha_and_lossless_pixels() {
        let pixels = [
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 17, 31, 47, 255,
        ];
        for format in [CaptureFormat::JxlLossless, CaptureFormat::JxlLossy] {
            let preferences = CapturePreferences {
                format,
                jxl_quality: 90,
                ..Default::default()
            };
            let capture =
                ExportImage::from_rgba_with_preferences(2, 2, &pixels, &preferences).unwrap();
            assert_eq!(capture.mime, "image/jxl");
            assert_eq!(capture.name, "screencap.jxl");
            let decoded =
                reader_engine::decode_image(&capture.bytes, reader_engine::DecodeLimits::default())
                    .unwrap();
            assert_eq!(decoded.dimensions(), (2, 2));
            assert!(
                decoded
                    .as_raw()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(pixels.as_chunks::<4>().0.iter())
                    .all(|(a, b)| a[3] == b[3])
            );
            if format == CaptureFormat::JxlLossless {
                assert_eq!(decoded.as_raw(), &pixels);
            }
        }
    }

    #[test]
    fn jpeg_xl_export_cost_and_dimension_check() {
        let (width, height) = (1080u32, 2340u32);
        let pixels = (0..width * height)
            .flat_map(|i| {
                let (x, y) = (i % width, i / width);
                let line = x.is_multiple_of(7) || y.is_multiple_of(11);
                let tone = if line { 20 } else { 235 - (y / 40) as u8 };
                [tone, tone, tone.wrapping_add((x / 9) as u8), 255]
            })
            .collect::<Vec<_>>();
        for format in [CaptureFormat::JxlLossless, CaptureFormat::JxlLossy] {
            let preferences = CapturePreferences {
                format,
                jxl_quality: 90,
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let capture =
                ExportImage::from_rgba_with_preferences(width, height, &pixels, &preferences)
                    .unwrap();
            eprintln!(
                "jxl_export format={format:?} total_ms={:.1} bytes={}",
                started.elapsed().as_secs_f64() * 1000.,
                capture.bytes.len()
            );
            let limits = reader_engine::current_limits();
            let started = std::time::Instant::now();
            let inspected = reader_engine::inspect_image(&capture.bytes, limits).unwrap();
            eprintln!(
                "  inspect_ms={:.2}",
                started.elapsed().as_secs_f64() * 1000.
            );
            assert_eq!((inspected.width, inspected.height), (width, height));
            let started = std::time::Instant::now();
            let decoded = reader_engine::decode_image(&capture.bytes, limits).unwrap();
            eprintln!(
                "  full_decode_ms={:.1}",
                started.elapsed().as_secs_f64() * 1000.
            );
            assert_eq!(decoded.dimensions(), (width, height));
        }
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
            (CaptureFormat::JxlLossless, "image/jxl", "jxl"),
            (CaptureFormat::JxlLossy, "image/jxl", "jxl"),
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
            if matches!(
                format,
                CaptureFormat::Png | CaptureFormat::WebpLossless | CaptureFormat::JxlLossless
            ) {
                assert_eq!(decoded.as_raw(), &pixels);
            }
        }
        for format in [
            CaptureFormat::Jpeg,
            CaptureFormat::WebpLossy,
            CaptureFormat::Avif,
            CaptureFormat::JxlLossy,
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
                        jxl_quality: quality,
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
        for format in [
            CaptureFormat::Png,
            CaptureFormat::Avif,
            CaptureFormat::JxlLossless,
            CaptureFormat::JxlLossy,
        ] {
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
    fn lossless_input_policy_preserves_png_and_webp_pixels_and_keeps_jpeg_quality() {
        let pixels: Vec<u8> = (0..32 * 24)
            .flat_map(|i| {
                [
                    (i * 17) as u8,
                    (i * 7) as u8,
                    (i * 31) as u8,
                    if i % 7 == 0 { 0 } else { 255 },
                ]
            })
            .collect();
        let image = image::RgbaImage::from_raw(32, 24, pixels.clone()).unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let png = png.into_inner();
        let limits = reader_engine::DecodeLimits::default();
        let webp = reencode_storage_image(
            &png,
            ImageCompression {
                lossless_inputs: true,
                ..ImageCompression::default()
            },
            limits,
        )
        .unwrap();
        for source in [&png, &webp] {
            for format in [ImageStorageFormat::Webp, ImageStorageFormat::JxlLossy] {
                let compression = ImageCompression {
                    format,
                    quality: 20,
                    lossless_inputs: true,
                    ..ImageCompression::default()
                };
                let low = reencode_storage_image(source, compression, limits).unwrap();
                let high = reencode_storage_image(
                    source,
                    ImageCompression {
                        quality: 95,
                        ..compression
                    },
                    limits,
                )
                .unwrap();
                assert_eq!(
                    low, high,
                    "lossless input must ignore quality for {format:?}"
                );
                assert_eq!(
                    reader_engine::decode_image(&low, limits).unwrap().as_raw(),
                    &pixels
                );
            }
        }
        let rgb = image::DynamicImage::ImageRgba8(image).to_rgb8();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
            .encode(rgb.as_raw(), 32, 24, image::ExtendedColorType::Rgb8)
            .unwrap();
        let lossy_webp =
            reencode_storage_image(&jpeg, ImageCompression::default(), limits).unwrap();
        let unrecognized_jxl =
            nova_image::encode_jxl(32, 24, &pixels, nova_image::JxlEncoding::Lossless, limits)
                .unwrap();
        for source in [&jpeg, &lossy_webp, &unrecognized_jxl] {
            for format in [ImageStorageFormat::Webp, ImageStorageFormat::JxlLossy] {
                let compression = ImageCompression {
                    format,
                    quality: 20,
                    lossless_inputs: true,
                    ..ImageCompression::default()
                };
                assert_ne!(
                    reencode_storage_image(source, compression, limits).unwrap(),
                    reencode_storage_image(
                        source,
                        ImageCompression {
                            quality: 95,
                            ..compression
                        },
                        limits
                    )
                    .unwrap(),
                    "lossy or unrecognized source must still use quality for {format:?}"
                );
            }
        }
    }

    #[test]
    fn lossless_input_policy_retains_high_depth_png_originals() {
        let image = image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(
            2,
            1,
            vec![1, 255, 1023, 65535, 17, 40000, 65534, 65535],
        )
        .unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba16(image)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        for format in [ImageStorageFormat::Webp, ImageStorageFormat::JxlLossy] {
            assert_eq!(
                reencode_storage_image(
                    &png,
                    ImageCompression {
                        format,
                        lossless_inputs: true,
                        ..ImageCompression::default()
                    },
                    reader_engine::DecodeLimits::default()
                )
                .unwrap(),
                png
            );
        }
    }

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

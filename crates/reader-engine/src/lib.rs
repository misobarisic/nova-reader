//! Bounded CPU image decode and renderer-independent logical geometry.
use image::ImageDecoder;
mod avif;
mod display;
mod jxl;
mod source;
pub use display::{
    DISPLAY_TILE_ROWS, DISPLAY_UPLOAD_ROWS, DISPLAY_WINDOW_BYTES, DisplayDecoder, DisplayOptions,
    DisplayWindow, DisplayWindowPlan, PagePresentation, PageRotation, WindowRenderer,
    decode_display_image, decode_display_window, filter_pixel, page_presentation,
    plan_display_window,
};
pub use nova_image::is_jpeg_xl;
pub use nova_image::{DecodeLimits, PageDimensions};
use reader_domain::{ReadingMode, ResumeAnchor};
pub use source::{PageSource, SharedPage};
use std::{
    collections::{BTreeMap, VecDeque},
    io::Cursor,
    ops::Range,
};

// Device policy is supplied by the application; Default remains the codec baseline.
static CURRENT_LIMITS: std::sync::LazyLock<std::sync::RwLock<(DecodeLimits, u64)>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new((DecodeLimits::default(), 0)));
thread_local! {
    static OPERATION_LIMITS: std::cell::Cell<Option<(DecodeLimits, u64)>> = const { std::cell::Cell::new(None) };
}
fn limits_snapshot() -> (DecodeLimits, u64) {
    OPERATION_LIMITS
        .with(|local| local.get())
        .unwrap_or_else(|| {
            *CURRENT_LIMITS
                .read()
                .unwrap_or_else(|error| error.into_inner())
        })
}
/// A consistent policy snapshot for a complete synchronous worker operation.
pub fn current_limits() -> DecodeLimits {
    limits_snapshot().0
}
/// Revision used by this operation, for guarding asynchronous publication.
pub fn current_limits_revision() -> u64 {
    limits_snapshot().1
}
/// Latest device policy revision, even inside an older operation snapshot.
pub fn limits_revision() -> u64 {
    CURRENT_LIMITS
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .1
}
pub fn set_limits(limits: DecodeLimits) {
    let mut current = CURRENT_LIMITS
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if current.0 != limits {
        *current = (limits, current.1.wrapping_add(1));
    }
}
/// Nested operations inherit their caller's snapshot, including fetch/compression.
pub fn with_current_limits<T>(operation: impl FnOnce() -> T) -> T {
    struct Reset(Option<(DecodeLimits, u64)>);
    impl Drop for Reset {
        fn drop(&mut self) {
            OPERATION_LIMITS.with(|local| local.set(self.0));
        }
    }
    let snapshot = limits_snapshot();
    let _reset = Reset(OPERATION_LIMITS.with(|local| local.replace(Some(snapshot))));
    operation()
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("image exceeds supported limits")]
    Limit,
    #[error("invalid layout dimensions")]
    Layout,
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error("AVIF: {0}")]
    Avif(String),
    #[error("JPEG XL: {0}")]
    Jxl(String),
}
pub type Result<T> = std::result::Result<T, Error>;
impl From<nova_image::Error> for Error {
    fn from(error: nova_image::Error) -> Self {
        match error {
            nova_image::Error::Limit => Self::Limit,
            nova_image::Error::Avif(message) => Self::Avif(message),
            nova_image::Error::Jxl(message) => Self::Jxl(message),
            nova_image::Error::JxlUnsupported(message) => Self::Jxl(message.into()),
        }
    }
}
fn reader(bytes: &[u8], limits: DecodeLimits) -> Result<image::ImageReader<Cursor<&[u8]>>> {
    if bytes.len() > limits.max_encoded_bytes {
        return Err(Error::Limit);
    }
    let format = image::guess_format(bytes)?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
    ) {
        return Err(Error::Limit);
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut bounds = image::Limits::default();
    bounds.max_image_width = Some(limits.max_width);
    bounds.max_image_height = Some(limits.max_height);
    bounds.max_alloc = Some(limits.max_decoded_bytes);
    reader.limits(bounds);
    Ok(reader)
}
pub fn inspect_image(bytes: &[u8], limits: DecodeLimits) -> Result<PageDimensions> {
    if is_jpeg_xl(bytes) {
        return jxl::inspect(bytes, limits);
    }
    if image::guess_format(bytes).is_ok_and(|format| format == image::ImageFormat::Avif) {
        return avif::inspect(bytes, limits);
    }
    let (width, height) = reader(bytes, limits)?.into_dimensions()?;
    admit_dimensions(PageDimensions { width, height }, limits)
}
/// The dimension rules every page obeys, however it is stored: nonzero, within
/// the width/height/pixel caps, and with an RGBA decode that fits the budget.
pub fn admit_dimensions(
    dimensions: PageDimensions,
    limits: DecodeLimits,
) -> Result<PageDimensions> {
    let PageDimensions { width, height } = dimensions;
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > limits.max_width
        || height > limits.max_height
        || pixels > limits.max_pixels
        || pixels
            .checked_mul(4)
            .is_none_or(|b| b > limits.max_decoded_bytes)
    {
        return Err(Error::Limit);
    }
    Ok(dimensions)
}
/// Full decode is admitted only after dimension probing. Images larger than the
/// supported budget fail explicitly; post-decode tiling is never used to claim
/// a reduction in peak allocation.
pub fn decode_image(bytes: &[u8], limits: DecodeLimits) -> Result<image::RgbaImage> {
    if is_jpeg_xl(bytes) {
        return jxl::decode(bytes, limits);
    }
    if image::guess_format(bytes).is_ok_and(|format| format == image::ImageFormat::Avif) {
        return avif::decode(bytes, limits);
    }
    let dimensions = inspect_image(bytes, limits)?;
    let decoder = reader(bytes, limits)?.into_decoder()?;
    let rgba_bytes = u64::from(dimensions.width) * u64::from(dimensions.height) * 4;
    let color = decoder.color_type();
    if matches!(
        color,
        image::ColorType::L8
            | image::ColorType::La8
            | image::ColorType::Rgb8
            | image::ColorType::Rgba8
    ) {
        // Decode into the native prefix of the final RGBA allocation, then
        // expand backwards. JPEG's RGB→RGBA conversion does not need a second
        // full-size image: 4096×6144 now needs96MiB rather than168MiB.
        let native = usize::try_from(decoder.total_bytes()).map_err(|_| Error::Limit)?;
        let count = usize::try_from(rgba_bytes).map_err(|_| Error::Limit)?;
        if native > count {
            return Err(Error::Limit);
        }
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(count).map_err(|_| Error::Limit)?;
        pixels.resize(count, 0);
        decoder.read_image(&mut pixels[..native])?;
        if color != image::ColorType::Rgba8 {
            let channels = usize::from(color.channel_count());
            for index in (0..count / 4).rev() {
                let at = index * channels;
                let rgba = match color {
                    image::ColorType::Rgb8 => [pixels[at], pixels[at + 1], pixels[at + 2], 255],
                    image::ColorType::La8 => [pixels[at], pixels[at], pixels[at], pixels[at + 1]],
                    _ => [pixels[at], pixels[at], pixels[at], 255],
                };
                pixels[index * 4..index * 4 + 4].copy_from_slice(&rgba);
            }
        }
        return image::RgbaImage::from_raw(dimensions.width, dimensions.height, pixels)
            .ok_or(Error::Limit);
    }
    // Reserve both the decoder's native output and RGBA conversion at once.
    if decoder
        .total_bytes()
        .checked_add(rgba_bytes)
        .is_none_or(|n| n > limits.max_decoded_bytes)
    {
        return Err(Error::Limit);
    }
    Ok(image::DynamicImage::from_decoder(decoder)?.into_rgba8())
}
#[derive(Clone, Debug)]
pub struct PageLayout {
    pub index: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Clone, Debug)]
pub struct Layout {
    pub pages: Vec<PageLayout>,
    pub width: f64,
    pub height: f64,
    pub mode: ReadingMode,
}
pub fn layout_pages(
    pages: &[PageDimensions],
    mode: ReadingMode,
    viewport_width: f64,
    viewport_height: f64,
    gap: f64,
) -> Result<Layout> {
    if !viewport_width.is_finite()
        || !viewport_height.is_finite()
        || !gap.is_finite()
        || viewport_width <= 0.0
        || viewport_height <= 0.0
        || gap < 0.0
        || pages.iter().any(|p| p.width == 0 || p.height == 0)
    {
        return Err(Error::Layout);
    }
    let mut result = Layout {
        pages: Vec::with_capacity(pages.len()),
        width: viewport_width,
        height: viewport_height,
        mode,
    };
    let mut cursor = 0.0;
    for (index, p) in pages.iter().enumerate() {
        let scale = match mode {
            ReadingMode::Webtoon => viewport_width / f64::from(p.width),
            _ => (viewport_width / f64::from(p.width)).min(viewport_height / f64::from(p.height)),
        };
        let width = f64::from(p.width) * scale;
        let height = f64::from(p.height) * scale;
        let (x, y) = match mode {
            ReadingMode::Webtoon => ((viewport_width - width) / 2.0, cursor),
            ReadingMode::Vertical => (
                (viewport_width - width) / 2.0,
                cursor + (viewport_height - height) / 2.0,
            ),
            _ => (
                cursor + (viewport_width - width) / 2.0,
                (viewport_height - height) / 2.0,
            ),
        };
        result.pages.push(PageLayout {
            index,
            x,
            y,
            width,
            height,
        });
        cursor += match mode {
            ReadingMode::Webtoon => height + gap,
            ReadingMode::Vertical => viewport_height + gap,
            _ => viewport_width + gap,
        };
    }
    match mode {
        ReadingMode::Webtoon | ReadingMode::Vertical => {
            result.height = (cursor - gap).max(viewport_height)
        }
        _ => result.width = (cursor - gap).max(viewport_width),
    }
    if mode == ReadingMode::RightToLeft {
        for p in &mut result.pages {
            p.x = result.width - p.x - p.width;
        }
    }
    Ok(result)
}
impl Layout {
    pub fn anchor_position(&self, anchor: &ResumeAnchor) -> Option<(f64, f64)> {
        self.pages
            .get(anchor.page_index)
            .map(|p| (p.x, p.y + p.height * anchor.offset()))
    }
    pub fn visible(&self, x: f64, y: f64, width: f64, height: f64) -> Vec<usize> {
        self.pages
            .iter()
            .filter(|p| {
                p.x < x + width && p.x + p.width > x && p.y < y + height && p.y + p.height > y
            })
            .map(|p| p.index)
            .collect()
    }
}
/// A page list update resolves by stable page ID first. A removed page falls
/// back to its previous logical index, clamped to the available list.
pub fn resolve_anchor(anchor: &ResumeAnchor, page_ids: &[String]) -> Option<ResumeAnchor> {
    if page_ids.is_empty() {
        return None;
    }
    let index = page_ids
        .iter()
        .position(|id| id == &anchor.page_id)
        .unwrap_or(anchor.page_index.min(page_ids.len() - 1));
    Some(ResumeAnchor {
        page_id: page_ids[index].clone(),
        page_index: index,
        offset_millionths: anchor.offset_millionths.min(1_000_000),
    })
}
pub fn prefetch_window(current: usize, count: usize, before: usize, after: usize) -> Range<usize> {
    current.saturating_sub(before).min(count)
        ..current.saturating_add(after).saturating_add(1).min(count)
}
/// Byte-accounted LRU. Pinned/visible pages should stay with the caller; this
/// cache owns only evictable decoded images, separate from downloaded originals.
pub struct ImageCache {
    budget: usize,
    used: usize,
    order: VecDeque<String>,
    images: BTreeMap<String, image::RgbaImage>,
}
impl ImageCache {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            order: VecDeque::new(),
            images: BTreeMap::new(),
        }
    }
    pub fn used_bytes(&self) -> usize {
        self.used
    }
    pub fn insert(&mut self, key: String, image: image::RgbaImage) -> bool {
        let bytes = image.as_raw().len();
        if bytes > self.budget {
            return false;
        }
        self.remove(&key);
        while self.used + bytes > self.budget {
            if let Some(old) = self.order.pop_front() {
                self.remove(&old);
            } else {
                break;
            }
        }
        self.used += bytes;
        self.order.push_back(key.clone());
        self.images.insert(key, image);
        true
    }
    fn remove(&mut self, key: &str) {
        if let Some(old) = self.images.remove(key) {
            self.used -= old.as_raw().len();
        }
        self.order.retain(|k| k != key);
    }
    pub fn get(&mut self, key: &str) -> Option<&image::RgbaImage> {
        if self.images.contains_key(key) {
            self.order.retain(|k| k != key);
            self.order.push_back(key.into());
        }
        self.images.get(key)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geometry_and_stable_resume() {
        let pages = [
            PageDimensions {
                width: 100,
                height: 1000,
            },
            PageDimensions {
                width: 200,
                height: 100,
            },
        ];
        let layout = layout_pages(&pages, ReadingMode::Webtoon, 200.0, 400.0, 0.0).unwrap();
        let anchor = ResumeAnchor::new("first", 0, 0.25).unwrap();
        assert_eq!(layout.anchor_position(&anchor), Some((0.0, 500.0)));
        let rtl = layout_pages(&pages, ReadingMode::RightToLeft, 200.0, 400.0, 0.0).unwrap();
        assert!(rtl.pages[0].x > rtl.pages[1].x);
        assert_eq!(
            resolve_anchor(&anchor, &["new".into(), "first".into()])
                .unwrap()
                .page_index,
            1
        );
    }
    #[test]
    fn memory_admission_and_eviction() {
        let mut cache = ImageCache::new(16);
        assert!(cache.insert("a".into(), image::RgbaImage::new(2, 2)));
        assert!(cache.insert("b".into(), image::RgbaImage::new(2, 2)));
        assert!(cache.get("a").is_none());
        assert_eq!(cache.used_bytes(), 16);
        let image = image::DynamicImage::new_rgba8(4, 4);
        let mut data = Cursor::new(Vec::new());
        image.write_to(&mut data, image::ImageFormat::Png).unwrap();
        assert!(
            inspect_image(
                data.get_ref(),
                DecodeLimits {
                    max_pixels: 15,
                    ..DecodeLimits::default()
                }
            )
            .is_err()
        );
        assert_eq!(
            decode_image(data.get_ref(), DecodeLimits::default())
                .unwrap()
                .width(),
            4
        );
        let display = decode_display_image(data.get_ref(), DecodeLimits::default(), 2, 2).unwrap();
        assert_eq!(display.dimensions(), (2, 2));
        assert!(decode_display_image(data.get_ref(), DecodeLimits::default(), 0, 2).is_err());
    }
}

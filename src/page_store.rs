//! The page boundary: every stored page is read, measured, decoded and
//! exported through [`PageImage`], and every stored page is produced by
//! [`storage_form`]. A tall page may be stored as independently encoded row
//! strips, but that is invisible past this module. The container is private to
//! `page_store/strips.rs`; callers see only a page with dimensions that decodes
//! rows, so counters, anchors, ratios, captures and thumbnails behave exactly
//! as they do for a whole image.
mod strips;

use crate::{
    image_storage::ImageCompression,
    reader_export::{self, StorageRecipe},
};
use reader_engine::{DecodeLimits, PageDimensions, PageSource, SharedPage};
use std::{
    ops::Range,
    sync::{Arc, Weak},
};

type Result<T> = reader_engine::Result<T>;

enum Stored {
    Whole(Vec<u8>),
    Strips(strips::Container),
}

impl PageSource for Stored {
    fn dimensions(&self, limits: DecodeLimits) -> Result<PageDimensions> {
        match self {
            Self::Whole(bytes) => bytes.dimensions(limits),
            // Admitted by the very rule a whole image obeys, so storing a page
            // as strips can neither admit nor reject anything new.
            Self::Strips(container) => reader_engine::admit_dimensions(container.size(), limits),
        }
    }
    fn encoded_len(&self) -> usize {
        match self {
            Self::Whole(bytes) => bytes.len(),
            Self::Strips(container) => container.encoded_len(),
        }
    }
    fn row_granularity(&self) -> Option<u32> {
        match self {
            Self::Whole(_) => None,
            Self::Strips(container) => Some(container.strip_rows()),
        }
    }
    fn decode_rows(&self, rows: Range<u32>, limits: DecodeLimits) -> Result<image::RgbaImage> {
        match self {
            Self::Whole(bytes) => bytes.decode_rows(rows, limits),
            Self::Strips(container) => {
                self.dimensions(limits)?;
                container.decode_rows(rows, limits)
            }
        }
    }
    fn decode(&self, limits: DecodeLimits) -> Result<image::RgbaImage> {
        match self {
            Self::Whole(bytes) => bytes.decode(limits),
            Self::Strips(container) => {
                self.dimensions(limits)?;
                container.decode(limits)
            }
        }
    }
}

/// One page as stored or fetched. Cloning shares the allocation, and that
/// allocation is the page's identity for decode reuse and capture provenance.
#[derive(Clone)]
pub struct PageImage(Arc<Stored>);

/// A non-owning reference that never extends a page's lifetime.
#[derive(Clone)]
pub struct WeakPage(Weak<Stored>);

impl SharedPage for PageImage {
    fn shared(&self) -> Arc<dyn PageSource> {
        self.0.clone()
    }
    fn page(&self) -> &dyn PageSource {
        &*self.0
    }
    fn identity(&self) -> *const () {
        Arc::as_ptr(&self.0).cast()
    }
}

/// What "save image" starts from.
pub enum Export {
    /// The page exactly as it was received.
    Original(Vec<u8>),
    /// A page stored in a form that is not itself an image file: its pixels.
    Pixels(image::RgbaImage),
}

impl PageImage {
    /// A response from a source, import or fetch. Never interpreted as stored
    /// layout: bytes that merely look like a container are just a bad image.
    pub fn from_encoded(bytes: Vec<u8>) -> Self {
        Self(Arc::new(Stored::Whole(bytes)))
    }
    /// A page read back from the cache or a download.
    pub fn from_stored(bytes: Vec<u8>) -> std::result::Result<Self, String> {
        if strips::looks_like(&bytes) {
            let container = strips::Container::parse(bytes).map_err(|e| e.to_string())?;
            Ok(Self(Arc::new(Stored::Strips(container))))
        } else {
            Ok(Self::from_encoded(bytes))
        }
    }
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn downgrade(&self) -> WeakPage {
        WeakPage(Arc::downgrade(&self.0))
    }
    pub fn strong_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }
    /// The original file for a page stored whole, or its reassembled pixels.
    pub fn export(&self, limits: DecodeLimits) -> std::result::Result<Export, String> {
        match &*self.0 {
            Stored::Whole(bytes) => Ok(Export::Original(bytes.clone())),
            Stored::Strips(_) => self
                .0
                .decode(limits)
                .map(Export::Pixels)
                .map_err(|error| error.to_string()),
        }
    }
    /// The file as stored: the fetched image for a whole page, the container
    /// for a split one. Only the stores that write and checksum files use this.
    pub fn stored_bytes(&self) -> &[u8] {
        match &*self.0 {
            Stored::Whole(bytes) => bytes,
            Stored::Strips(container) => container.stored_bytes(),
        }
    }
}

impl PageSource for PageImage {
    fn dimensions(&self, limits: DecodeLimits) -> Result<PageDimensions> {
        self.0.dimensions(limits)
    }
    fn encoded_len(&self) -> usize {
        self.0.encoded_len()
    }
    fn row_granularity(&self) -> Option<u32> {
        self.0.row_granularity()
    }
    fn decode_rows(&self, rows: Range<u32>, limits: DecodeLimits) -> Result<image::RgbaImage> {
        self.0.decode_rows(rows, limits)
    }
    fn decode(&self, limits: DecodeLimits) -> Result<image::RgbaImage> {
        self.0.decode(limits)
    }
}

impl WeakPage {
    pub fn upgrade(&self) -> Option<PageImage> {
        self.0.upgrade().map(PageImage)
    }
    pub fn strong_count(&self) -> usize {
        self.0.strong_count()
    }
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

/// Dimensions of any stored file, from its header alone. Used as the cache's
/// entry check, so a malformed container is a miss exactly as a malformed
/// image is.
pub fn inspect_stored(bytes: &[u8]) -> std::result::Result<(u32, u32), String> {
    let limits = reader_engine::current_limits();
    let dimensions = if strips::looks_like(bytes) {
        strips::Container::parse(bytes.to_vec())
            .and_then(|container| reader_engine::admit_dimensions(container.size(), limits))
    } else {
        reader_engine::inspect_image(bytes, limits)
    }
    .map_err(|error| error.to_string())?;
    Ok((dimensions.width, dimensions.height))
}

/// Full validation of a file about to be committed as a download: every strip
/// decodes, or the image does, so truncated streams fail now rather than at
/// first view.
pub fn validate_stored(bytes: &[u8]) -> std::result::Result<(), String> {
    let limits = reader_engine::current_limits();
    if strips::looks_like(bytes) {
        let container = strips::Container::parse(bytes.to_vec()).map_err(|e| e.to_string())?;
        reader_engine::admit_dimensions(container.size(), limits).map_err(|e| e.to_string())?;
        container.validate(limits).map_err(|e| e.to_string())
    } else {
        reader_engine::decode_image(bytes, limits)
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// Reject a fetched response that claims to be a stored container. Sources
/// serve images; a forged container must never reach a download directory.
pub fn check_fetched(bytes: &[u8]) -> std::result::Result<(), String> {
    if strips::looks_like(bytes) {
        Err("Source returned an unsupported image header".into())
    } else {
        Ok(())
    }
}

/// When and how to split: pages taller than `threshold_rows` become strips of
/// `strip_rows` rows (a multiple of 16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitPlan {
    pub threshold_rows: u32,
    pub strip_rows: u32,
}

impl SplitPlan {
    pub const DEFAULT_THRESHOLD_ROWS: u32 = 4096;
    pub const DEFAULT_STRIP_ROWS: u32 = 2048;
    pub const STRIP_ROWS: std::ops::RangeInclusive<u32> = 256..=8192;
    pub const THRESHOLD_ROWS: std::ops::RangeInclusive<u32> = 1024..=65_536;
    pub fn valid(self) -> bool {
        Self::STRIP_ROWS.contains(&self.strip_rows)
            && self.strip_rows.is_multiple_of(16)
            && Self::THRESHOLD_ROWS.contains(&self.threshold_rows)
    }
}

/// The bytes to store for a freshly fetched page. A tall page becomes strips
/// when `split` allows it; any other page is transformed as a whole image.
/// `Err` means "keep the original": nothing here can lose a page.
pub fn storage_form(
    original: &[u8],
    compression: ImageCompression,
    split: Option<SplitPlan>,
    limits: DecodeLimits,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, String> {
    if let Some(form) = split_form(original, compression, split, limits, max_bytes)? {
        return Ok(form);
    }
    reader_export::reencode_storage_image(original, compression, limits)
}

/// The strip container for `original`, or `None` when the page is not stored
/// as strips: splitting off, compression off, a page that is not tall, or one
/// whose codec choice requires keeping the whole file (a lossless input the
/// pipeline cannot hold, or a lossless JPEG transcode).
pub fn split_form(
    original: &[u8],
    compression: ImageCompression,
    split: Option<SplitPlan>,
    limits: DecodeLimits,
    max_bytes: usize,
) -> std::result::Result<Option<Vec<u8>>, String> {
    // Without compression the store keeps the source bytes; a strip must be
    // encoded with the store's format and quality, so there is nothing to do.
    let Some(plan) = split.filter(|plan| compression.enabled && plan.valid()) else {
        return Ok(None);
    };
    check_fetched(original)?;
    let dimensions = reader_engine::inspect_image(original, limits).map_err(|e| e.to_string())?;
    if dimensions.height <= plan.threshold_rows || dimensions.height <= plan.strip_rows {
        return Ok(None);
    }
    let lossless = match reader_export::storage_recipe(original, compression, limits)? {
        StorageRecipe::Original | StorageRecipe::Transcoded(_) => return Ok(None),
        StorageRecipe::Pixels { lossless } => lossless,
    };
    let page = reader_engine::decode_image(original, limits).map_err(|e| e.to_string())?;
    let mut strips = Vec::new();
    let mut top = 0;
    while top < page.height() {
        let rows = plan.strip_rows.min(page.height() - top);
        let strip = image::imageops::crop_imm(&page, 0, top, page.width(), rows).to_image();
        strips.push((
            rows,
            reader_export::reencode_storage_pixels(strip, compression, lossless, limits)?,
        ));
        top += rows;
    }
    let form = strips::build(page.width(), page.height(), plan.strip_rows, &strips)?;
    if form.len() > max_bytes {
        return Err("Split page exceeds the stored size limit".into());
    }
    Ok(Some(form))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_storage::ImageStorageFormat;

    const LIMITS: DecodeLimits = DecodeLimits {
        max_encoded_bytes: 32 * 1024 * 1024,
        max_width: 16384,
        max_height: 65536,
        max_pixels: 32_000_000,
        max_decoded_bytes: 128 * 1024 * 1024,
    };
    const PLAN: SplitPlan = SplitPlan {
        threshold_rows: 1024,
        strip_rows: 256,
    };

    fn page(width: u32, height: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(width, height, |x, y| {
            let n = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503) ^ (y / 9);
            image::Rgba([n as u8, (n >> 8) as u8, (n >> 16) as u8, 255])
        })
    }
    fn png(image: &image::RgbaImage) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
    fn compression(
        format: ImageStorageFormat,
        quality: u8,
        lossless_inputs: bool,
    ) -> ImageCompression {
        ImageCompression {
            enabled: true,
            format,
            quality,
            lossless_inputs,
            ..ImageCompression::default()
        }
    }
    fn split(image: &image::RgbaImage, compression: ImageCompression) -> Vec<u8> {
        split_form(&png(image), compression, Some(PLAN), LIMITS, 32 << 20)
            .unwrap()
            .expect("tall page splits")
    }

    #[test]
    fn lossless_strips_round_trip_pixel_exactly_and_keep_dimensions() {
        let original = page(60, 1500);
        let bytes = split(&original, compression(ImageStorageFormat::Webp, 85, true));
        assert!(strips::looks_like(&bytes));
        let stored = PageImage::from_stored(bytes.clone()).unwrap();
        let whole = PageImage::from_encoded(png(&original));
        assert_eq!(
            stored.dimensions(LIMITS).unwrap(),
            whole.dimensions(LIMITS).unwrap()
        );
        assert_eq!(inspect_stored(&bytes).unwrap(), (60, 1500));
        assert_eq!(stored.decode(LIMITS).unwrap(), original);
        assert_eq!(
            stored.decode(LIMITS).unwrap(),
            whole.decode(LIMITS).unwrap()
        );
        // Any row range, including ones that straddle strips, matches.
        for rows in [0..256, 255..257, 100..900, 1499..1500, 0..1500] {
            let expected =
                image::imageops::crop_imm(&original, 0, rows.start, 60, rows.end - rows.start)
                    .to_image();
            assert_eq!(
                stored.decode_rows(rows.clone(), LIMITS).unwrap(),
                expected,
                "{rows:?}"
            );
        }
        assert!(stored.decode_rows(10..10, LIMITS).is_err());
        assert!(stored.decode_rows(0..1501, LIMITS).is_err());
        validate_stored(&bytes).unwrap();
        assert_eq!(stored.0.row_granularity(), Some(256));
        assert!(whole.0.row_granularity().is_none());
    }

    #[test]
    fn each_strip_uses_the_stores_format_and_quality() {
        let original = page(48, 1100);
        for (format, quality, guess) in [
            (ImageStorageFormat::Jpeg, 40, image::ImageFormat::Jpeg),
            (ImageStorageFormat::Webp, 30, image::ImageFormat::WebP),
        ] {
            let bytes = split(&original, compression(format, quality, false));
            let container = strips::Container::parse(bytes).unwrap();
            assert_eq!(container.strip_count(), 5);
            for index in 0..container.strip_count() {
                assert_eq!(
                    image::guess_format(container.strip_bytes(index)).unwrap(),
                    guess
                );
            }
        }
        // Quality is the store's: lower quality is smaller and further from the source.
        let high = split(&original, compression(ImageStorageFormat::Webp, 95, false));
        let low = split(&original, compression(ImageStorageFormat::Webp, 10, false));
        assert!(low.len() < high.len());
        let error = |bytes: Vec<u8>| -> u64 {
            let decoded = PageImage::from_stored(bytes)
                .unwrap()
                .decode(LIMITS)
                .unwrap();
            decoded
                .as_raw()
                .iter()
                .zip(original.as_raw())
                .map(|(a, b)| u64::from(a.abs_diff(*b)))
                .sum()
        };
        assert!(error(low) > error(high));
        // Lossless inputs follow the toggle per strip: a PNG source stays exact.
        let exact = split(&original, compression(ImageStorageFormat::Webp, 10, true));
        assert_eq!(
            PageImage::from_stored(exact)
                .unwrap()
                .decode(LIMITS)
                .unwrap(),
            original
        );
    }

    #[test]
    fn decode_rows_touches_only_the_strips_they_intersect() {
        let original = page(32, 1200);
        let mut bytes = split(&original, compression(ImageStorageFormat::Webp, 85, true));
        let container = strips::Container::parse(bytes.clone()).unwrap();
        // Destroy strips 0 and 3; rows in strips 1 and 2 must still decode.
        for index in [0, 3] {
            let range = container.strip_range(index);
            bytes[range.start + 4..range.end - 4].fill(0x5a);
        }
        let stored = PageImage::from_stored(bytes).unwrap();
        assert_eq!(
            stored.decode_rows(256..768, LIMITS).unwrap(),
            image::imageops::crop_imm(&original, 0, 256, 32, 512).to_image()
        );
        assert!(stored.decode_rows(0..10, LIMITS).is_err());
        assert!(stored.decode_rows(900..901, LIMITS).is_err());
        assert!(stored.decode(LIMITS).is_err());
    }

    #[test]
    fn corrupt_containers_are_rejected_not_composited() {
        let original = page(32, 1100);
        let good = split(&original, compression(ImageStorageFormat::Webp, 85, true));
        assert!(PageImage::from_stored(good.clone()).is_ok());
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        cases.push(("truncated payload", good[..good.len() - 1].to_vec()));
        cases.push(("truncated header", good[..20].to_vec()));
        cases.push(("trailing bytes", [good.clone(), vec![0]].concat()));
        for (name, at) in [
            ("version", 8),
            ("width", 12),
            ("height", 16),
            ("rows", 20),
            ("count", 24),
            ("check", 30),
            ("table", 40),
        ] {
            let mut bad = good.clone();
            bad[at] ^= 0x01;
            cases.push((name, bad));
        }
        cases.push(("empty", Vec::new()));
        for (name, bytes) in cases {
            if bytes.is_empty() {
                assert!(
                    PageImage::from_stored(bytes).is_ok(),
                    "empty is just a bad image"
                );
                continue;
            }
            assert!(PageImage::from_stored(bytes.clone()).is_err(), "{name}");
            assert!(inspect_stored(&bytes).is_err(), "{name}");
            assert!(validate_stored(&bytes).is_err(), "{name}");
        }
        // A strip whose own size disagrees with its table entry is corruption.
        let other = split(
            &page(40, 1100),
            compression(ImageStorageFormat::Webp, 85, true),
        );
        let container = strips::Container::parse(good.clone()).unwrap();
        let foreign = strips::Container::parse(other).unwrap();
        let swapped = strips::build(
            32,
            1100,
            256,
            &(0..container.strip_count())
                .map(|i| (container.strip_rows_at(i), foreign.strip_bytes(i).to_vec()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let swapped = PageImage::from_stored(swapped).unwrap();
        assert!(swapped.decode(LIMITS).is_err());
        assert!(validate_stored(swapped.stored_bytes()).is_err());
        // Encoded responses are never interpreted as containers.
        let encoded = PageImage::from_encoded(good.clone());
        assert!(encoded.dimensions(LIMITS).is_err());
        assert!(check_fetched(&good).is_err());
        assert!(check_fetched(&png(&original)).is_ok());
    }

    #[test]
    fn pages_stay_whole_unless_tall_and_compressed() {
        let tall = page(40, 1500);
        let tall_png = png(&tall);
        let on = compression(ImageStorageFormat::Webp, 85, false);
        let form = |compression, plan, bytes: &[u8]| {
            split_form(bytes, compression, plan, LIMITS, 32 << 20).unwrap()
        };
        // Compression off: nothing is split, whatever the plan says.
        assert!(
            form(
                ImageCompression {
                    enabled: false,
                    ..on
                },
                Some(PLAN),
                &tall_png
            )
            .is_none()
        );
        // No plan.
        assert!(form(on, None, &tall_png).is_none());
        // Not taller than the threshold or a strip.
        assert!(
            form(
                on,
                Some(SplitPlan {
                    threshold_rows: 1500,
                    strip_rows: 256
                }),
                &tall_png
            )
            .is_none()
        );
        assert!(
            form(
                on,
                Some(SplitPlan {
                    threshold_rows: 1024,
                    strip_rows: 2048
                }),
                &tall_png
            )
            .is_none()
        );
        // Invalid plans are ignored rather than trusted.
        assert!(
            form(
                on,
                Some(SplitPlan {
                    threshold_rows: 1024,
                    strip_rows: 250
                }),
                &tall_png
            )
            .is_none()
        );
        // storage_form without a plan is the ordinary whole-image transform.
        let whole = storage_form(&tall_png, on, None, LIMITS, 32 << 20).unwrap();
        assert_eq!(
            whole,
            reader_export::reencode_storage_image(&tall_png, on, LIMITS).unwrap()
        );
        assert!(!strips::looks_like(&whole));
        // The codec's own rule that keeps a file whole: a JPEG transcoded to JXL.
        let jpeg = {
            let rgb = image::DynamicImage::ImageRgba8(tall.clone()).to_rgb8();
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
                .encode(rgb.as_raw(), 40, 1500, image::ExtendedColorType::Rgb8)
                .unwrap();
            bytes
        };
        let jxl = ImageCompression {
            format: ImageStorageFormat::JxlLossy,
            jxl_lossless_jpeg: true,
            ..on
        };
        assert!(
            form(jxl, Some(PLAN), &jpeg).is_none(),
            "lossless transcode stays whole"
        );
        // A forged container from a source is rejected, never stored.
        assert!(split_form(&split(&tall, on), on, Some(PLAN), LIMITS, 32 << 20).is_err());
        // The stored-size cap falls back to the original.
        assert!(split_form(&tall_png, on, Some(PLAN), LIMITS, 100).is_err());
    }

    #[test]
    fn exports_and_storage_forms_of_whole_and_split_pages() {
        let original = page(32, 1100);
        let source = png(&original);
        let whole = PageImage::from_encoded(source.clone());
        assert!(matches!(whole.export(LIMITS), Ok(Export::Original(bytes)) if bytes == source));
        assert_eq!(whole.stored_bytes(), source);
        let bytes = split(&original, compression(ImageStorageFormat::Webp, 85, true));
        let stored = PageImage::from_stored(bytes.clone()).unwrap();
        assert_eq!(stored.stored_bytes(), bytes);
        match stored.export(LIMITS).unwrap() {
            Export::Pixels(pixels) => assert_eq!(pixels, original),
            Export::Original(_) => panic!("a split page has no single original file"),
        }
        // Identity follows the shared allocation, weak references never extend it.
        let weak = stored.downgrade();
        let bytes = stored.stored_bytes().to_vec();
        assert!(weak.upgrade().unwrap().ptr_eq(&stored));
        assert!(!stored.ptr_eq(&PageImage::from_stored(bytes).unwrap()));
        drop(stored);
        assert!(weak.upgrade().is_none());
    }
}

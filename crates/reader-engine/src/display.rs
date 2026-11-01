//! Display admission is by visible rows, never by shrinking a long page's width.
use crate::{DecodeLimits, Error, PageDimensions, Result, decode_image, inspect_image};
use std::sync::Arc;

pub const DISPLAY_WINDOW_BYTES: usize = 32 * 1024 * 1024;
const FULL_DISPLAY_BYTES: u64 = 64 * 1024 * 1024;
pub const DISPLAY_TILE_ROWS: u32 = 2048;
const PREFETCH_ROWS: u32 = DISPLAY_TILE_ROWS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayWindowPlan {
    pub width: u32,
    pub full_height: u32,
    pub first_row: u32,
    pub rows: u32,
}
impl DisplayWindowPlan {
    pub fn byte_len(self) -> usize {
        self.width as usize * self.rows as usize * 4
    }
    pub fn covers(self, start: f64, end: f64) -> bool {
        let (first, last) = visible_rows(self.full_height, start, end);
        first >= self.first_row && last <= self.first_row + self.rows
    }
    pub fn covers_with_margin(self, start: f64, end: f64) -> bool {
        let (first, last) = visible_rows(self.full_height, start, end);
        let reserve = ((self.rows.saturating_sub(last - first)) / 4).min(512);
        self.covers(start, end)
            && (self.first_row == 0 || first >= self.first_row + reserve)
            && (self.first_row + self.rows == self.full_height
                || last + reserve <= self.first_row + self.rows)
    }
}
pub struct DisplayWindow {
    pub image: image::RgbaImage,
    pub plan: DisplayWindowPlan,
}

/// One image worker's most recent windowed original, separate from display
/// buffers. Moving a viewport must not decompress the same long page again.
///
/// The encoded Arc identifies the exact admitted source response (including
/// source/authentication changes). A miss releases the old original before
/// decoding, so two originals never overlap within the decoder phase budget.
#[derive(Default)]
pub struct DisplayDecoder {
    cached: Option<DecodedOriginal>,
}
struct DecodedOriginal {
    bytes: Arc<Vec<u8>>,
    limits: DecodeLimits,
    image: image::RgbaImage,
}
impl DisplayDecoder {
    /// Release pixels and their response on reader close, generation changes,
    /// or source revocation, including while the worker has no queued images.
    pub fn clear(&mut self) {
        self.cached = None;
    }

    pub fn decode_window(
        &mut self,
        bytes: &Arc<Vec<u8>>,
        limits: DecodeLimits,
        target_width: u32,
        start: f64,
        end: f64,
        max_bytes: usize,
    ) -> Result<DisplayWindow> {
        let dimensions = inspect_image(bytes, limits)?;
        let plan = plan_display_window(dimensions, target_width, start, end, max_bytes)?;
        if !self
            .cached
            .as_ref()
            .is_some_and(|cached| Arc::ptr_eq(&cached.bytes, bytes) && cached.limits == limits)
        {
            self.cached = None;
            self.cached = Some(DecodedOriginal {
                bytes: bytes.clone(),
                limits,
                image: decode_image(bytes, limits)?,
            });
        }
        let original = &self.cached.as_ref().unwrap().image;
        // Whole-page display already retains the pixels: transfer ownership
        // instead of keeping a second copy just for a cache hit.
        let image = if is_original(plan, original) {
            self.cached.take().unwrap().image
        } else {
            render_window(original, plan, limits)?
        };
        Ok(DisplayWindow { image, plan })
    }
}
fn visible_rows(height: u32, start: f64, end: f64) -> (u32, u32) {
    let first = (start.clamp(0., 1.) * f64::from(height)).floor() as u32;
    let first = first.min(height - 1);
    let last = (end.clamp(0., 1.) * f64::from(height)).ceil() as u32;
    (first, last.max(first + 1).min(height))
}
pub fn plan_display_window(
    dimensions: PageDimensions,
    target_width: u32,
    start: f64,
    end: f64,
    max_bytes: usize,
) -> Result<DisplayWindowPlan> {
    if dimensions.width == 0
        || dimensions.height == 0
        || target_width == 0
        || !start.is_finite()
        || !end.is_finite()
        || !(0.0..=1.0).contains(&start)
        || !(start..=1.0).contains(&end)
    {
        return Err(Error::Layout);
    }
    let width = target_width.min(dimensions.width);
    let full_height = (u64::from(dimensions.height) * u64::from(width)
        / u64::from(dimensions.width))
    .max(1) as u32;
    let (first, last) = visible_rows(full_height, start, end);
    let max_rows = (max_bytes / (width as usize * 4)).min(full_height as usize) as u32;
    let required = last - first;
    if required > max_rows {
        return Err(Error::Limit);
    }
    let rows = required
        .saturating_add(PREFETCH_ROWS * 2)
        .min(max_rows)
        .min(full_height);
    let first_row = first
        .saturating_sub((rows - required) / 2)
        .min(full_height - rows);
    // Stable global strips let the renderer retain overlapping textures.
    // A tight budget keeps the exact admitted window rather than dropping any
    // visible rows or increasing its allocation allowance.
    let aligned_first = first_row / DISPLAY_TILE_ROWS * DISPLAY_TILE_ROWS;
    let aligned_last = (first_row + rows)
        .div_ceil(DISPLAY_TILE_ROWS)
        .saturating_mul(DISPLAY_TILE_ROWS)
        .min(full_height);
    let (first_row, rows) = if aligned_last - aligned_first <= max_rows {
        (aligned_first, aligned_last - aligned_first)
    } else {
        (first_row, rows)
    };
    Ok(DisplayWindowPlan {
        width,
        full_height,
        first_row,
        rows,
    })
}
/// Full originals still pass the bounded CPU decoder. Only the retained output
/// is windowed. 2048 rows of leading/trailing coverage normally arrive before
/// the viewport needs them; a viewport larger than admission fails explicitly.
pub fn decode_display_window(
    bytes: &[u8],
    limits: DecodeLimits,
    target_width: u32,
    start: f64,
    end: f64,
    max_bytes: usize,
) -> Result<DisplayWindow> {
    let dimensions = inspect_image(bytes, limits)?;
    let plan = plan_display_window(dimensions, target_width, start, end, max_bytes)?;
    let original = decode_image(bytes, limits)?;
    let image = render(original, plan, limits)?;
    Ok(DisplayWindow { image, plan })
}
pub fn decode_display_image(
    bytes: &[u8],
    limits: DecodeLimits,
    max_width: u32,
    max_height: u32,
) -> Result<image::RgbaImage> {
    if max_width == 0 || max_height == 0 {
        return Err(Error::Limit);
    }
    let dimensions = inspect_image(bytes, limits)?;
    let scale = (f64::from(max_width) / f64::from(dimensions.width))
        .min(f64::from(max_height) / f64::from(dimensions.height))
        .min(1.);
    let width = (f64::from(dimensions.width) * scale).floor().max(1.) as u32;
    let height = (f64::from(dimensions.height) * scale).floor().max(1.) as u32;
    if u64::from(width) * u64::from(height) * 4 > FULL_DISPLAY_BYTES {
        return Err(Error::Limit);
    }
    render(
        decode_image(bytes, limits)?,
        DisplayWindowPlan {
            width,
            full_height: height,
            first_row: 0,
            rows: height,
        },
        limits,
    )
}
fn render(
    original: image::RgbaImage,
    plan: DisplayWindowPlan,
    limits: DecodeLimits,
) -> Result<image::RgbaImage> {
    if is_original(plan, &original) {
        return Ok(original);
    }
    render_window(&original, plan, limits)
}
fn is_original(plan: DisplayWindowPlan, original: &image::RgbaImage) -> bool {
    original.width() == plan.width
        && original.height() == plan.full_height
        && plan.first_row == 0
        && plan.rows == plan.full_height
}
fn render_window(
    original: &image::RgbaImage,
    plan: DisplayWindowPlan,
    limits: DecodeLimits,
) -> Result<image::RgbaImage> {
    // Decoder/native conversion memory is gone before output allocation. Admit
    // this second phase separately, rather than summing disjoint phase peaks.
    if (original.as_raw().len() as u64)
        .checked_add(plan.byte_len() as u64)
        .is_none_or(|n| n > limits.max_decoded_bytes)
    {
        return Err(Error::Limit);
    }
    if original.width() == plan.width && original.height() == plan.full_height {
        let stride = plan.width as usize * 4;
        let start = plan.first_row as usize * stride;
        let bytes = original.as_raw()[start..start + plan.byte_len()].to_vec();
        return image::RgbaImage::from_raw(plan.width, plan.rows, bytes).ok_or(Error::Limit);
    }
    // Bilinear sampling uses the FULL page's coordinate transform, so two
    // windows sample identical source pixels at a shared row. No giant float
    // intermediate, nearest-neighbour aliasing, or artificial source upscaling.
    let mut image = image::RgbaImage::new(plan.width, plan.rows);
    let source_stride = original.width() as usize * 4;
    for (y, row) in image
        .as_mut()
        .chunks_exact_mut(plan.width as usize * 4)
        .enumerate()
    {
        // Source row selection is shared by every pixel in this output row.
        let sy = ((f64::from(plan.first_row + y as u32) + 0.5) * f64::from(original.height())
            / f64::from(plan.full_height)
            - 0.5)
            .max(0.);
        let y0 = (sy.floor() as u32).min(original.height() - 1);
        let y1 = (y0 + 1).min(original.height() - 1);
        let fy = sy - f64::from(y0);
        let upper = &original.as_raw()[y0 as usize * source_stride..][..source_stride];
        let lower = &original.as_raw()[y1 as usize * source_stride..][..source_stride];
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let sx = ((x as f64 + 0.5) * f64::from(original.width()) / f64::from(plan.width) - 0.5)
                .max(0.);
            let x0 = (sx.floor() as u32).min(original.width() - 1);
            let x1 = (x0 + 1).min(original.width() - 1);
            let fx = sx - f64::from(x0);
            let a = &upper[x0 as usize * 4..][..4];
            let b = &upper[x1 as usize * 4..][..4];
            let c = &lower[x0 as usize * 4..][..4];
            let d = &lower[x1 as usize * 4..][..4];
            for i in 0..4 {
                pixel[i] = ((f64::from(a[i]) * (1. - fx) + f64::from(b[i]) * fx) * (1. - fy)
                    + (f64::from(c[i]) * (1. - fx) + f64::from(d[i]) * fx) * fy)
                    .round()
                    .clamp(0., 255.) as u8;
            }
        }
    }
    Ok(image)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pattern(x: u32, y: u32) -> image::Rgba<u8> {
        if x.is_multiple_of(9) || y.is_multiple_of(13) || (x % 9 == 6 && y % 13 < 7) {
            image::Rgba([0, 0, 0, 255])
        } else {
            image::Rgba([255, 255, 255, 255])
        }
    }
    #[test]
    fn tall_scroll_reuses_original_and_preserves_native_pixels() {
        // RGB PNG fits the existing native-output + RGBA admission. An RGBA
        // source of this size intentionally exceeds that decode-phase budget.
        let image = image::RgbImage::from_fn(720, 25000, |x, y| {
            let p = pattern(x, y);
            image::Rgb([p[0], p[1], p[2]])
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        drop(image);
        let bytes = Arc::new(encoded.into_inner());
        let mut decoder = DisplayDecoder::default();
        let mut original: *const u8 = std::ptr::null();
        for start in [0., 0.3, 0.6, 0.94] {
            let window = decoder
                .decode_window(
                    &bytes,
                    DecodeLimits::default(),
                    1080,
                    start,
                    start + 0.06,
                    DISPLAY_WINDOW_BYTES,
                )
                .unwrap();
            let pixels = decoder.cached.as_ref().unwrap().image.as_raw().as_ptr();
            if original.is_null() {
                original = pixels;
            }
            assert_eq!(
                pixels, original,
                "scrolling must reuse the decoded original"
            );
            assert_eq!(window.image.width(), 720);
            assert!(window.plan.covers(start, start + 0.06));
            assert!(window.plan.byte_len() <= DISPLAY_WINDOW_BYTES);
            for y in (0..window.plan.rows).step_by(113) {
                for x in 0..720 {
                    assert_eq!(
                        *window.image.get_pixel(x, y),
                        pattern(x, window.plan.first_row + y)
                    );
                }
            }
        }
    }
    #[test]
    fn cached_original_tracks_response_identity_limits_and_full_page_ownership() {
        let encode = |value| {
            let image = image::RgbImage::from_pixel(32, 10000, image::Rgb([value, 20, 30]));
            let mut encoded = std::io::Cursor::new(Vec::new());
            image
                .write_to(&mut encoded, image::ImageFormat::Png)
                .unwrap();
            Arc::new(encoded.into_inner())
        };
        let mut decoder = DisplayDecoder::default();
        for bytes in [encode(10), encode(200)] {
            let window = decoder
                .decode_window(&bytes, DecodeLimits::default(), 32, 0.4, 0.42, 64000)
                .unwrap();
            assert_eq!(
                window.image.get_pixel(0, 0)[0],
                decode_image(&bytes, DecodeLimits::default())
                    .unwrap()
                    .get_pixel(0, 0)[0]
            );
            let stricter = DecodeLimits {
                max_height: 100,
                ..DecodeLimits::default()
            };
            assert!(
                decoder
                    .decode_window(&bytes, stricter, 32, 0.4, 0.42, 64000)
                    .is_err()
            );
        }
        let bytes = encode(77);
        let window = decoder
            .decode_window(
                &bytes,
                DecodeLimits::default(),
                32,
                0.,
                1.,
                DISPLAY_WINDOW_BYTES,
            )
            .unwrap();
        assert_eq!(window.image.dimensions(), (32, 10000));
        assert!(
            decoder.cached.is_none(),
            "full output owns the original allocation"
        );
        let limits = DecodeLimits {
            max_decoded_bytes: 2_300_000,
            ..DecodeLimits::default()
        };
        assert!(decode_image(&bytes, limits).is_ok());
        assert!(matches!(
            decoder.decode_window(&bytes, limits, 31, 0., 1., DISPLAY_WINDOW_BYTES),
            Err(Error::Limit)
        ));
    }
    #[test]
    fn clearing_idle_decoder_releases_pixels_and_encoded_response() {
        let bytes = Arc::new(include_bytes!("../tests/fixtures/tiny.avif").to_vec());
        let limits = DecodeLimits::default();
        let mut decoder = DisplayDecoder::default();
        let first = decoder
            .decode_window(&bytes, limits, 16, 0., 0.1, 128)
            .unwrap();
        assert_eq!(decoder.cached.as_ref().unwrap().image.as_raw().len(), 1024);
        assert_eq!(Arc::strong_count(&bytes), 2);
        decoder.clear();
        assert!(decoder.cached.is_none());
        assert_eq!(Arc::strong_count(&bytes), 1);
        let again = decoder
            .decode_window(&bytes, limits, 16, 0., 0.1, 128)
            .unwrap();
        assert_eq!(first.image, again.image);
        assert_eq!(decoder.cached.as_ref().unwrap().image.as_raw().len(), 1024);
        assert_eq!(Arc::strong_count(&bytes), 2);
    }
    #[test]
    fn row_resampling_matches_original_bilinear_pixels_at_window_boundaries() {
        let original = image::RgbaImage::from_fn(97, 211, |x, y| {
            image::Rgba([
                (x * 71 + y * 19) as u8,
                (x * 11 + y * 83) as u8,
                (x * 31 + y * 47) as u8,
                (x * 13 + y * 7) as u8,
            ])
        });
        for width in [1, 2, 17, 53, 96] {
            let full_height = 211 * width / 97;
            let plan = DisplayWindowPlan {
                width,
                full_height,
                first_row: full_height / 3,
                rows: full_height - full_height / 3,
            };
            let expected = image::RgbaImage::from_fn(plan.width, plan.rows, |x, y| {
                let sx = ((f64::from(x) + 0.5) * f64::from(original.width())
                    / f64::from(plan.width)
                    - 0.5)
                    .max(0.);
                let sy = ((f64::from(plan.first_row + y) + 0.5) * f64::from(original.height())
                    / f64::from(plan.full_height)
                    - 0.5)
                    .max(0.);
                let x0 = (sx.floor() as u32).min(original.width() - 1);
                let x1 = (x0 + 1).min(original.width() - 1);
                let y0 = (sy.floor() as u32).min(original.height() - 1);
                let y1 = (y0 + 1).min(original.height() - 1);
                let fx = sx - f64::from(x0);
                let fy = sy - f64::from(y0);
                let a = original.get_pixel(x0, y0);
                let b = original.get_pixel(x1, y0);
                let c = original.get_pixel(x0, y1);
                let d = original.get_pixel(x1, y1);
                image::Rgba(std::array::from_fn(|i| {
                    ((f64::from(a[i]) * (1. - fx) + f64::from(b[i]) * fx) * (1. - fy)
                        + (f64::from(c[i]) * (1. - fx) + f64::from(d[i]) * fx) * fy)
                        .round()
                        .clamp(0., 255.) as u8
                }))
            });
            assert_eq!(
                render_window(&original, plan, DecodeLimits::default()).unwrap(),
                expected
            );
        }
    }
    #[test]
    fn tall_small_letter_strokes_retain_native_width_and_exact_pixels() {
        let image = image::RgbaImage::from_fn(720, 20000, pattern);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        drop(image);
        let full =
            decode_display_image(encoded.get_ref(), DecodeLimits::default(), 1080, 65536).unwrap();
        assert_eq!(full.dimensions(), (720, 20000));
        assert_eq!(*full.get_pixel(6, 1000), pattern(6, 1000));
        drop(full);
        for (start, end) in [(0., 0.08), (0.45, 0.53), (0.92, 1.)] {
            let window = decode_display_window(
                encoded.get_ref(),
                DecodeLimits::default(),
                1080,
                start,
                end,
                DISPLAY_WINDOW_BYTES,
            )
            .unwrap();
            assert_eq!(window.image.width(), 720);
            assert!(window.plan.covers(start, end));
            assert!(window.plan.byte_len() <= DISPLAY_WINDOW_BYTES);
            for y in (0..window.image.height()).step_by(113) {
                for x in 0..720 {
                    assert_eq!(
                        *window.image.get_pixel(x, y),
                        pattern(x, window.plan.first_row + y)
                    );
                }
            }
        }
    }
    #[test]
    fn strip_alignment_preserves_visible_rows_and_tight_budget_admission() {
        let dimensions = PageDimensions {
            width: 720,
            height: 25000,
        };
        for first in (0..23400).step_by(61) {
            let start = first as f64 / 25000.;
            let end = (first + 1600) as f64 / 25000.;
            let plan =
                plan_display_window(dimensions, 1080, start, end, DISPLAY_WINDOW_BYTES).unwrap();
            assert_eq!(plan.first_row % DISPLAY_TILE_ROWS, 0);
            assert!(
                plan.first_row + plan.rows == 25000
                    || (plan.first_row + plan.rows).is_multiple_of(DISPLAY_TILE_ROWS)
            );
            assert!(plan.covers(start, end));
            assert!(plan.byte_len() <= DISPLAY_WINDOW_BYTES);
            let tight = plan_display_window(dimensions, 1080, start, end, 1602 * 720 * 4).unwrap();
            assert!(tight.covers(start, end));
            assert!(tight.byte_len() <= 1602 * 720 * 4);
        }
    }

    #[test]
    fn window_admission_keeps_width_and_guarantees_two_outputs_fit() {
        for dpr in [1., 1.25, 1.5, 2.75, 3.] {
            for zoom in [0.75, 1., 1.5, 3.] {
                let target = (390_f64 * zoom * dpr).ceil() as u32;
                let plan = plan_display_window(
                    PageDimensions {
                        width: 1440,
                        height: 20000,
                    },
                    target,
                    0.3,
                    0.38,
                    DISPLAY_WINDOW_BYTES,
                )
                .unwrap();
                assert_eq!(plan.width, target.min(1440));
                assert!(plan.covers(0.3, 0.38));
                assert!(plan.byte_len() * 2 <= 64 * 1024 * 1024);
                if plan.first_row > 0 {
                    assert!(!plan.covers(0., 0.1));
                }
            }
        }
        assert!(
            plan_display_window(
                PageDimensions {
                    width: 16384,
                    height: 65536
                },
                16384,
                0.,
                1.,
                DISPLAY_WINDOW_BYTES
            )
            .is_err()
        );
    }
}

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

/// Immutable settings for a display request. They never change encoded bytes,
/// downloaded originals, or the decoder's cached source pixels. Callers must
/// include this snapshot in their window/tile identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayOptions {
    pub crop_borders: bool,
    pub split_wide: bool,
    pub rotate_wide: bool,
    pub flip_wide: bool,
    pub rtl: bool,
    /// Rotate the composed virtual page 180 degrees (desktop reverse portrait).
    pub reverse_page: bool,
    pub color_filter: bool,
    pub filter_color: u32,
    pub filter_intensity: u8,
    pub grayscale: bool,
    pub invert_colors: bool,
}
impl DisplayOptions {
    fn has_filters(self) -> bool {
        (self.color_filter && self.filter_intensity > 0) || self.grayscale || self.invert_colors
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageRotation {
    #[default]
    None,
    Clockwise,
    CounterClockwise,
}
/// Geometry of the virtual display image, before output-width resampling.
/// Split spreads are stacked in reading order; each half occupies half_height
/// rows. Coordinates below are pixel centers, from zero through dimension-1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagePresentation {
    pub source: PageDimensions,
    pub cropped: PageDimensions,
    pub effective: PageDimensions,
    pub crop_x: u32,
    pub crop_y: u32,
    pub split: bool,
    pub half_height: u32,
    pub rotation: PageRotation,
    pub rtl: bool,
    pub reverse_page: bool,
    /// Original top-left pixel after filters, stable across viewport windows.
    pub background_color: [u8; 4],
}
impl PagePresentation {
    fn new(source: PageDimensions, crop: CropRect, options: DisplayOptions) -> Result<Self> {
        let cropped = PageDimensions {
            width: crop.width,
            height: crop.height,
        };
        let split = options.split_wide && cropped.width > cropped.height;
        let rotation = if !split && options.rotate_wide && cropped.width > cropped.height {
            if options.flip_wide {
                PageRotation::CounterClockwise
            } else {
                PageRotation::Clockwise
            }
        } else {
            PageRotation::None
        };
        let effective = if split {
            PageDimensions {
                width: cropped.width.div_ceil(2),
                height: cropped.height.checked_mul(2).ok_or(Error::Limit)?,
            }
        } else if rotation != PageRotation::None {
            PageDimensions {
                width: cropped.height,
                height: cropped.width,
            }
        } else {
            cropped
        };
        Ok(Self {
            source,
            cropped,
            effective,
            crop_x: crop.x,
            crop_y: crop.y,
            split,
            half_height: if split { cropped.height } else { 0 },
            rotation,
            rtl: options.rtl,
            reverse_page: options.reverse_page,
            background_color: [255; 4],
        })
    }
    fn is_identity(self) -> bool {
        self.crop_x == 0
            && self.crop_y == 0
            && self.source == self.cropped
            && !self.split
            && self.rotation == PageRotation::None
            && !self.reverse_page
    }
    fn source_pixel(self, x: u32, y: u32) -> (u32, u32) {
        let (x, y) = if self.reverse_page {
            (self.effective.width - 1 - x, self.effective.height - 1 - y)
        } else {
            (x, y)
        };
        let (x, y) = if self.split {
            let right = (y >= self.half_height) != self.rtl;
            let left_width = self.cropped.width.div_ceil(2);
            let offset = if right { left_width } else { 0 };
            let width = if right {
                self.cropped.width - left_width
            } else {
                left_width
            };
            (x.min(width - 1) + offset, y % self.half_height)
        } else {
            match self.rotation {
                PageRotation::None => (x, y),
                PageRotation::Clockwise => (y, self.cropped.height - 1 - x),
                PageRotation::CounterClockwise => (self.cropped.width - 1 - y, x),
            }
        };
        (x + self.crop_x, y + self.crop_y)
    }
    /// Map display pixel centers to the original image (also useful for capture
    /// and source-coordinate anchors). The shorter odd-width half pads its last
    /// column, so that single padded column has no unique inverse.
    pub fn source_coordinate(self, x: f64, y: f64) -> (f64, f64) {
        let x = x.clamp(0., f64::from(self.effective.width - 1));
        let y = y.clamp(0., f64::from(self.effective.height - 1));
        let (x, y) = if self.reverse_page {
            (
                f64::from(self.effective.width - 1) - x,
                f64::from(self.effective.height - 1) - y,
            )
        } else {
            (x, y)
        };
        let (x, y) = if self.split {
            let second = y >= f64::from(self.half_height);
            let right = second != self.rtl;
            let left_width = self.cropped.width.div_ceil(2);
            let offset = if right { left_width } else { 0 };
            let width = if right {
                self.cropped.width - left_width
            } else {
                left_width
            };
            (
                x.min(f64::from(width - 1)) + f64::from(offset),
                y % f64::from(self.half_height),
            )
        } else {
            match self.rotation {
                PageRotation::None => (x, y),
                PageRotation::Clockwise => (y, f64::from(self.cropped.height - 1) - x),
                PageRotation::CounterClockwise => (f64::from(self.cropped.width - 1) - y, x),
            }
        };
        (x + f64::from(self.crop_x), y + f64::from(self.crop_y))
    }
    /// Inverse source-to-display mapping for source-coordinate anchors.
    pub fn display_coordinate(self, x: f64, y: f64) -> (f64, f64) {
        let x = (x - f64::from(self.crop_x)).clamp(0., f64::from(self.cropped.width - 1));
        let y = (y - f64::from(self.crop_y)).clamp(0., f64::from(self.cropped.height - 1));
        let (x, y) = if self.split {
            let left_width = self.cropped.width.div_ceil(2);
            let right = x >= f64::from(left_width);
            let second = right != self.rtl;
            (
                x - if right { f64::from(left_width) } else { 0. },
                y + if second {
                    f64::from(self.half_height)
                } else {
                    0.
                },
            )
        } else {
            match self.rotation {
                PageRotation::None => (x, y),
                PageRotation::Clockwise => (f64::from(self.cropped.height - 1) - y, x),
                PageRotation::CounterClockwise => (y, f64::from(self.cropped.width - 1) - x),
            }
        };
        if self.reverse_page {
            (
                f64::from(self.effective.width - 1) - x,
                f64::from(self.effective.height - 1) - y,
            )
        } else {
            (x, y)
        }
    }
}
#[derive(Clone, Copy)]
struct CropRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}
impl CropRect {
    fn full(dimensions: PageDimensions) -> Self {
        Self {
            x: 0,
            y: 0,
            width: dimensions.width,
            height: dimensions.height,
        }
    }
}
/// Remove only confidently empty outer bands. An all-blank or sparsely marked
/// page and tiny gutters keep their original framing. Nonwhite edge details,
/// including low-alpha ink, stop the scan; transparent RGB values are ignored.
fn detect_crop(original: &image::RgbaImage) -> CropRect {
    let full = CropRect::full(PageDimensions {
        width: original.width(),
        height: original.height(),
    });
    let (mut left, mut top, mut right, mut bottom) = (full.width, full.height, 0, 0);
    for (x, y, pixel) in original.enumerate_pixels() {
        if pixel[3] != 0 && pixel.0[..3].iter().any(|channel| *channel < 248) {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    if right <= left
        || bottom <= top
        || (right - left) < full.width.div_ceil(2)
        || (bottom - top) < full.height.div_ceil(2)
    {
        return full;
    }
    let min_x = (full.width / 100).max(2);
    let min_y = (full.height / 100).max(2);
    let x = if left >= min_x { left } else { 0 };
    let y = if top >= min_y { top } else { 0 };
    let right = if full.width - right >= min_x {
        right
    } else {
        full.width
    };
    let bottom = if full.height - bottom >= min_y {
        bottom
    } else {
        full.height
    };
    CropRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

/// Describe an already admitted original without allocating a transformed copy.
/// Capture uses the same crop/rotation/split geometry as the display worker.
pub fn page_presentation(
    original: &image::RgbaImage,
    options: DisplayOptions,
) -> Result<PagePresentation> {
    let dimensions = PageDimensions {
        width: original.width(),
        height: original.height(),
    };
    if dimensions.width == 0 || dimensions.height == 0 {
        return Err(Error::Layout);
    }
    let crop = if options.crop_borders {
        detect_crop(original)
    } else {
        CropRect::full(dimensions)
    };
    let mut presentation = PagePresentation::new(dimensions, crop, options)?;
    let mut background = original.get_pixel(0, 0).0;
    filter_pixel(&mut background, options);
    presentation.background_color = background;
    Ok(presentation)
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
    crop: Option<CropRect>,
}
impl DisplayDecoder {
    /// Release pixels and their response on reader close, generation changes,
    /// or source revocation, including while the worker has no queued images.
    pub fn clear(&mut self) {
        self.cached = None;
    }

    fn ensure_original(&mut self, bytes: &Arc<Vec<u8>>, limits: DecodeLimits) -> Result<()> {
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
                crop: None,
            });
        }
        Ok(())
    }

    /// Obtain virtual geometry before constructing the viewport/window request.
    /// Cropping scans the admitted original once; filter-only changes reuse it.
    pub fn presentation(
        &mut self,
        bytes: &Arc<Vec<u8>>,
        limits: DecodeLimits,
        options: DisplayOptions,
    ) -> Result<PagePresentation> {
        let dimensions = inspect_image(bytes, limits)?;
        self.ensure_original(bytes, limits)?;
        let crop = if options.crop_borders {
            let cached = self.cached.as_mut().unwrap();
            *cached
                .crop
                .get_or_insert_with(|| detect_crop(&cached.image))
        } else {
            CropRect::full(dimensions)
        };
        let mut presentation = PagePresentation::new(dimensions, crop, options)?;
        let mut background = *self.cached.as_ref().unwrap().image.get_pixel(0, 0);
        filter_pixel(&mut background.0, options);
        presentation.background_color = background.0;
        Ok(presentation)
    }

    pub fn display_dimensions(
        &mut self,
        bytes: &Arc<Vec<u8>>,
        limits: DecodeLimits,
        options: DisplayOptions,
    ) -> Result<PageDimensions> {
        Ok(self.presentation(bytes, limits, options)?.effective)
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
        self.decode_window_with_options(
            bytes,
            limits,
            target_width,
            start,
            end,
            max_bytes,
            DisplayOptions::default(),
        )
    }

    /// Render only the admitted virtual row window. No transformed full-frame
    /// copy or float plane is allocated, and the cached original stays exact.
    #[allow(clippy::too_many_arguments)]
    pub fn decode_window_with_options(
        &mut self,
        bytes: &Arc<Vec<u8>>,
        limits: DecodeLimits,
        target_width: u32,
        start: f64,
        end: f64,
        max_bytes: usize,
        options: DisplayOptions,
    ) -> Result<DisplayWindow> {
        let presentation = self.presentation(bytes, limits, options)?;
        let plan =
            plan_display_window(presentation.effective, target_width, start, end, max_bytes)?;
        self.ensure_original(bytes, limits)?;
        let original = &self.cached.as_ref().unwrap().image;
        // Keep the original for changing filters, but preserve the native
        // identity transfer fast path used by ordinary full-page display.
        let image = if presentation.is_identity()
            && !options.has_filters()
            && is_original(plan, original)
        {
            self.cached.take().unwrap().image
        } else {
            let mut image = if presentation.is_identity() {
                render_window(original, plan, limits)?
            } else {
                render_presentation(original, presentation, plan, limits)?
            };
            if options.has_filters() {
                apply_filters(&mut image, options);
            }
            image
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
fn render_presentation(
    original: &image::RgbaImage,
    presentation: PagePresentation,
    plan: DisplayWindowPlan,
    limits: DecodeLimits,
) -> Result<image::RgbaImage> {
    if (original.as_raw().len() as u64)
        .checked_add(plan.byte_len() as u64)
        .is_none_or(|n| n > limits.max_decoded_bytes)
    {
        return Err(Error::Limit);
    }
    let mut output = image::RgbaImage::new(plan.width, plan.rows);
    // Native cropped rows remain contiguous. Copy directly rather than paying
    // per-pixel transform/sampling cost on a long cropped page.
    if !presentation.split
        && !presentation.reverse_page
        && presentation.rotation == PageRotation::None
        && plan.width == presentation.effective.width
        && plan.full_height == presentation.effective.height
    {
        let stride = original.width() as usize * 4;
        let width_bytes = plan.width as usize * 4;
        for (y, row) in output.as_mut().chunks_exact_mut(width_bytes).enumerate() {
            let start = (presentation.crop_y as usize + plan.first_row as usize + y) * stride
                + presentation.crop_x as usize * 4;
            row.copy_from_slice(&original.as_raw()[start..start + width_bytes]);
        }
        return Ok(output);
    }
    let width = presentation.effective.width;
    let height = presentation.effective.height;
    for (y, row) in output
        .as_mut()
        .chunks_exact_mut(plan.width as usize * 4)
        .enumerate()
    {
        let sy = ((f64::from(plan.first_row + y as u32) + 0.5) * f64::from(height)
            / f64::from(plan.full_height)
            - 0.5)
            .clamp(0., f64::from(height - 1));
        let y0 = sy.floor() as u32;
        // Clamp interpolation at the split seam: separate paper halves must
        // never mix the bottom of one page with the top of its neighbour.
        let section_end = if presentation.split && y0 < presentation.half_height {
            presentation.half_height - 1
        } else {
            height - 1
        };
        let y1 = (y0 + 1).min(section_end);
        let fy = sy - f64::from(y0);
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let sx = ((x as f64 + 0.5) * f64::from(width) / f64::from(plan.width) - 0.5)
                .clamp(0., f64::from(width - 1));
            let x0 = sx.floor() as u32;
            let x1 = (x0 + 1).min(width - 1);
            let fx = sx - f64::from(x0);
            let sample = |x, y| {
                let (sx, sy) = presentation.source_pixel(x, y);
                original.get_pixel(sx, sy)
            };
            let (a, b, c, d) = (
                sample(x0, y0),
                sample(x1, y0),
                sample(x0, y1),
                sample(x1, y1),
            );
            for i in 0..4 {
                pixel[i] = ((f64::from(a[i]) * (1. - fx) + f64::from(b[i]) * fx) * (1. - fy)
                    + (f64::from(c[i]) * (1. - fx) + f64::from(d[i]) * fx) * fy)
                    .round()
                    .clamp(0., 255.) as u8;
            }
        }
    }
    Ok(output)
}
fn apply_filters(image: &mut image::RgbaImage, options: DisplayOptions) {
    for pixel in image.pixels_mut() {
        filter_pixel(&mut pixel.0, options);
    }
}
/// Apply the display color chain to one RGBA sample, preserving alpha.
pub fn filter_pixel(pixel: &mut [u8; 4], options: DisplayOptions) {
    let intensity = u32::from(options.filter_intensity.min(100));
    let tint = [
        (options.filter_color >> 16) & 255,
        (options.filter_color >> 8) & 255,
        options.filter_color & 255,
    ];
    if options.color_filter && intensity > 0 {
        for (channel, tint) in pixel[..3].iter_mut().zip(tint) {
            *channel =
                ((u32::from(*channel) * (100 - intensity) + tint * intensity + 50) / 100) as u8;
        }
    }
    if options.grayscale {
        let gray = ((u32::from(pixel[0]) * 299
            + u32::from(pixel[1]) * 587
            + u32::from(pixel[2]) * 114
            + 500)
            / 1000) as u8;
        pixel[..3].fill(gray);
    }
    if options.invert_colors {
        for channel in &mut pixel[..3] {
            *channel = 255 - *channel;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admitted_original_presentation_matches_worker_and_rejects_empty_images() {
        assert!(
            page_presentation(&image::RgbaImage::new(0, 0), DisplayOptions::default()).is_err()
        );
        let original = image::RgbaImage::from_fn(20, 16, |x, y| {
            image::Rgba(if (3..17).contains(&x) && (2..14).contains(&y) {
                [20, 40, 80, 255]
            } else {
                [255; 4]
            })
        });
        let options = DisplayOptions {
            crop_borders: true,
            split_wide: true,
            rtl: true,
            color_filter: true,
            filter_color: 0x123456,
            filter_intensity: 25,
            grayscale: true,
            invert_colors: true,
            ..Default::default()
        };
        let presentation = page_presentation(&original, options).unwrap();
        let mut worker = DisplayDecoder::default();
        assert_eq!(
            presentation,
            worker
                .presentation(&encoded(&original), DecodeLimits::default(), options)
                .unwrap()
        );
        assert_eq!(
            presentation.effective,
            PageDimensions {
                width: 7,
                height: 24
            }
        );
    }

    fn encoded(image: &image::RgbaImage) -> Arc<Vec<u8>> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        Arc::new(bytes.into_inner())
    }
    #[test]
    fn conservative_crop_retains_details_tiny_gutters_and_blank_framing() {
        let image = image::RgbaImage::from_fn(20, 16, |x, y| {
            if (3..17).contains(&x) && (2..14).contains(&y) {
                image::Rgba([20, 30, 40, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            }
        });
        let bytes = encoded(&image);
        let options = DisplayOptions {
            crop_borders: true,
            ..DisplayOptions::default()
        };
        let mut decoder = DisplayDecoder::default();
        let p = decoder
            .presentation(&bytes, DecodeLimits::default(), options)
            .unwrap();
        assert_eq!(
            (p.crop_x, p.crop_y, p.cropped.width, p.cropped.height),
            (3, 2, 14, 12)
        );
        let original = decoder.cached.as_ref().unwrap().image.as_ptr();
        let window = decoder
            .decode_window_with_options(&bytes, DecodeLimits::default(), 14, 0., 1., 10000, options)
            .unwrap();
        assert_eq!(
            window.image,
            image::imageops::crop_imm(&image, 3, 2, 14, 12).to_image()
        );
        assert_eq!(decoder.cached.as_ref().unwrap().image.as_ptr(), original);
        assert_eq!(decoder.cached.as_ref().unwrap().image, image);
        assert_eq!(p.source_coordinate(0., 0.), (3., 2.));
        assert_eq!(p.display_coordinate(3., 2.), (0., 0.));
        for mut blank in [
            image::RgbaImage::from_pixel(20, 16, image::Rgba([255; 4])),
            image::RgbaImage::from_pixel(20, 16, image::Rgba([17, 19, 29, 0])),
        ] {
            let c = detect_crop(&blank);
            assert_eq!((c.x, c.y, c.width, c.height), (0, 0, 20, 16));
            blank.put_pixel(10, 8, image::Rgba([0, 0, 0, 255]));
            let c = detect_crop(&blank);
            assert_eq!((c.x, c.y, c.width, c.height), (0, 0, 20, 16));
        }
        let tiny = image::RgbaImage::from_fn(20, 16, |x, y| {
            if x == 0 || x == 19 || y == 0 || y == 15 {
                image::Rgba([255; 4])
            } else {
                image::Rgba([0, 0, 0, 255])
            }
        });
        let c = detect_crop(&tiny);
        assert_eq!((c.x, c.y, c.width, c.height), (0, 0, 20, 16));
        let mut edge = image;
        edge.put_pixel(0, 8, image::Rgba([0, 0, 0, 1]));
        assert_eq!(detect_crop(&edge).x, 0, "low-alpha edge ink must survive");
    }
    #[test]
    fn split_and_rotations_map_exact_pixels_and_inverse_coordinates() {
        let source =
            image::RgbaImage::from_fn(5, 2, |x, y| image::Rgba([(x + y * 5) as u8, 0, 0, 255]));
        let dimensions = PageDimensions {
            width: 5,
            height: 2,
        };
        for rtl in [false, true] {
            let options = DisplayOptions {
                split_wide: true,
                rotate_wide: true,
                rtl,
                ..DisplayOptions::default()
            };
            let p = PagePresentation::new(dimensions, CropRect::full(dimensions), options).unwrap();
            assert_eq!(p.rotation, PageRotation::None);
            assert_eq!(
                p.effective,
                PageDimensions {
                    width: 3,
                    height: 4
                }
            );
            let result = render_presentation(
                &source,
                p,
                DisplayWindowPlan {
                    width: 3,
                    full_height: 4,
                    first_row: 0,
                    rows: 4,
                },
                DecodeLimits::default(),
            )
            .unwrap();
            let expected = if rtl {
                [3, 4, 4, 8, 9, 9, 0, 1, 2, 5, 6, 7]
            } else {
                [0, 1, 2, 5, 6, 7, 3, 4, 4, 8, 9, 9]
            };
            assert_eq!(result.pixels().map(|p| p[0]).collect::<Vec<_>>(), expected);
            for y in 0..2 {
                for x in 0..5 {
                    let (dx, dy) = p.display_coordinate(f64::from(x), f64::from(y));
                    assert_eq!(p.source_coordinate(dx, dy), (f64::from(x), f64::from(y)));
                }
            }
        }
        for flip in [false, true] {
            let options = DisplayOptions {
                rotate_wide: true,
                flip_wide: flip,
                ..DisplayOptions::default()
            };
            let p = PagePresentation::new(dimensions, CropRect::full(dimensions), options).unwrap();
            let result = render_presentation(
                &source,
                p,
                DisplayWindowPlan {
                    width: 2,
                    full_height: 5,
                    first_row: 0,
                    rows: 5,
                },
                DecodeLimits::default(),
            )
            .unwrap();
            assert_eq!(
                result,
                if flip {
                    image::imageops::rotate270(&source)
                } else {
                    image::imageops::rotate90(&source)
                }
            );
            for y in 0..2 {
                for x in 0..5 {
                    let (dx, dy) = p.display_coordinate(f64::from(x), f64::from(y));
                    assert_eq!(p.source_coordinate(dx, dy), (f64::from(x), f64::from(y)));
                }
            }
        }
    }
    #[test]
    fn reverse_composes_crop_split_rotation_filters_and_inverse_coordinates() {
        let original = image::RgbaImage::from_fn(12, 8, |x, y| {
            if (2..10).contains(&x) && (2..6).contains(&y) {
                image::Rgba([(x + y * 12) as u8, 40, 100, 127])
            } else {
                image::Rgba([255; 4])
            }
        });
        let bytes = encoded(&original);
        let mut decoder = DisplayDecoder::default();
        for (split, rotate, rtl) in [
            (false, false, false),
            (true, false, false),
            (true, true, true),
            (false, true, false),
            (false, true, true),
        ] {
            let base = DisplayOptions {
                crop_borders: true,
                split_wide: split,
                rotate_wide: rotate,
                rtl,
                color_filter: true,
                filter_color: 0x122fbc,
                filter_intensity: 25,
                invert_colors: true,
                ..DisplayOptions::default()
            };
            let normal = decoder
                .decode_window_with_options(
                    &bytes,
                    DecodeLimits::default(),
                    20,
                    0.,
                    1.,
                    10000,
                    base,
                )
                .unwrap();
            let options = DisplayOptions {
                reverse_page: true,
                ..base
            };
            let reversed = decoder
                .decode_window_with_options(
                    &bytes,
                    DecodeLimits::default(),
                    20,
                    0.,
                    1.,
                    10000,
                    options,
                )
                .unwrap();
            assert_eq!(reversed.image, image::imageops::rotate180(&normal.image));
            let presentation = decoder
                .presentation(&bytes, DecodeLimits::default(), options)
                .unwrap();
            assert!(!presentation.is_identity());
            for y in 2..6 {
                for x in 2..10 {
                    let (dx, dy) = presentation.display_coordinate(f64::from(x), f64::from(y));
                    assert_eq!(
                        presentation.source_coordinate(dx, dy),
                        (f64::from(x), f64::from(y))
                    );
                }
            }
            let plan = DisplayWindowPlan {
                width: 3,
                full_height: presentation.effective.height * 3 / presentation.effective.width,
                first_row: 0,
                rows: presentation.effective.height * 3 / presentation.effective.width,
            };
            let full = render_presentation(&original, presentation, plan, DecodeLimits::default())
                .unwrap();
            if plan.rows > 1 {
                let partial = DisplayWindowPlan {
                    first_row: 1,
                    rows: plan.rows - 1,
                    ..plan
                };
                assert_eq!(
                    render_presentation(&original, presentation, partial, DecodeLimits::default())
                        .unwrap(),
                    image::imageops::crop_imm(&full, 0, 1, 3, partial.rows).to_image()
                );
            }
        }
    }
    #[test]
    fn filters_preserve_alpha_and_cached_original_across_settings_changes() {
        let original = image::RgbaImage::from_fn(4, 8, |x, y| {
            image::Rgba([20 + x as u8, 80, 160, y as u8 * 30])
        });
        let bytes = encoded(&original);
        let mut decoder = DisplayDecoder::default();
        let options = DisplayOptions {
            color_filter: true,
            filter_color: 0xff0000,
            filter_intensity: 50,
            ..DisplayOptions::default()
        };
        let tinted = decoder
            .decode_window_with_options(&bytes, DecodeLimits::default(), 4, 0., 1., 1000, options)
            .unwrap();
        assert_eq!(tinted.image.get_pixel(0, 1).0, [138, 40, 80, 30]);
        assert_eq!(
            decoder
                .presentation(&bytes, DecodeLimits::default(), options)
                .unwrap()
                .background_color,
            [138, 40, 80, 0]
        );
        let ptr = decoder.cached.as_ref().unwrap().image.as_ptr();
        let next = DisplayOptions {
            grayscale: true,
            invert_colors: true,
            ..DisplayOptions::default()
        };
        let filtered = decoder
            .decode_window_with_options(&bytes, DecodeLimits::default(), 4, 0., 1., 1000, next)
            .unwrap();
        assert_eq!(filtered.image.get_pixel(0, 1).0, [184, 184, 184, 30]);
        assert_eq!(decoder.cached.as_ref().unwrap().image.as_ptr(), ptr);
        assert_eq!(decoder.cached.as_ref().unwrap().image, original);
        let mut transparent = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 0]));
        apply_filters(
            &mut transparent,
            DisplayOptions {
                color_filter: true,
                filter_intensity: 255,
                filter_color: 0x123456,
                ..DisplayOptions::default()
            },
        );
        assert_eq!(transparent.get_pixel(0, 0).0, [0x12, 0x34, 0x56, 0]);
    }
    #[test]
    fn transformed_resampling_is_bit_exact_on_overlapping_windows_and_phase_bounded() {
        let original = image::RgbaImage::from_fn(43, 121, |x, y| {
            if !(3..40).contains(&x) || !(4..117).contains(&y) {
                image::Rgba([255; 4])
            } else {
                image::Rgba([
                    (x * 17 + y * 3) as u8,
                    (x + y * 13) as u8,
                    (x * 7 + y * 11) as u8,
                    (x * 5 + y) as u8,
                ])
            }
        });
        let dimensions = PageDimensions {
            width: 43,
            height: 121,
        };
        let p = PagePresentation::new(
            dimensions,
            detect_crop(&original),
            DisplayOptions {
                crop_borders: true,
                ..DisplayOptions::default()
            },
        )
        .unwrap();
        let full = DisplayWindowPlan {
            width: 19,
            full_height: p.effective.height * 19 / p.effective.width,
            first_row: 0,
            rows: p.effective.height * 19 / p.effective.width,
        };
        let expected = render_presentation(&original, p, full, DecodeLimits::default()).unwrap();
        for first_row in [0, 7, 19] {
            let plan = DisplayWindowPlan {
                first_row,
                rows: full.full_height - first_row,
                ..full
            };
            let result = render_presentation(&original, p, plan, DecodeLimits::default()).unwrap();
            assert_eq!(
                result,
                image::imageops::crop_imm(&expected, 0, first_row, 19, plan.rows).to_image()
            );
        }
        let limits = DecodeLimits {
            max_decoded_bytes: original.as_raw().len() as u64 + full.byte_len() as u64 - 1,
            ..DecodeLimits::default()
        };
        assert!(matches!(
            render_presentation(&original, p, full, limits),
            Err(Error::Limit)
        ));
        let bytes = encoded(&original);
        let mut decoder = DisplayDecoder::default();
        assert!(matches!(
            decoder.decode_window_with_options(
                &bytes,
                DecodeLimits::default(),
                19,
                0.,
                1.,
                1,
                DisplayOptions {
                    crop_borders: true,
                    ..DisplayOptions::default()
                }
            ),
            Err(Error::Limit)
        ));
    }
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

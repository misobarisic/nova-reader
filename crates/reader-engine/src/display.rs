//! Display admission is by visible rows, never by shrinking a long page's width.
use crate::{
    DecodeLimits, Error, PageDimensions, PageSource, Result, SharedPage, decode_image,
    inspect_image,
};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

pub const DISPLAY_WINDOW_BYTES: usize = 32 * 1024 * 1024;
const FULL_DISPLAY_BYTES: u64 = 64 * 1024 * 1024;
pub const DISPLAY_TILE_ROWS: u32 = 2048;
/// Upload granularity inside one aligned strip. A 2,048-row strip is about
/// 9 MiB at 1,150 px wide; mounting it as 512-row textures keeps each mid-scroll
/// GPU upload to a quarter of that. Must divide `DISPLAY_TILE_ROWS`.
pub const DISPLAY_UPLOAD_ROWS: u32 = 512;
const _: () = assert!(DISPLAY_TILE_ROWS.is_multiple_of(DISPLAY_UPLOAD_ROWS));
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
    let mut scan = CropScan::new(PageDimensions {
        width: original.width(),
        height: original.height(),
    });
    scan.rows(0, original);
    scan.finish()
}
/// Incremental form of the crop scan: rows may arrive in any number of bands,
/// in any order, and the result equals scanning the assembled page at once.
struct CropScan {
    full: CropRect,
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}
impl CropScan {
    fn new(dimensions: PageDimensions) -> Self {
        let full = CropRect::full(dimensions);
        Self {
            full,
            left: full.width,
            top: full.height,
            right: 0,
            bottom: 0,
        }
    }
    /// Fold in `band`, whose first row is page row `first_row`.
    fn rows(&mut self, first_row: u32, band: &image::RgbaImage) {
        for (x, y, pixel) in band.enumerate_pixels() {
            if pixel[3] != 0 && pixel.0[..3].iter().any(|channel| *channel < 248) {
                let y = y + first_row;
                self.left = self.left.min(x);
                self.top = self.top.min(y);
                self.right = self.right.max(x + 1);
                self.bottom = self.bottom.max(y + 1);
            }
        }
    }
    fn finish(self) -> CropRect {
        let Self {
            full,
            left,
            top,
            right,
            bottom,
        } = self;
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

/// Decoded bands kept for one split page. Large enough to hold a viewport and
/// its read-ahead at common widths (about ten 2,048-row bands at 720 px);
/// admission also keeps cache plus output window within the decoded budget.
const BAND_CACHE_BYTES: u64 = 64 * 1024 * 1024;
/// What decoded bands may keep resident once a render has the ones it needs:
/// the reader's model budget. The rows a single rectangle samples may exceed it
/// for the moment they are needed (wide pages), but nothing beyond them stays
/// above it; the bands farthest from the rows just rendered are evicted first.
pub const BAND_RETAIN_BYTES: u64 = DISPLAY_WINDOW_BYTES as u64;

/// One image worker's most recent windowed original, separate from display
/// buffers. Moving a viewport must not decompress the same long page again.
///
/// The source's shared identity names the exact admitted response (including
/// source/authentication changes). A miss releases the old original before
/// decoding, so two originals never overlap within the decoder phase budget.
///
/// A source that reports a row granularity smaller than the page (a page
/// stored as independent strips) is not decoded whole when only rows of it are
/// shown: its bands are decoded on demand and kept in a small LRU, and the one
/// resampler samples rows across band seams. Anything that needs the full page
/// at once (rotation, spread splitting, reversal) decodes it whole instead.
#[derive(Default)]
pub struct DisplayDecoder {
    cached: Option<DecodedOriginal>,
    banded: Option<BandedOriginal>,
    counters: Arc<BandCounters>,
}

/// Bands decoded by one [`DisplayDecoder`] since it was created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BandStats {
    /// Decoded because a render, the background pixel or a crop scan needed
    /// them right now: the viewer waits for each one.
    pub on_demand: u64,
    /// Decoded by [`DisplayDecoder::read_ahead`] before any render asked.
    pub ahead: u64,
}
#[derive(Default)]
struct BandCounters {
    on_demand: std::sync::atomic::AtomicU64,
    ahead: std::sync::atomic::AtomicU64,
}
struct DecodedOriginal {
    source: Arc<dyn PageSource>,
    limits: DecodeLimits,
    image: image::RgbaImage,
    crop: Option<CropRect>,
}
struct Band {
    image: image::RgbaImage,
    used: u64,
}
struct BandedOriginal {
    source: Arc<dyn PageSource>,
    limits: DecodeLimits,
    dimensions: PageDimensions,
    /// Rows per band; every band but the last has exactly this many.
    rows: u32,
    bands: BTreeMap<u32, Band>,
    bytes: u64,
    clock: u64,
    crop: Option<CropRect>,
    background: Option<[u8; 4]>,
    counters: Arc<BandCounters>,
}
impl BandedOriginal {
    fn count(&self) -> u32 {
        self.dimensions.height.div_ceil(self.rows)
    }
    fn band_range(&self, band: u32) -> std::ops::Range<u32> {
        band * self.rows..((band + 1) * self.rows).min(self.dimensions.height)
    }
    fn band_bytes(&self, band: u32) -> u64 {
        let rows = self.band_range(band);
        u64::from(self.dimensions.width) * u64::from(rows.end - rows.start) * 4
    }
    /// Make every band in `needed` resident, evicting least recently used
    /// bands outside the range. The range itself must fit `budget`.
    fn load(&mut self, needed: std::ops::Range<u32>, budget: u64) -> Result<()> {
        let total = needed
            .clone()
            .map(|band| self.band_bytes(band))
            .sum::<u64>();
        if total > budget {
            return Err(Error::Limit);
        }
        for band in needed.clone() {
            self.clock += 1;
            if let Some(resident) = self.bands.get_mut(&band) {
                resident.used = self.clock;
                continue;
            }
            let size = self.band_bytes(band);
            while self.bytes + size > budget {
                let Some(oldest) = self
                    .bands
                    .iter()
                    .filter(|(index, _)| !needed.contains(index))
                    .min_by_key(|(_, band)| band.used)
                    .map(|(index, _)| *index)
                else {
                    break;
                };
                if let Some(old) = self.bands.remove(&oldest) {
                    self.bytes -= old.image.as_raw().len() as u64;
                }
            }
            let rows = self.band_range(band);
            let image = self.source.decode_rows(rows.clone(), self.limits)?;
            if image.width() != self.dimensions.width || image.height() != rows.end - rows.start {
                return Err(Error::Layout);
            }
            self.bytes += image.as_raw().len() as u64;
            self.counters
                .on_demand
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.bands.insert(
                band,
                Band {
                    image,
                    used: self.clock,
                },
            );
        }
        if self.bytes > BAND_RETAIN_BYTES {
            let keep = needed.clone().collect::<Vec<_>>();
            let middle = (needed.start + needed.end) * self.rows / 2;
            while self.bytes > BAND_RETAIN_BYTES && self.evict_farthest(&keep, middle) {}
        }
        Ok(())
    }
    /// Evict the resident band outside `keep` that is farthest from source row
    /// `edge`. `false` when nothing outside `keep` is resident.
    fn evict_farthest(&mut self, keep: &[u32], edge: u32) -> bool {
        let rows = self.rows;
        let Some(far) = self
            .bands
            .keys()
            .copied()
            .filter(|band| !keep.contains(band))
            .max_by_key(|band| (band * rows + rows / 2).abs_diff(edge))
        else {
            return false;
        };
        if let Some(old) = self.bands.remove(&far) {
            self.bytes -= old.image.as_raw().len() as u64;
        }
        true
    }
    /// Pixel (0, 0): the page's background colour source.
    fn background(&mut self, budget: u64) -> Result<[u8; 4]> {
        if let Some(background) = self.background {
            return Ok(background);
        }
        self.load(0..1, budget)?;
        let pixel = self.bands[&0].image.get_pixel(0, 0).0;
        self.background = Some(pixel);
        Ok(pixel)
    }
    /// Scan every band once, in order, without holding more than the cache.
    fn crop(&mut self, budget: u64) -> Result<CropRect> {
        if let Some(crop) = self.crop {
            return Ok(crop);
        }
        let mut scan = CropScan::new(self.dimensions);
        for band in 0..self.count() {
            self.load(band..band + 1, budget)?;
            scan.rows(self.band_range(band).start, &self.bands[&band].image);
        }
        let crop = scan.finish();
        self.crop = Some(crop);
        Ok(crop)
    }
}
fn banded_rows(source: &dyn PageSource, dimensions: PageDimensions) -> Option<u32> {
    source
        .row_granularity()
        .filter(|rows| *rows > 0 && *rows < dimensions.height)
}
/// Whether display rows are source rows (possibly cropped and resampled), so a
/// window can be sampled from bands.
fn row_contiguous(presentation: &PagePresentation) -> bool {
    !presentation.split && !presentation.reverse_page && presentation.rotation == PageRotation::None
}
/// What to decode ahead of a published window: the bands behind the rows just
/// beyond its leading edge, so the next window shift finds them resident.
#[derive(Clone, Copy, Debug)]
pub struct ReadAhead {
    /// Target output width the window was planned for.
    pub width: u32,
    pub options: DisplayOptions,
    /// The window the viewer currently holds.
    pub window: DisplayWindowPlan,
    /// Scroll direction: ahead is above the window when set.
    pub backwards: bool,
    /// Output rows beyond the window edge to cover (about one to two viewport
    /// heights).
    pub depth_rows: u32,
    /// Most band bytes read-ahead may leave resident. Bands outside the wanted
    /// range are evicted, farthest from the window edge first, to stay within it.
    pub max_bytes: u64,
}
/// Result of one [`DisplayDecoder::read_ahead`] step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadAheadStep {
    /// One band was decoded; call again for the next.
    Decoded,
    /// Every band in range is resident (or does not fit the budget).
    Done,
    /// Not a banded page held by this decoder, a stale window, or a
    /// presentation that needs the whole page: nothing to read ahead.
    Unavailable,
}
impl DisplayDecoder {
    /// Bands decoded so far, split into those a render waited for and those
    /// decoded ahead of time.
    pub fn band_stats(&self) -> BandStats {
        use std::sync::atomic::Ordering::Relaxed;
        BandStats {
            on_demand: self.counters.on_demand.load(Relaxed),
            ahead: self.counters.ahead.load(Relaxed),
        }
    }
    /// Bands and bytes of the current banded page that are resident.
    pub fn resident_bands(&self) -> (usize, u64) {
        self.banded
            .as_ref()
            .map_or((0, 0), |banded| (banded.bands.len(), banded.bytes))
    }
    /// Decode at most one band of a split page that the next window shift will
    /// need, in the scroll direction and nearest first. Only a page this
    /// decoder already holds banded is read ahead; the caller decides when the
    /// viewer is idle and calls again while it returns `Decoded`. The window's
    /// own pixels were rendered already, so bands behind the edge are only
    /// kept while the budget allows: the farthest from the edge go first.
    pub fn read_ahead(
        &mut self,
        source: &impl SharedPage,
        limits: DecodeLimits,
        request: ReadAhead,
    ) -> Result<ReadAheadStep> {
        if self.holds_whole(source, limits) || !self.holds_bands(source, limits) {
            return Ok(ReadAheadStep::Unavailable);
        }
        let presentation = self.presentation(source, limits, request.options)?;
        let plan = request.window;
        let effective = presentation.effective;
        let width = request.width.min(effective.width).max(1);
        let full_height = (u64::from(effective.height) * u64::from(width)
            / u64::from(effective.width))
        .max(1) as u32;
        if !row_contiguous(&presentation)
            || plan.width != width
            || plan.full_height != full_height
            || plan.rows == 0
            || plan.first_row + plan.rows > full_height
        {
            return Ok(ReadAheadStep::Unavailable);
        }
        let (low, high) = if request.backwards {
            (
                plan.first_row.saturating_sub(request.depth_rows),
                plan.first_row,
            )
        } else {
            let edge = plan.first_row + plan.rows;
            (
                edge,
                edge.saturating_add(request.depth_rows).min(full_height),
            )
        };
        if low >= high {
            return Ok(ReadAheadStep::Done);
        }
        let view = SourceView {
            x: presentation.crop_x,
            y: presentation.crop_y,
            width: effective.width,
            height: effective.height,
        };
        let (first, last) = source_span(view, plan, low, high - low);
        let edge_row = if request.backwards { last } else { first };
        let budget = request.max_bytes.min(band_budget(limits, plan));
        let banded = self.banded.as_mut().unwrap();
        let count = banded.count();
        let mut order =
            (first / banded.rows..=(last / banded.rows).min(count - 1)).collect::<Vec<_>>();
        if request.backwards {
            order.reverse();
        }
        // Wanted bands, nearest first, as many as the budget holds.
        let mut wanted = Vec::new();
        let mut total = 0;
        for band in order {
            total += banded.band_bytes(band);
            if total > budget {
                break;
            }
            wanted.push(band);
        }
        // The budget holds for resident bands too, not only for new ones.
        while banded.bytes > budget && banded.evict_farthest(&wanted, edge_row) {}
        for band in wanted.iter().copied() {
            banded.clock += 1;
            if let Some(resident) = banded.bands.get_mut(&band) {
                resident.used = banded.clock;
                continue;
            }
            let size = banded.band_bytes(band);
            while banded.bytes + size > budget {
                if !banded.evict_farthest(&wanted, edge_row) {
                    return Ok(ReadAheadStep::Done);
                }
            }
            let rows = banded.band_range(band);
            let image = banded.source.decode_rows(rows.clone(), banded.limits)?;
            if image.width() != banded.dimensions.width || image.height() != rows.end - rows.start {
                return Err(Error::Layout);
            }
            banded.bytes += image.as_raw().len() as u64;
            banded
                .counters
                .ahead
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            banded.bands.insert(
                band,
                Band {
                    image,
                    used: banded.clock,
                },
            );
            return Ok(ReadAheadStep::Decoded);
        }
        Ok(ReadAheadStep::Done)
    }

    /// Release pixels and their response on reader close, generation changes,
    /// or source revocation, including while the worker has no queued images.
    pub fn clear(&mut self) {
        self.cached = None;
        self.banded = None;
    }

    fn holds_whole(&self, source: &impl SharedPage, limits: DecodeLimits) -> bool {
        self.cached.as_ref().is_some_and(|cached| {
            Arc::as_ptr(&cached.source).cast::<()>() == source.identity() && cached.limits == limits
        })
    }
    fn holds_bands(&self, source: &impl SharedPage, limits: DecodeLimits) -> bool {
        self.banded.as_ref().is_some_and(|banded| {
            Arc::as_ptr(&banded.source).cast::<()>() == source.identity() && banded.limits == limits
        })
    }

    fn ensure_original(&mut self, source: &impl SharedPage, limits: DecodeLimits) -> Result<()> {
        if !self.holds_whole(source, limits) {
            self.cached = None;
            self.banded = None;
            self.cached = Some(DecodedOriginal {
                source: source.shared(),
                limits,
                image: source.page().decode(limits)?,
                crop: None,
            });
        }
        Ok(())
    }

    fn ensure_banded(
        &mut self,
        source: &impl SharedPage,
        limits: DecodeLimits,
        dimensions: PageDimensions,
        rows: u32,
    ) -> &mut BandedOriginal {
        if !self.holds_bands(source, limits) {
            self.cached = None;
            self.banded = Some(BandedOriginal {
                source: source.shared(),
                limits,
                dimensions,
                rows,
                bands: BTreeMap::new(),
                bytes: 0,
                clock: 0,
                crop: None,
                background: None,
                counters: self.counters.clone(),
            });
        }
        self.banded.as_mut().unwrap()
    }

    /// Obtain virtual geometry before constructing the viewport/window request.
    /// Cropping scans the admitted original once; filter-only changes reuse it.
    pub fn presentation(
        &mut self,
        source: &impl SharedPage,
        limits: DecodeLimits,
        options: DisplayOptions,
    ) -> Result<PagePresentation> {
        let dimensions = source.page().dimensions(limits)?;
        if !self.holds_whole(source, limits)
            && let Some(rows) = banded_rows(source.page(), dimensions)
        {
            let budget = BAND_CACHE_BYTES.min(limits.max_decoded_bytes / 2);
            let banded = self.ensure_banded(source, limits, dimensions, rows);
            let background = banded.background(budget)?;
            let crop = if options.crop_borders {
                banded.crop(budget)?
            } else {
                CropRect::full(dimensions)
            };
            let mut presentation = PagePresentation::new(dimensions, crop, options)?;
            let mut background = background;
            filter_pixel(&mut background, options);
            presentation.background_color = background;
            return Ok(presentation);
        }
        self.ensure_original(source, limits)?;
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
        source: &impl SharedPage,
        limits: DecodeLimits,
        options: DisplayOptions,
    ) -> Result<PageDimensions> {
        Ok(self.presentation(source, limits, options)?.effective)
    }

    pub fn decode_window(
        &mut self,
        source: &impl SharedPage,
        limits: DecodeLimits,
        target_width: u32,
        start: f64,
        end: f64,
        max_bytes: usize,
    ) -> Result<DisplayWindow> {
        self.decode_window_with_options(
            source,
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
        source: &impl SharedPage,
        limits: DecodeLimits,
        target_width: u32,
        start: f64,
        end: f64,
        max_bytes: usize,
        options: DisplayOptions,
    ) -> Result<DisplayWindow> {
        let presentation = self.presentation(source, limits, options)?;
        let plan =
            plan_display_window(presentation.effective, target_width, start, end, max_bytes)?;
        if self.sample_bands(source, limits, &presentation) {
            let budget = band_budget(limits, plan);
            let banded = self.banded.as_mut().unwrap();
            let renderer = WindowRenderer {
                original: RenderSource::Banded(RefCell::new(banded), budget),
                presentation,
                options,
                plan,
            };
            let mut image = image::RgbaImage::new(plan.width, plan.rows);
            renderer.render_rect(plan.first_row, plan.rows, 0, plan.width, image.as_mut())?;
            return Ok(DisplayWindow { image, plan });
        }
        self.ensure_original(source, limits)?;
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

    /// Whether this window is sampled from bands rather than a whole decode.
    /// A page already decoded whole keeps using that decode.
    fn sample_bands(
        &self,
        source: &impl SharedPage,
        limits: DecodeLimits,
        presentation: &PagePresentation,
    ) -> bool {
        !self.holds_whole(source, limits)
            && self.holds_bands(source, limits)
            && row_contiguous(presentation)
    }

    /// Plan the same window as `decode_window_with_options`, but render nothing
    /// up front. `render` receives a [`WindowRenderer`] that samples any
    /// sub-rectangle of the window straight into a caller-owned buffer (for
    /// example a GPU upload buffer), so a one-strip viewport shift pays only for
    /// the strip that actually enters. Pixels are identical to the full window.
    /// A split page decodes only the bands those rectangles touch.
    #[allow(clippy::too_many_arguments)]
    pub fn render_window_tiles<R>(
        &mut self,
        source: &impl SharedPage,
        limits: DecodeLimits,
        target_width: u32,
        start: f64,
        end: f64,
        max_bytes: usize,
        options: DisplayOptions,
        render: impl FnOnce(&WindowRenderer<'_>) -> Result<R>,
    ) -> Result<(DisplayWindowPlan, R)> {
        let presentation = self.presentation(source, limits, options)?;
        let plan =
            plan_display_window(presentation.effective, target_width, start, end, max_bytes)?;
        if self.sample_bands(source, limits, &presentation) {
            let budget = band_budget(limits, plan);
            let banded = self.banded.as_mut().unwrap();
            let renderer = WindowRenderer {
                original: RenderSource::Banded(RefCell::new(banded), budget),
                presentation,
                options,
                plan,
            };
            let result = render(&renderer)?;
            return Ok((plan, result));
        }
        self.ensure_original(source, limits)?;
        let original = &self.cached.as_ref().unwrap().image;
        // A whole-page native identity output used to hand over its allocation
        // without a second admission phase; keep that exemption.
        let whole_native =
            presentation.is_identity() && !options.has_filters() && is_original(plan, original);
        if !whole_native
            && (original.as_raw().len() as u64)
                .checked_add(plan.byte_len() as u64)
                .is_none_or(|n| n > limits.max_decoded_bytes)
        {
            return Err(Error::Limit);
        }
        let renderer = WindowRenderer {
            original: RenderSource::Whole(original),
            presentation,
            options,
            plan,
        };
        let result = render(&renderer)?;
        if whole_native {
            // Mirrors decode_window_with_options, which gives the original away.
            self.cached = None;
        }
        Ok((plan, result))
    }
}
/// Band budget that leaves room for the output window within the decoded cap.
fn band_budget(limits: DecodeLimits, plan: DisplayWindowPlan) -> u64 {
    BAND_CACHE_BYTES.min(
        limits
            .max_decoded_bytes
            .saturating_sub(plan.byte_len() as u64),
    )
}
enum RenderSource<'a> {
    Whole(&'a image::RgbaImage),
    /// Bands are loaded as rectangles ask for rows; the budget caps residency.
    Banded(RefCell<&'a mut BandedOriginal>, u64),
}
/// Samples rectangles of one planned display window out of a decoded original.
pub struct WindowRenderer<'a> {
    original: RenderSource<'a>,
    presentation: PagePresentation,
    options: DisplayOptions,
    plan: DisplayWindowPlan,
}
impl WindowRenderer<'_> {
    pub fn plan(&self) -> DisplayWindowPlan {
        self.plan
    }
    /// Render the output rectangle `[left, left + width) x [top, top + height)`,
    /// in full-page output coordinates, into tightly packed RGBA `out`. The
    /// rectangle must lie inside the planned window.
    pub fn render_rect(
        &self,
        top: u32,
        height: u32,
        left: u32,
        width: u32,
        out: &mut [u8],
    ) -> Result<()> {
        let plan = self.plan;
        let rect = Rect { top, left, width };
        if height == 0
            || width == 0
            || top < plan.first_row
            || u64::from(top) + u64::from(height) > u64::from(plan.first_row + plan.rows)
            || u64::from(left) + u64::from(width) > u64::from(plan.width)
            || out.len() != width as usize * height as usize * 4
        {
            return Err(Error::Layout);
        }
        match &self.original {
            RenderSource::Whole(original) => {
                if self.presentation.is_identity() {
                    render_rect(
                        &WholeRows::new(original),
                        SourceView::whole(original),
                        plan,
                        rect,
                        out,
                    );
                } else {
                    render_presentation_rect(original, self.presentation, plan, rect, out);
                }
            }
            RenderSource::Banded(banded, budget) => {
                let presentation = self.presentation;
                let view = SourceView {
                    x: presentation.crop_x,
                    y: presentation.crop_y,
                    width: presentation.effective.width,
                    height: presentation.effective.height,
                };
                let (first, last) = source_span(view, plan, top, height);
                let mut banded = banded.borrow_mut();
                let needed = first / banded.rows..last / banded.rows + 1;
                banded.load(needed.clone(), *budget)?;
                let rows = BandRows {
                    first_row: needed.start * banded.rows,
                    band_rows: banded.rows,
                    bands: needed.map(|band| &banded.bands[&band].image).collect(),
                };
                render_rect(&rows, view, plan, rect, out);
            }
        }
        if self.options.has_filters() {
            apply_filters_raw(out, self.options);
        }
        Ok(())
    }
}
/// First and last source rows (inclusive, in page coordinates) that sampling
/// the output rows `top..top + height` can read.
fn source_span(view: SourceView, plan: DisplayWindowPlan, top: u32, height: u32) -> (u32, u32) {
    if view.width == plan.width && view.height == plan.full_height {
        return (view.y + top, view.y + top + height - 1);
    }
    let (first, _, _) = y_tap(top, view.height, plan.full_height);
    let (_, last, _) = y_tap(top + height - 1, view.height, plan.full_height);
    (view.y + first, view.y + last)
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
    display_image(
        inspect_image(bytes, limits)?,
        limits,
        max_width,
        max_height,
        || decode_image(bytes, limits),
    )
}
/// [`decode_display_image`] for a page that may be stored in strips.
pub fn decode_display_page(
    page: &(impl PageSource + ?Sized),
    limits: DecodeLimits,
    max_width: u32,
    max_height: u32,
) -> Result<image::RgbaImage> {
    if max_width == 0 || max_height == 0 {
        return Err(Error::Limit);
    }
    display_image(
        page.dimensions(limits)?,
        limits,
        max_width,
        max_height,
        || page.decode(limits),
    )
}
fn display_image(
    dimensions: PageDimensions,
    limits: DecodeLimits,
    max_width: u32,
    max_height: u32,
    decode: impl FnOnce() -> Result<image::RgbaImage>,
) -> Result<image::RgbaImage> {
    let scale = (f64::from(max_width) / f64::from(dimensions.width))
        .min(f64::from(max_height) / f64::from(dimensions.height))
        .min(1.);
    let width = (f64::from(dimensions.width) * scale).floor().max(1.) as u32;
    let height = (f64::from(dimensions.height) * scale).floor().max(1.) as u32;
    if u64::from(width) * u64::from(height) * 4 > FULL_DISPLAY_BYTES {
        return Err(Error::Limit);
    }
    render(
        decode()?,
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
        // Native rows: one allocation-and-copy, no zero fill first.
        let stride = plan.width as usize * 4;
        let start = plan.first_row as usize * stride;
        let bytes = original.as_raw()[start..start + plan.byte_len()].to_vec();
        return image::RgbaImage::from_raw(plan.width, plan.rows, bytes).ok_or(Error::Limit);
    }
    let mut image = image::RgbaImage::new(plan.width, plan.rows);
    render_rect(
        &WholeRows::new(original),
        SourceView::whole(original),
        plan,
        Rect {
            top: plan.first_row,
            left: 0,
            width: plan.width,
        },
        image.as_mut(),
    );
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
    render_presentation_rect(
        original,
        presentation,
        plan,
        Rect {
            top: plan.first_row,
            left: 0,
            width: plan.width,
        },
        output.as_mut(),
    );
    Ok(output)
}

/// Output rectangle in full-page output coordinates.
#[derive(Clone, Copy)]
struct Rect {
    top: u32,
    left: u32,
    width: u32,
}
/// A rectangle of the decoded original that forms the (untransformed) virtual
/// page: the whole image, or the crop of a page with only cropping applied.
#[derive(Clone, Copy)]
struct SourceView {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}
impl SourceView {
    fn whole(original: &image::RgbaImage) -> Self {
        Self {
            x: 0,
            y: 0,
            width: original.width(),
            height: original.height(),
        }
    }
}
/// Bilinear weights are 8-bit fixed point: 256 is exactly the second sample.
const WEIGHT_ONE: u32 = 256;
fn weight(fraction: f64) -> u32 {
    (fraction * f64::from(WEIGHT_ONE)).round() as u32
}
/// Horizontal sample positions, shared by every output row of a rectangle.
struct XTaps {
    x0: Vec<u32>,
    x1: Vec<u32>,
    weight: Vec<u32>,
}
impl XTaps {
    /// Source columns for output columns `left..left + width`, using the FULL
    /// page's coordinate transform so tiles agree on shared pixels.
    fn new(source_width: u32, output_width: u32, left: u32, width: u32) -> Self {
        let mut taps = Self {
            x0: Vec::with_capacity(width as usize),
            x1: Vec::with_capacity(width as usize),
            weight: Vec::with_capacity(width as usize),
        };
        for x in left..left + width {
            let sx = ((f64::from(x) + 0.5) * f64::from(source_width) / f64::from(output_width)
                - 0.5)
                .clamp(0., f64::from(source_width - 1));
            let x0 = sx.floor() as u32;
            taps.x0.push(x0);
            taps.x1.push((x0 + 1).min(source_width - 1));
            taps.weight.push(weight(sx - f64::from(x0)));
        }
        taps
    }
}
/// Vertical sample position of one output row.
fn y_tap(row: u32, source_height: u32, output_height: u32) -> (u32, u32, u32) {
    let sy = ((f64::from(row) + 0.5) * f64::from(source_height) / f64::from(output_height) - 0.5)
        .clamp(0., f64::from(source_height - 1));
    let y0 = sy.floor() as u32;
    (
        y0,
        (y0 + 1).min(source_height - 1),
        weight(sy - f64::from(y0)),
    )
}
#[inline(always)]
fn blend(a: &[u8], b: &[u8], c: &[u8], d: &[u8], wx: u32, wy: u32, out: &mut [u8]) {
    for i in 0..4 {
        let top = u32::from(a[i]) * (WEIGHT_ONE - wx) + u32::from(b[i]) * wx;
        let bottom = u32::from(c[i]) * (WEIGHT_ONE - wx) + u32::from(d[i]) * wx;
        out[i] = vertical(top, bottom, wy);
    }
}
/// Combine two horizontally interpolated samples (8.8 fixed point) vertically.
#[inline(always)]
fn vertical(top: u32, bottom: u32, wy: u32) -> u8 {
    ((top * (WEIGHT_ONE - wy) + bottom * wy + WEIGHT_ONE * WEIGHT_ONE / 2)
        / (WEIGHT_ONE * WEIGHT_ONE)) as u8
}
/// Rows of a decoded original, addressed by absolute page row. A whole decode
/// and a set of resident bands present the same rows, so one resampler serves
/// both and a seam between bands samples exactly what the whole page would.
trait RowSource {
    /// All `width * 4` bytes of page row `y`.
    fn row(&self, y: u32) -> &[u8];
    /// The tightly packed whole page, when it is one allocation.
    fn contiguous(&self) -> Option<&[u8]> {
        None
    }
}
struct WholeRows<'a> {
    raw: &'a [u8],
    stride: usize,
}
impl<'a> WholeRows<'a> {
    fn new(original: &'a image::RgbaImage) -> Self {
        Self {
            raw: original.as_raw(),
            stride: original.width() as usize * 4,
        }
    }
}
impl RowSource for WholeRows<'_> {
    fn row(&self, y: u32) -> &[u8] {
        &self.raw[y as usize * self.stride..][..self.stride]
    }
    fn contiguous(&self) -> Option<&[u8]> {
        Some(self.raw)
    }
}
/// Consecutive resident bands starting at page row `first_row`.
struct BandRows<'a> {
    first_row: u32,
    band_rows: u32,
    bands: Vec<&'a image::RgbaImage>,
}
impl RowSource for BandRows<'_> {
    fn row(&self, y: u32) -> &[u8] {
        let local = y - self.first_row;
        let band = self.bands[(local / self.band_rows) as usize];
        let stride = band.width() as usize * 4;
        &band.as_raw()[(local % self.band_rows) as usize * stride..][..stride]
    }
}
/// Horizontally interpolated source rows. Neighbouring output rows of a
/// downsampled page share source rows, so the last two are kept.
struct HorizontalRows<'a, R: RowSource> {
    source: &'a R,
    view: SourceView,
    taps: XTaps,
    rows: [(u32, Vec<u16>); 2],
}
impl<'a, R: RowSource> HorizontalRows<'a, R> {
    const NONE: u32 = u32::MAX;
    fn new(source: &'a R, view: SourceView, taps: XTaps) -> Self {
        let len = taps.x0.len() * 4;
        Self {
            source,
            view,
            taps,
            rows: [(Self::NONE, vec![0; len]), (Self::NONE, vec![0; len])],
        }
    }
    /// Slot holding `y`, filling one other than `keep` on a miss.
    fn slot(&mut self, y: u32, keep: Option<usize>) -> usize {
        if let Some(found) = self.rows.iter().position(|(row, _)| *row == y) {
            return found;
        }
        let slot = usize::from(keep == Some(0));
        let source = self.source.row(self.view.y + y);
        let (row, target) = &mut self.rows[slot];
        *row = y;
        for (x, pixel) in target.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let wx = self.taps.weight[x];
            let a = &source[(self.view.x + self.taps.x0[x]) as usize * 4..][..4];
            let b = &source[(self.view.x + self.taps.x1[x]) as usize * 4..][..4];
            for i in 0..4 {
                pixel[i] = (u32::from(a[i]) * (WEIGHT_ONE - wx) + u32::from(b[i]) * wx) as u16;
            }
        }
        slot
    }
    fn blend_row(&mut self, y0: u32, y1: u32, wy: u32, out: &mut [u8]) {
        let first = self.slot(y0, None);
        let second = if y1 == y0 {
            first
        } else {
            self.slot(y1, Some(first))
        };
        let (top, bottom) = (&self.rows[first].1, &self.rows[second].1);
        for ((out, top), bottom) in out.iter_mut().zip(top).zip(bottom) {
            *out = vertical(u32::from(*top), u32::from(*bottom), wy);
        }
    }
}
/// Sample a rectangle of an untransformed (identity or crop-only) page.
/// Bilinear sampling uses the FULL page's coordinate transform, so two windows
/// or tiles sample identical source pixels at a shared row. No giant float
/// intermediate, nearest-neighbour aliasing, or artificial source upscaling.
fn render_rect<R: RowSource>(
    source: &R,
    view: SourceView,
    plan: DisplayWindowPlan,
    rect: Rect,
    out: &mut [u8],
) {
    let out_stride = rect.width as usize * 4;
    if view.width == plan.width && view.height == plan.full_height {
        // Native rows stay contiguous: copy directly.
        if let Some(raw) = source.contiguous()
            && view.x == 0
            && rect.left == 0
            && rect.width == plan.width
            && raw.len() as u64 == u64::from(plan.width) * u64::from(view.y + view.height) * 4
        {
            let start = (view.y + rect.top) as usize * out_stride;
            out.copy_from_slice(&raw[start..start + out.len()]);
            return;
        }
        for (row, target) in out.chunks_exact_mut(out_stride).enumerate() {
            let line = source.row(view.y + rect.top + row as u32);
            let start = (view.x + rect.left) as usize * 4;
            target.copy_from_slice(&line[start..start + out_stride]);
        }
        return;
    }
    let mut rows = HorizontalRows::new(
        source,
        view,
        XTaps::new(view.width, plan.width, rect.left, rect.width),
    );
    for (row, target) in out.chunks_exact_mut(out_stride).enumerate() {
        // Source row selection is shared by every pixel in this output row.
        let (y0, y1, wy) = y_tap(rect.top + row as u32, view.height, plan.full_height);
        rows.blend_row(y0, y1, wy, target);
    }
}
fn render_presentation_rect(
    original: &image::RgbaImage,
    presentation: PagePresentation,
    plan: DisplayWindowPlan,
    rect: Rect,
    out: &mut [u8],
) {
    // Cropped rows remain contiguous: skip the per-pixel transform.
    if !presentation.split
        && !presentation.reverse_page
        && presentation.rotation == PageRotation::None
    {
        render_rect(
            &WholeRows::new(original),
            SourceView {
                x: presentation.crop_x,
                y: presentation.crop_y,
                width: presentation.effective.width,
                height: presentation.effective.height,
            },
            plan,
            rect,
            out,
        );
        return;
    }
    let width = presentation.effective.width;
    let height = presentation.effective.height;
    let taps = XTaps::new(width, plan.width, rect.left, rect.width);
    for (row, target) in out.chunks_exact_mut(rect.width as usize * 4).enumerate() {
        let (y0, mut y1, wy) = y_tap(rect.top + row as u32, height, plan.full_height);
        // Clamp interpolation at the split seam: separate paper halves must
        // never mix the bottom of one page with the top of its neighbour.
        if presentation.split && y0 < presentation.half_height {
            y1 = y1.min(presentation.half_height - 1);
        }
        for (x, pixel) in target.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let sample = |x, y| {
                let (sx, sy) = presentation.source_pixel(x, y);
                &original.as_raw()[(sy as usize * original.width() as usize + sx as usize) * 4..]
            };
            blend(
                sample(taps.x0[x], y0),
                sample(taps.x1[x], y0),
                sample(taps.x0[x], y1),
                sample(taps.x1[x], y1),
                taps.weight[x],
                wy,
                pixel,
            );
        }
    }
}
fn apply_filters(image: &mut image::RgbaImage, options: DisplayOptions) {
    apply_filters_raw(image.as_mut(), options);
}
fn apply_filters_raw(pixels: &mut [u8], options: DisplayOptions) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        filter_pixel(pixel, options);
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
            // Weights are 8-bit fixed point (1/256 steps) rather than f64, so a
            // sample may differ from the exact formula by one level, never more.
            let actual = render_window(&original, plan, DecodeLimits::default()).unwrap();
            assert_eq!(actual.dimensions(), expected.dimensions());
            let worst = actual
                .as_raw()
                .iter()
                .zip(expected.as_raw())
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(worst <= 1, "width {width}: fixed point drifted by {worst}");
        }
    }
    #[test]
    fn window_renderer_tiles_match_the_full_window_for_every_presentation() {
        let original = image::RgbaImage::from_fn(61, 140, |x, y| {
            image::Rgba([
                (x * 37 + y * 5) as u8,
                (x * 3 + y * 29) as u8,
                (x * 11 + y * 7) as u8,
                255 - (x + y) as u8,
            ])
        });
        let bytes = encoded(&original);
        let limits = DecodeLimits::default();
        let variants = [
            DisplayOptions::default(),
            DisplayOptions {
                grayscale: true,
                invert_colors: true,
                ..Default::default()
            },
            DisplayOptions {
                crop_borders: true,
                ..Default::default()
            },
            DisplayOptions {
                split_wide: true,
                rtl: true,
                reverse_page: true,
                ..Default::default()
            },
            DisplayOptions {
                rotate_wide: true,
                flip_wide: true,
                ..Default::default()
            },
        ];
        // The wide variants need a landscape source.
        let wide = image::RgbaImage::from_fn(140, 61, |x, y| *original.get_pixel(y, x));
        let wide_bytes = encoded(&wide);
        for (options, bytes) in variants
            .iter()
            .flat_map(|options| [(*options, bytes.clone()), (*options, wide_bytes.clone())])
        {
            for width in [23, 140] {
                let mut decoder = DisplayDecoder::default();
                let window = decoder
                    .decode_window_with_options(&bytes, limits, width, 0.2, 0.8, 1 << 20, options)
                    .unwrap();
                let plan = window.plan;
                let (tiled_plan, tiles) = decoder
                    .render_window_tiles(&bytes, limits, width, 0.2, 0.8, 1 << 20, options, |r| {
                        // Awkward rectangles, including a column split and a
                        // single row, must agree with the whole-window render.
                        let mut tiles = Vec::new();
                        for (top, height, left, width) in [
                            (plan.first_row, plan.rows, 0, plan.width),
                            (plan.first_row + 1, 1, 0, plan.width),
                            (
                                plan.first_row + plan.rows / 2,
                                plan.rows - plan.rows / 2,
                                1,
                                plan.width - 2,
                            ),
                            (plan.first_row, 3.min(plan.rows), plan.width - 1, 1),
                        ] {
                            let mut out = vec![0; width as usize * height as usize * 4];
                            r.render_rect(top, height, left, width, &mut out)?;
                            tiles.push((top, height, left, width, out));
                        }
                        let mut out = [0; 4];
                        assert!(
                            r.render_rect(plan.first_row + plan.rows, 1, 0, 1, &mut out)
                                .is_err()
                        );
                        assert!(
                            r.render_rect(plan.first_row, 1, plan.width, 1, &mut out)
                                .is_err()
                        );
                        assert!(
                            r.render_rect(plan.first_row, 1, 0, 1, &mut out[..3])
                                .is_err()
                        );
                        Ok(tiles)
                    })
                    .unwrap();
                assert_eq!(tiled_plan, plan);
                for (top, height, left, width, out) in tiles {
                    let expected = image::imageops::crop_imm(
                        &window.image,
                        left,
                        top - plan.first_row,
                        width,
                        height,
                    )
                    .to_image();
                    assert_eq!(out, expected.into_raw(), "{options:?} {width}");
                }
            }
        }
    }
    #[test]
    fn tile_rendering_keeps_window_admission_and_releases_whole_native_pages() {
        let original = image::RgbaImage::from_fn(32, 40, pattern);
        let bytes = encoded(&original);
        let limits = DecodeLimits::default();
        let mut decoder = DisplayDecoder::default();
        let (plan, copy) = decoder
            .render_window_tiles(
                &bytes,
                limits,
                32,
                0.,
                1.,
                1 << 20,
                DisplayOptions::default(),
                |r| {
                    let mut out = vec![0; 32 * 40 * 4];
                    r.render_rect(0, 40, 0, 32, &mut out)?;
                    Ok(out)
                },
            )
            .unwrap();
        assert_eq!((plan.first_row, plan.rows), (0, 40));
        assert_eq!(copy, original.as_raw().to_vec());
        assert!(
            decoder.cached.is_none(),
            "a whole native page releases its original like decode_window"
        );
        // A resampled window is admitted against original + whole window bytes,
        // exactly as in decode_window_with_options, even though only tiles render.
        let tight = DecodeLimits {
            max_decoded_bytes: original.as_raw().len() as u64 + 16 * 20 * 4 - 1,
            ..limits
        };
        assert!(matches!(
            decoder.render_window_tiles(
                &bytes,
                tight,
                16,
                0.,
                1.,
                1 << 20,
                DisplayOptions::default(),
                |_| Ok(())
            ),
            Err(Error::Limit)
        ));
        assert!(decoder.cached.is_some());
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

    /// A page stored as independent PNG strips, counting what is decoded.
    struct Strips {
        dimensions: PageDimensions,
        rows: u32,
        strips: Vec<Vec<u8>>,
        decoded: std::sync::Mutex<Vec<u32>>,
        whole: std::sync::atomic::AtomicUsize,
    }
    impl Strips {
        fn new(image: &image::RgbaImage, rows: u32) -> Arc<Self> {
            let strips = (0..image.height().div_ceil(rows))
                .map(|band| {
                    let top = band * rows;
                    let height = rows.min(image.height() - top);
                    let strip =
                        image::imageops::crop_imm(image, 0, top, image.width(), height).to_image();
                    let mut bytes = std::io::Cursor::new(Vec::new());
                    strip.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
                    bytes.into_inner()
                })
                .collect();
            Arc::new(Self {
                dimensions: PageDimensions {
                    width: image.width(),
                    height: image.height(),
                },
                rows,
                strips,
                decoded: Default::default(),
                whole: Default::default(),
            })
        }
        fn decoded(&self) -> Vec<u32> {
            let mut bands = self.decoded.lock().unwrap().clone();
            bands.sort();
            bands
        }
    }
    impl PageSource for Strips {
        fn dimensions(&self, _: DecodeLimits) -> Result<PageDimensions> {
            Ok(self.dimensions)
        }
        fn encoded_len(&self) -> usize {
            self.strips.iter().map(Vec::len).sum()
        }
        fn row_granularity(&self) -> Option<u32> {
            Some(self.rows)
        }
        fn decode_rows(
            &self,
            rows: std::ops::Range<u32>,
            limits: DecodeLimits,
        ) -> Result<image::RgbaImage> {
            assert_eq!(rows.start % self.rows, 0, "bands start on the grid");
            let band = rows.start / self.rows;
            self.decoded.lock().unwrap().push(band);
            let image = decode_image(&self.strips[band as usize], limits)?;
            assert_eq!(image.height(), rows.end - rows.start);
            Ok(image)
        }
        fn decode(&self, limits: DecodeLimits) -> Result<image::RgbaImage> {
            self.whole.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut page = image::RgbaImage::new(self.dimensions.width, self.dimensions.height);
            for (band, strip) in self.strips.iter().enumerate() {
                let strip = decode_image(strip, limits)?;
                image::imageops::replace(&mut page, &strip, 0, i64::from(band as u32 * self.rows));
            }
            Ok(page)
        }
    }
    fn textured(width: u32, height: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(width, height, |x, y| {
            let n = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503) ^ (y / 7);
            image::Rgba([
                n as u8,
                (n >> 8) as u8,
                (n >> 16) as u8,
                255 - (y % 5) as u8,
            ])
        })
    }
    fn dyn_source(strips: &Arc<Strips>) -> Arc<dyn PageSource> {
        strips.clone()
    }

    #[test]
    fn banded_windows_decode_only_the_bands_they_sample() {
        let page = textured(40, 1000);
        let strips = Strips::new(&page, 128);
        let source = dyn_source(&strips);
        let mut decoder = DisplayDecoder::default();
        let (plan, ()) = decoder
            .render_window_tiles(
                &source,
                DecodeLimits::default(),
                40,
                0.5,
                0.6,
                1 << 20,
                DisplayOptions::default(),
                |renderer| {
                    // Rows 520..580 sit inside band 4 (512..640).
                    let mut out = vec![0; 40 * 60 * 4];
                    renderer.render_rect(520, 60, 0, 40, &mut out)?;
                    assert_eq!(out, page.as_raw()[520 * 160..580 * 160]);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(plan.full_height, 1000);
        // Band 0 supplies the background colour; band 4 the rows.
        assert_eq!(strips.decoded(), vec![0, 4]);
        assert_eq!(strips.whole.load(std::sync::atomic::Ordering::SeqCst), 0);
        // A rectangle across a seam needs both neighbours, and only those.
        decoder
            .render_window_tiles(
                &source,
                DecodeLimits::default(),
                40,
                0.5,
                0.6,
                1 << 20,
                DisplayOptions::default(),
                |renderer| {
                    let mut out = vec![0; 40 * 20 * 4];
                    renderer.render_rect(630, 20, 0, 40, &mut out)?;
                    assert_eq!(out, page.as_raw()[630 * 160..650 * 160]);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(strips.decoded(), vec![0, 4, 5], "band 4 stayed cached");
    }

    #[test]
    fn banded_and_whole_sources_render_identical_bytes() {
        let page = textured(37, 900);
        let strips = Strips::new(&page, 128);
        let banded = dyn_source(&strips);
        let whole: Arc<dyn PageSource> = encoded(&page);
        let limits = DecodeLimits::default();
        let filters = DisplayOptions {
            color_filter: true,
            filter_color: 0x336699,
            filter_intensity: 30,
            invert_colors: true,
            ..Default::default()
        };
        for options in [DisplayOptions::default(), filters] {
            for width in [37, 25, 10] {
                for (start, end) in [(0., 1.), (0.2, 0.3), (0.77, 0.78), (0.99, 1.)] {
                    let render = |source: &Arc<dyn PageSource>| {
                        let mut decoder = DisplayDecoder::default();
                        let (plan, tiles) = decoder
                            .render_window_tiles(
                                source,
                                limits,
                                width,
                                start,
                                end,
                                1 << 22,
                                options,
                                |renderer| {
                                    // Rectangles of odd sizes straddle every seam.
                                    let plan = renderer.plan();
                                    let mut tiles = Vec::new();
                                    let mut top = plan.first_row;
                                    while top < plan.first_row + plan.rows {
                                        let height = (plan.first_row + plan.rows - top).min(97);
                                        let mut out =
                                            vec![0; plan.width as usize * height as usize * 4];
                                        renderer
                                            .render_rect(top, height, 0, plan.width, &mut out)?;
                                        tiles.extend(out);
                                        top += height;
                                    }
                                    Ok(tiles)
                                },
                            )
                            .unwrap();
                        let window = DisplayDecoder::default()
                            .decode_window_with_options(
                                source,
                                limits,
                                width,
                                start,
                                end,
                                1 << 22,
                                options,
                            )
                            .unwrap();
                        assert_eq!(window.plan, plan);
                        assert_eq!(window.image.as_raw(), &tiles, "tiles equal the window");
                        tiles
                    };
                    assert_eq!(
                        render(&banded),
                        render(&whole),
                        "options {options:?} width {width} rows {start}..{end}"
                    );
                }
            }
        }
        assert_eq!(strips.whole.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn banded_pages_needing_the_whole_page_fall_back_and_agree() {
        let page = textured(30, 400);
        let strips = Strips::new(&page, 64);
        let whole: Arc<dyn PageSource> = encoded(&page);
        let banded = dyn_source(&strips);
        let options = DisplayOptions {
            reverse_page: true,
            ..Default::default()
        };
        let limits = DecodeLimits::default();
        let a = DisplayDecoder::default()
            .decode_window_with_options(&banded, limits, 30, 0., 1., 1 << 22, options)
            .unwrap();
        let b = DisplayDecoder::default()
            .decode_window_with_options(&whole, limits, 30, 0., 1., 1 << 22, options)
            .unwrap();
        assert_eq!(a.image, b.image);
        assert_eq!(strips.whole.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn streamed_crop_scan_equals_whole_page_detection() {
        // Ink in a central block, white margins, plus low-alpha edge details.
        for (width, height, rows) in [(50u32, 300u32, 64u32), (50, 301, 100), (7, 1000, 16)] {
            let page = image::RgbaImage::from_fn(width, height, |x, y| {
                if (width / 10..width - width / 10).contains(&x)
                    && (height / 6..height - height / 5).contains(&y)
                {
                    image::Rgba([(x * 3) as u8, 20, (y % 200) as u8, 255])
                } else {
                    image::Rgba([255; 4])
                }
            });
            let expected = detect_crop(&page);
            let strips = Strips::new(&page, rows);
            let source = dyn_source(&strips);
            let options = DisplayOptions {
                crop_borders: true,
                ..Default::default()
            };
            let presentation = DisplayDecoder::default()
                .presentation(&source, DecodeLimits::default(), options)
                .unwrap();
            assert_eq!(
                (
                    presentation.crop_x,
                    presentation.crop_y,
                    presentation.cropped.width,
                    presentation.cropped.height
                ),
                (expected.x, expected.y, expected.width, expected.height)
            );
            assert_eq!(strips.whole.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
        // A cropped banded page renders the same bytes as the whole one.
        let page = image::RgbaImage::from_fn(60, 700, |x, y| {
            if (6..54).contains(&x) && (100..600).contains(&y) {
                image::Rgba([(x * 4) as u8, (y % 251) as u8, 9, 255])
            } else {
                image::Rgba([255; 4])
            }
        });
        let strips = Strips::new(&page, 128);
        let options = DisplayOptions {
            crop_borders: true,
            ..Default::default()
        };
        let whole: Arc<dyn PageSource> = encoded(&page);
        for width in [48, 20] {
            let a = DisplayDecoder::default()
                .decode_window_with_options(
                    &dyn_source(&strips),
                    DecodeLimits::default(),
                    width,
                    0.1,
                    0.9,
                    1 << 22,
                    options,
                )
                .unwrap();
            let b = DisplayDecoder::default()
                .decode_window_with_options(
                    &whole,
                    DecodeLimits::default(),
                    width,
                    0.1,
                    0.9,
                    1 << 22,
                    options,
                )
                .unwrap();
            assert_eq!(a.image, b.image, "cropped width {width}");
        }
    }

    /// Render every 512-row upload rect of the window that is not in `previous`
    /// and return the rows as the viewer would receive them.
    fn render_entering(
        decoder: &mut DisplayDecoder,
        source: &Arc<dyn PageSource>,
        limits: DecodeLimits,
        (start, end): (f64, f64),
        previous: Option<DisplayWindowPlan>,
    ) -> (DisplayWindowPlan, Vec<(u32, Vec<u8>)>) {
        decoder
            .render_window_tiles(
                source,
                limits,
                16,
                start,
                end,
                DISPLAY_WINDOW_BYTES,
                DisplayOptions::default(),
                |renderer| {
                    let plan = renderer.plan();
                    let mut out = Vec::new();
                    let mut top = plan.first_row;
                    while top < plan.first_row + plan.rows {
                        let bottom = ((top / DISPLAY_TILE_ROWS + 1) * DISPLAY_TILE_ROWS)
                            .min(plan.first_row + plan.rows);
                        if previous
                            .is_none_or(|p| top < p.first_row || bottom > p.first_row + p.rows)
                        {
                            let mut pixels = vec![0; 16 * (bottom - top) as usize * 4];
                            renderer.render_rect(top, bottom - top, 0, 16, &mut pixels)?;
                            out.push((top, pixels));
                        }
                        top = bottom;
                    }
                    Ok(out)
                },
            )
            .unwrap()
    }
    fn ahead(
        plan: DisplayWindowPlan,
        backwards: bool,
        depth_rows: u32,
        max_bytes: u64,
    ) -> ReadAhead {
        ReadAhead {
            width: 16,
            options: DisplayOptions::default(),
            window: plan,
            backwards,
            depth_rows,
            max_bytes,
        }
    }
    fn drain(
        decoder: &mut DisplayDecoder,
        source: &Arc<dyn PageSource>,
        request: ReadAhead,
    ) -> usize {
        let mut steps = 0;
        while decoder
            .read_ahead(source, DecodeLimits::default(), request)
            .unwrap()
            == ReadAheadStep::Decoded
        {
            steps += 1;
            assert!(steps < 100, "read-ahead must terminate");
        }
        steps
    }

    #[test]
    fn read_ahead_decodes_the_entering_bands_before_the_window_shift_needs_them() {
        // 16 bands of 512 rows; the windows are 2,048-row aligned.
        let page = textured(16, 12288);
        let strips = Strips::new(&page, 512);
        let source = dyn_source(&strips);
        let limits = DecodeLimits::default();
        let mut decoder = DisplayDecoder::default();
        let (plan, _) = render_entering(&mut decoder, &source, limits, (0., 0.05), None);
        assert_eq!((plan.first_row, plan.rows), (0, 6144));
        let waited = decoder.band_stats();
        assert_eq!(waited.ahead, 0);
        // Without read-ahead the next shift decodes four bands the viewer waits for.
        let mut cold = DisplayDecoder::default();
        let (cold_plan, _) = render_entering(&mut cold, &source, limits, (0., 0.05), None);
        let before = cold.band_stats().on_demand;
        let (_, entering) =
            render_entering(&mut cold, &source, limits, (0.4, 0.45), Some(cold_plan));
        assert_eq!(cold.band_stats().on_demand - before, 4, "stall on entry");
        assert_eq!(entering.len(), 1);
        // With read-ahead for the same rows, the shift waits for none.
        assert_eq!(
            drain(&mut decoder, &source, ahead(plan, false, 2048, 1 << 30)),
            4
        );
        assert_eq!(decoder.band_stats().ahead, 4);
        let on_demand = decoder.band_stats().on_demand;
        let (shifted, entering) =
            render_entering(&mut decoder, &source, limits, (0.4, 0.45), Some(plan));
        assert_eq!((shifted.first_row, shifted.rows), (2048, 6144));
        assert_eq!(
            decoder.band_stats().on_demand,
            on_demand,
            "no band decoded on entry"
        );
        // And the pixels are the page's own rows.
        let (top, pixels) = &entering[0];
        assert_eq!(*top, 6144);
        assert_eq!(pixels[..], page.as_raw()[6144 * 64..8192 * 64]);
    }

    #[test]
    fn read_ahead_follows_the_direction_and_evicts_the_farthest_band_first() {
        let page = textured(16, 12288);
        let strips = Strips::new(&page, 512);
        let source = dyn_source(&strips);
        let limits = DecodeLimits::default();
        let mut decoder = DisplayDecoder::default();
        // Window [4096, 8192) in the middle of the page.
        let (plan, _) = render_entering(&mut decoder, &source, limits, (0.45, 0.47), None);
        assert_eq!((plan.first_row, plan.rows), (2048, 6144));
        // Forward: rows 8192.. are bands 16.. (the page ends at band 24).
        assert_eq!(
            drain(&mut decoder, &source, ahead(plan, false, 1024, 1 << 30)),
            2
        );
        let ahead_bands = strips.decoded();
        assert!(
            ahead_bands.contains(&16) && ahead_bands.contains(&17),
            "{ahead_bands:?}"
        );
        // Backward: rows below 2048 are bands 3, 2 (nearest first).
        let before = strips.decoded.lock().unwrap().len();
        decoder
            .read_ahead(&source, limits, ahead(plan, true, 1024, 1 << 30))
            .unwrap();
        assert_eq!(
            *strips.decoded.lock().unwrap().last().unwrap(),
            3,
            "nearest first"
        );
        assert_eq!(strips.decoded.lock().unwrap().len(), before + 1);
        // A budget of four bands keeps those nearest the leading edge. The
        // first window left bands 0..=11 resident (the whole window rendered).
        let band = 16 * 512 * 4;
        let mut decoder = DisplayDecoder::default();
        let (plan, _) = render_entering(&mut decoder, &source, limits, (0.1, 0.12), None);
        assert_eq!((plan.first_row, plan.rows), (0, 6144));
        assert_eq!(decoder.resident_bands().0, 12);
        let max = 4 * band as u64;
        drain(&mut decoder, &source, ahead(plan, false, 1024, max));
        let (count, bytes) = decoder.resident_bands();
        assert!(bytes <= max && count <= 4, "{count} bands, {bytes} bytes");
        let mut resident = decoder
            .banded
            .as_ref()
            .unwrap()
            .bands
            .keys()
            .copied()
            .collect::<Vec<_>>();
        resident.sort();
        assert_eq!(
            resident,
            [10, 11, 12, 13],
            "bands farthest from the edge went first"
        );
    }

    #[test]
    fn read_ahead_declines_whole_pages_stale_windows_and_whole_page_recipes() {
        let page = textured(16, 12288);
        let strips = Strips::new(&page, 512);
        let source = dyn_source(&strips);
        let limits = DecodeLimits::default();
        let mut decoder = DisplayDecoder::default();
        // Nothing held yet.
        let probe = DisplayWindowPlan {
            width: 16,
            full_height: 12288,
            first_row: 0,
            rows: 2048,
        };
        assert_eq!(
            decoder
                .read_ahead(&source, limits, ahead(probe, false, 1024, 1 << 30))
                .unwrap(),
            ReadAheadStep::Unavailable
        );
        let (plan, _) = render_entering(&mut decoder, &source, limits, (0., 0.05), None);
        // A window planned for another height is stale.
        let stale = DisplayWindowPlan {
            full_height: 9000,
            ..plan
        };
        assert_eq!(
            decoder
                .read_ahead(&source, limits, ahead(stale, false, 1024, 1 << 30))
                .unwrap(),
            ReadAheadStep::Unavailable
        );
        // A recipe that needs the whole page is not sampled from bands.
        let mut reversed = ahead(plan, false, 1024, 1 << 30);
        reversed.options.reverse_page = true;
        assert_eq!(
            decoder.read_ahead(&source, limits, reversed).unwrap(),
            ReadAheadStep::Unavailable
        );
        // Nothing lies beyond a window that reaches the end of the page.
        let last = DisplayWindowPlan {
            first_row: 8192,
            rows: 4096,
            ..plan
        };
        assert_eq!(
            decoder
                .read_ahead(&source, limits, ahead(last, false, 1024, 1 << 30))
                .unwrap(),
            ReadAheadStep::Done
        );
        // A page decoded whole has no bands to read ahead.
        let png = {
            let mut bytes = std::io::Cursor::new(Vec::new());
            page.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            Arc::new(bytes.into_inner())
        };
        let mut whole = DisplayDecoder::default();
        whole
            .decode_window(&png, limits, 16, 0., 0.05, DISPLAY_WINDOW_BYTES)
            .unwrap();
        assert_eq!(
            whole
                .read_ahead(&png, limits, ahead(plan, false, 1024, 1 << 30))
                .unwrap(),
            ReadAheadStep::Unavailable
        );
        assert_eq!(whole.band_stats(), BandStats::default());
    }
    #[test]
    fn resident_bands_stay_within_the_retention_budget_farthest_evicted_first() {
        // Nine bands of 8 MiB: the render needs one or two at a time, and
        // only 32 MiB of what it has seen may stay.
        let page = image::RgbaImage::from_fn(4096, 4608, |x, y| {
            image::Rgba([x as u8, y as u8, (y / 512) as u8, 255])
        });
        let strips = Strips::new(&page, 512);
        let source = dyn_source(&strips);
        let limits = DecodeLimits::default();
        let mut decoder = DisplayDecoder::default();
        decoder
            .presentation(&source, limits, DisplayOptions::default())
            .unwrap();
        for band in 1..9 {
            let banded = decoder.banded.as_mut().unwrap();
            banded.load(band..band + 1, BAND_CACHE_BYTES).unwrap();
            assert!(banded.bytes <= BAND_RETAIN_BYTES, "after band {band}");
        }
        let mut resident = decoder
            .banded
            .as_ref()
            .unwrap()
            .bands
            .keys()
            .copied()
            .collect::<Vec<_>>();
        resident.sort();
        assert_eq!(
            resident,
            [5, 6, 7, 8],
            "the bands nearest the last render stay"
        );
        // A two-band seam rectangle exceeds nothing it needs, then trims.
        let banded = decoder.banded.as_mut().unwrap();
        banded.load(1..3, BAND_CACHE_BYTES).unwrap();
        assert!(banded.bytes <= BAND_RETAIN_BYTES);
        assert!(banded.bands.contains_key(&1) && banded.bands.contains_key(&2));
    }

    #[test]
    fn band_cache_is_bounded_and_replaced_with_its_source() {
        let page = textured(64, 1024);
        let strips = Strips::new(&page, 128);
        let source = dyn_source(&strips);
        // A 400-row window is 100 KiB; each band is 32 KiB. Leave room for two.
        let window = 64 * 400 * 4;
        let limits = DecodeLimits {
            max_decoded_bytes: window as u64 + 2 * 32 * 1024,
            ..DecodeLimits::default()
        };
        let mut decoder = DisplayDecoder::default();
        let options = DisplayOptions::default();
        let (plan, ()) = decoder
            .render_window_tiles(&source, limits, 64, 0.2, 0.5, window, options, |renderer| {
                // Walk the window in seam-straddling pieces; each needs <= 2 bands.
                let plan = renderer.plan();
                let mut top = plan.first_row;
                while top < plan.first_row + plan.rows {
                    let height = (plan.first_row + plan.rows - top).min(100);
                    let mut out = vec![0; 64 * height as usize * 4];
                    renderer.render_rect(top, height, 0, 64, &mut out)?;
                    assert_eq!(
                        out,
                        page.as_raw()[top as usize * 256..(top + height) as usize * 256]
                    );
                    top += height;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(plan.rows, 400);
        assert!(decoder.banded.as_ref().unwrap().bytes <= 2 * 32 * 1024);
        // The whole window needs more bands than the budget keeps resident.
        assert!(matches!(
            decoder.decode_window_with_options(&source, limits, 64, 0.2, 0.5, window, options),
            Err(Error::Limit)
        ));
        // A different source drops every band of the old one.
        let other = dyn_source(&Strips::new(&page, 128));
        decoder.presentation(&other, limits, options).unwrap();
        assert_eq!(decoder.banded.as_ref().unwrap().bands.len(), 1);
        decoder.clear();
        assert!(decoder.banded.is_none() && decoder.cached.is_none());
    }
}

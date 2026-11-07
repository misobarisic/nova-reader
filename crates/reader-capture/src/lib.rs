//! Compose the visible reader area from original chapter pixels, independently
//! of the window's device scale factor or retained display textures.
use std::{collections::BTreeSet, sync::Arc};

const PIXEL_LIMIT: u64 = 8_000_000;
const MEMORY_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Rect {
    pub fn valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.
            && self.height > 0.
            && self.right().is_finite()
            && self.bottom().is_finite()
            && self.right() > self.x
            && self.bottom() > self.y
    }
    pub fn right(self) -> f64 {
        self.x + self.width
    }
    pub fn bottom(self) -> f64 {
        self.y + self.height
    }
    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let width = self.right().min(other.right()) - x;
        let height = self.bottom().min(other.bottom()) - y;
        (width > 0. && height > 0.).then_some(Self {
            x,
            y,
            width,
            height,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageRect {
    pub index: usize,
    pub rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceStart {
    pub index: usize,
    pub y: u32,
}

pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub source_start: Option<SourceStart>,
}

struct Raster {
    width: u32,
    height: u32,
    viewport: Rect,
    scale: f64,
    reference_index: usize,
    reference_origin: (f64, f64),
}

fn geometry_error() -> String {
    "Chapter capture geometry is unavailable. Use Device resolution.".into()
}
fn budget_error() -> String {
    "Chapter capture exceeds supported image or memory limits. Use Device resolution.".into()
}

/// A decoded original, shareable with a caller-owned cache.
pub type Decoded = Arc<image::RgbaImage>;
type DecodeResult = Result<Decoded, reader_engine::Error>;
fn plain_decode<B: reader_engine::PageSource>(
    _: usize,
    page: &B,
    limits: reader_engine::DecodeLimits,
) -> DecodeResult {
    page.decode(limits).map(Arc::new)
}

/// Rectangles use one common logical coordinate space. Paint order follows
/// `pages`; each intersecting source is fetched once. No device bitmap is used.
pub fn capture(
    viewport: Rect,
    reference_index: usize,
    pages: &[PageRect],
    background: [u8; 4],
    fetch: impl FnMut(usize) -> Result<Vec<u8>, String>,
) -> Result<Capture, String> {
    capture_with_budget(
        viewport,
        reference_index,
        pages,
        background,
        reader_engine::DisplayOptions::default(),
        MEMORY_LIMIT,
        None,
        fetch,
    )
}

/// Render the visible virtual page geometry from one original at a time.
/// Options must be the immutable snapshot used to lay out `pages`.
pub fn capture_with_options(
    viewport: Rect,
    reference_index: usize,
    pages: &[PageRect],
    background: [u8; 4],
    options: reader_engine::DisplayOptions,
    fetch: impl FnMut(usize) -> Result<Vec<u8>, String>,
) -> Result<Capture, String> {
    if options == reader_engine::DisplayOptions::default() {
        return capture(viewport, reference_index, pages, background, fetch);
    }
    capture_with_budget(
        viewport,
        reference_index,
        pages,
        background,
        options,
        MEMORY_LIMIT,
        None,
        fetch,
    )
}

#[allow(clippy::too_many_arguments)]
fn capture_with_budget<B: reader_engine::PageSource>(
    viewport: Rect,
    reference_index: usize,
    pages: &[PageRect],
    background: [u8; 4],
    options: reader_engine::DisplayOptions,
    memory_limit: u64,
    target_scale: Option<f64>,
    fetch: impl FnMut(usize) -> Result<B, String>,
) -> Result<Capture, String> {
    capture_core(
        viewport,
        reference_index,
        pages,
        background,
        options,
        memory_limit,
        target_scale,
        fetch,
        plain_decode,
    )
}

#[allow(clippy::too_many_arguments)]
fn capture_core<B: reader_engine::PageSource>(
    viewport: Rect,
    reference_index: usize,
    pages: &[PageRect],
    background: [u8; 4],
    options: reader_engine::DisplayOptions,
    memory_limit: u64,
    target_scale: Option<f64>,
    mut fetch: impl FnMut(usize) -> Result<B, String>,
    mut decode: impl FnMut(usize, &B, reader_engine::DecodeLimits) -> DecodeResult,
) -> Result<Capture, String> {
    reader_engine::with_current_limits(|| {
        if !viewport.valid() || pages.len() > 10_000 {
            return Err(geometry_error());
        }
        let mut identities = BTreeSet::new();
        let mut visible = Vec::new();
        for page in pages {
            if !page.rect.valid()
                || !identities.insert(page.index)
                || !(viewport.x - page.rect.x).is_finite()
                || !(viewport.y - page.rect.y).is_finite()
            {
                return Err(geometry_error());
            }
            if page.rect.intersection(viewport).is_some() {
                visible.push(page);
            }
        }
        let reference = visible
            .iter()
            .find(|page| page.index == reference_index)
            .ok_or_else(geometry_error)?;
        let reference_bytes = fetch(reference_index)?;
        let reference_encoded = reference_bytes.encoded_len() as u64;
        let limits = reader_engine::DecodeLimits {
            max_decoded_bytes: memory_limit
                .checked_sub(reference_encoded)
                .ok_or_else(budget_error)?
                .min(reader_engine::current_limits().max_decoded_bytes),
            ..reader_engine::current_limits()
        };
        // Crop geometry depends on pixels. Admit this decode before output exists;
        // later, admit its retained original plus output as a separate phase.
        // Default keeps the legacy probe-first/reduced-decode admission unchanged.
        let early_reference = if options.crop_borders || options.split_wide || options.rotate_wide {
            let image = decode(reference_index, &reference_bytes, limits).map_err(|error| {
            format!(
                "Original chapter image exceeds capture admission: {error}. Use Device resolution."
            )
        })?;
            let presentation = reader_engine::page_presentation(&image, options)
                .map_err(|error| format!("Cannot present original chapter image: {error}"))?;
            Some((image, presentation))
        } else {
            None
        };
        let reference_presentation = early_reference
            .as_ref()
            .map(|(_, presentation)| *presentation);
        let dimensions = if let Some(presentation) = reference_presentation {
            presentation.effective
        } else {
            reference_bytes
                .dimensions(limits)
                .map_err(|error| format!("Cannot inspect original chapter image: {error}"))?
        };
        let scale = target_scale.unwrap_or(f64::from(dimensions.width) / reference.rect.width);
        let left = ((viewport.x - reference.rect.x) * scale).floor();
        let top = ((viewport.y - reference.rect.y) * scale).floor();
        let right = ((viewport.right() - reference.rect.x) * scale).ceil();
        let bottom = ((viewport.bottom() - reference.rect.y) * scale).ceil();
        let width = right - left;
        let height = bottom - top;
        if !scale.is_finite()
            || scale <= 0.
            || !width.is_finite()
            || !height.is_finite()
            || width < 1.
            || height < 1.
            || width > 16384.
            || height > 65536.
        {
            return Err(budget_error());
        }
        let (width, height) = (width as u32, height as u32);
        // Anchor pixel centers to the reference source, rather than a fractional
        // gutter or scroll offset. The framing expands by less than one native
        // pixel at each edge, preserving exact reference strokes at native scale.
        let raster = Raster {
            width,
            height,
            scale,
            reference_index,
            reference_origin: (left, top),
            viewport: Rect {
                x: reference.rect.x + left / scale,
                y: reference.rect.y + top / scale,
                width: f64::from(width) / scale,
                height: f64::from(height) / scale,
            },
        };
        if !raster.viewport.valid() {
            return Err(geometry_error());
        }
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .filter(|pixels| *pixels <= PIXEL_LIMIT)
            .ok_or_else(budget_error)?;
        let output_bytes = pixels.checked_mul(4).ok_or_else(budget_error)?;
        let remaining = memory_limit
            .checked_sub(output_bytes)
            .and_then(|remaining| remaining.checked_sub(reference_encoded))
            .ok_or_else(budget_error)?;
        let limits = reader_engine::DecodeLimits {
            max_decoded_bytes: remaining.min(limits.max_decoded_bytes),
            ..limits
        };
        // Admit the reference before allocating output. Decode now only when it
        // paints first; otherwise defer decompression until its turn. Pixel-based
        // presentation geometry may already have required an early decode.
        let reference_first = visible
            .first()
            .is_some_and(|page| page.index == reference_index);
        let reference_image = if let Some((image, _)) = early_reference {
            if image.as_raw().len() as u64 > remaining {
                return Err(budget_error());
            }
            reference_first.then_some(image)
        } else if reference_first {
            Some(decode(reference_index, &reference_bytes, limits).map_err(|error| {
            format!(
                "Original chapter image exceeds capture admission: {error}. Use Device resolution."
            )
        })?)
        } else {
            // Preserve decode admission without decompressing this source twice.
            reference_bytes.dimensions(limits).map_err(|error| {
                format!("Original chapter image exceeds capture admission: {error}")
            })?;
            None
        };
        let mut reference_image = reference_image;
        let mut reference_bytes = Some(reference_bytes);
        let mut output = Vec::new();
        output
            .try_reserve_exact(output_bytes as usize)
            .map_err(|_| budget_error())?;
        output.resize(output_bytes as usize, 0);
        for pixel in output.as_chunks_mut::<4>().0 {
            *pixel = background;
        }
        let mut source_start = None;
        for page in visible {
            let image = if page.index == reference_index {
                let bytes = reference_bytes.take().ok_or_else(geometry_error)?;
                if let Some(image) = reference_image.take() {
                    image
                } else {
                    decode(page.index, &bytes, limits).map_err(|error| {
                        format!(
                            "Cannot decode original chapter image: {error}. Use Device resolution."
                        )
                    })?
                }
            } else {
                let bytes = fetch(page.index)?;
                let live_reference = reference_bytes
                    .as_ref()
                    .map_or(0, |bytes| bytes.encoded_len() as u64);
                let limits = reader_engine::DecodeLimits {
                    max_decoded_bytes: memory_limit
                        .checked_sub(output_bytes)
                        .and_then(|remaining| remaining.checked_sub(live_reference))
                        .and_then(|remaining| remaining.checked_sub(bytes.encoded_len() as u64))
                        .ok_or_else(budget_error)?
                        .min(reader_engine::current_limits().max_decoded_bytes),
                    ..limits
                };
                decode(page.index, &bytes, limits).map_err(|error| {
                    format!("Cannot decode original chapter image: {error}. Use Device resolution.")
                })?
            };
            let presentation = if page.index == reference_index {
                reference_presentation
                    .map(Ok)
                    .unwrap_or_else(|| reader_engine::page_presentation(&image, options))
            } else {
                reader_engine::page_presentation(&image, options)
            }
            .map_err(|error| format!("Cannot present original chapter image: {error}"))?;
            let geometry = paint_geometry(&raster, page, presentation)?;
            if source_start.is_none()
                && geometry.start_x < geometry.end_x
                && geometry.start_y < geometry.end_y
            {
                // Name the actual first admitted output sample, including native
                // floor/ceil framing. Resampled mixtures use the nearest original
                // coordinate of that sample after presentation transforms.
                let x = geometry.source_x(geometry.start_x, presentation.effective.width);
                let y = geometry.source_y(geometry.start_y, presentation.effective.height);
                let (_, source_y) = presentation.source_coordinate(x, y);
                source_start = Some(SourceStart {
                    index: page.index,
                    y: source_y
                        .round()
                        .clamp(0., f64::from(presentation.source.height - 1))
                        as u32,
                });
            }
            paint(
                &mut output,
                &raster,
                &geometry,
                &image,
                presentation,
                options,
            );
            // No decoded page survives into the next iteration.
        }
        Ok(Capture {
            width,
            height,
            pixels: output,
            source_start,
        })
    })
}

struct PaintGeometry {
    start_x: u32,
    end_x: u32,
    start_y: u32,
    end_y: u32,
    step_x: f64,
    step_y: f64,
    offset_x: f64,
    offset_y: f64,
}
impl PaintGeometry {
    fn source_x(&self, x: u32, width: u32) -> f64 {
        ((f64::from(x) + 0.5) * self.step_x + self.offset_x).clamp(0., f64::from(width - 1))
    }
    fn source_y(&self, y: u32, height: u32) -> f64 {
        ((f64::from(y) + 0.5) * self.step_y + self.offset_y).clamp(0., f64::from(height - 1))
    }
}
fn paint_geometry(
    raster: &Raster,
    page: &PageRect,
    presentation: reader_engine::PagePresentation,
) -> Result<PaintGeometry, String> {
    let Raster {
        width,
        height,
        viewport,
        scale,
        ..
    } = *raster;
    let rect = page.rect;
    let clip = rect.intersection(viewport).ok_or_else(geometry_error)?;
    let start_x = ((clip.x - viewport.x) * scale - 0.5)
        .ceil()
        .clamp(0., f64::from(width)) as u32;
    let end_x = ((clip.right() - viewport.x) * scale - 0.5)
        .ceil()
        .clamp(0., f64::from(width)) as u32;
    let start_y = ((clip.y - viewport.y) * scale - 0.5)
        .ceil()
        .clamp(0., f64::from(height)) as u32;
    let end_y = ((clip.bottom() - viewport.y) * scale - 0.5)
        .ceil()
        .clamp(0., f64::from(height)) as u32;
    let mut step_x = f64::from(presentation.effective.width) / (rect.width * scale);
    let mut step_y = f64::from(presentation.effective.height) / (rect.height * scale);
    let (offset_x, offset_y) = if page.index == raster.reference_index {
        // Ignore only layout float rounding at a true native 1:1 ratio.
        if (step_x - 1.).abs() < 1e-6 {
            step_x = 1.;
        }
        if (step_y - 1.).abs() < 1e-6 {
            step_y = 1.;
        }
        (
            raster.reference_origin.0 * step_x - 0.5,
            raster.reference_origin.1 * step_y - 0.5,
        )
    } else {
        (
            (viewport.x - rect.x) / rect.width * f64::from(presentation.effective.width) - 0.5,
            (viewport.y - rect.y) / rect.height * f64::from(presentation.effective.height) - 0.5,
        )
    };
    if [step_x, step_y, offset_x, offset_y]
        .iter()
        .any(|value| !value.is_finite())
    {
        return Err(geometry_error());
    }
    Ok(PaintGeometry {
        start_x,
        end_x,
        start_y,
        end_y,
        step_x,
        step_y,
        offset_x,
        offset_y,
    })
}
/// Horizontal sample position of one output column, shared by every row.
struct XTap {
    x0: u32,
    x1: u32,
    fx: f64,
}
fn paint(
    output: &mut [u8],
    raster: &Raster,
    geometry: &PaintGeometry,
    image: &image::RgbaImage,
    presentation: reader_engine::PagePresentation,
    options: reader_engine::DisplayOptions,
) {
    let width = raster.width;
    let identity = presentation.crop_x == 0
        && presentation.crop_y == 0
        && presentation.source == presentation.effective
        && !presentation.split
        && presentation.rotation == reader_engine::PageRotation::None
        && !presentation.reverse_page;
    // Row walk: columns depend only on x, so floor/fraction/neighbour are
    // computed once per column instead of once per pixel.
    let last_x = presentation.effective.width - 1;
    let taps = (geometry.start_x..geometry.end_x)
        .map(|x| {
            let source_x = geometry.source_x(x, presentation.effective.width);
            let x0 = source_x.floor() as u32;
            XTap {
                x0,
                x1: (x0 + 1).min(last_x),
                fx: source_x - f64::from(x0),
            }
        })
        .collect::<Vec<_>>();
    for y in geometry.start_y..geometry.end_y {
        let source_y = geometry.source_y(y, presentation.effective.height);
        let y0 = source_y.floor() as u32;
        let fy = source_y - f64::from(y0);
        // Interpolation must stay on one paper half, including when reversed.
        let section_end = if presentation.split && y0 < presentation.half_height {
            presentation.half_height - 1
        } else {
            presentation.effective.height - 1
        };
        let y1 = (y0 + 1).min(section_end);
        let row = ((u64::from(y) * u64::from(width) + u64::from(geometry.start_x)) * 4) as usize;
        for (tap, target) in taps.iter().zip(
            output[row..row + taps.len() * 4]
                .as_chunks_mut::<4>()
                .0
                .iter_mut(),
        ) {
            blend(
                target,
                image,
                presentation,
                tap,
                (y0, y1, fy),
                options,
                identity,
            );
        }
    }
}

/// Bilinear filtering uses premultiplied colors to avoid dark fringes around
/// transparent source pixels. Integer source coordinates remain exact.
fn blend(
    target: &mut [u8],
    image: &image::RgbaImage,
    presentation: reader_engine::PagePresentation,
    tap: &XTap,
    (y0, y1, fy): (u32, u32, f64),
    options: reader_engine::DisplayOptions,
    identity: bool,
) {
    let (x0, x1, fx) = (tap.x0, tap.x1, tap.fx);
    let sample = |x, y| {
        if identity {
            return image.get_pixel(x, y).0;
        }
        let (x, y) = presentation.source_coordinate(f64::from(x), f64::from(y));
        image.get_pixel(x as u32, y as u32).0
    };
    let mut first = sample(x0, y0);
    if fx == 0. && fy == 0. && first[3] == 255 {
        if options.color_filter || options.grayscale || options.invert_colors {
            reader_engine::filter_pixel(&mut first, options);
        }
        target.copy_from_slice(&first);
        return;
    }
    let mut color = [0.; 3];
    let mut alpha = 0.;
    for (pixel, weight) in [
        (first, (1. - fx) * (1. - fy)),
        (sample(x1, y0), fx * (1. - fy)),
        (sample(x0, y1), (1. - fx) * fy),
        (sample(x1, y1), fx * fy),
    ] {
        let coverage = f64::from(pixel[3]) / 255. * weight;
        alpha += coverage;
        for channel in 0..3 {
            color[channel] += f64::from(pixel[channel]) * coverage;
        }
    }
    if alpha > 0.
        && ((options.color_filter && options.filter_intensity > 0)
            || options.grayscale
            || options.invert_colors)
    {
        let mut foreground = [0; 4];
        for channel in 0..3 {
            foreground[channel] = (color[channel] / alpha).round().clamp(0., 255.) as u8;
        }
        foreground[3] = (alpha * 255.).round().clamp(0., 255.) as u8;
        reader_engine::filter_pixel(&mut foreground, options);
        for channel in 0..3 {
            color[channel] = f64::from(foreground[channel]) * alpha;
        }
    }
    let background_alpha = f64::from(target[3]) / 255.;
    let result_alpha = alpha + background_alpha * (1. - alpha);
    for channel in 0..3 {
        target[channel] = if result_alpha == 0. {
            0
        } else {
            ((color[channel] + f64::from(target[channel]) * background_alpha * (1. - alpha))
                / result_alpha)
                .round()
                .clamp(0., 255.) as u8
        };
    }
    target[3] = (result_alpha * 255.).round().clamp(0., 255.) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }
    fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(
                image.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        encoded
    }
    fn at(capture: &Capture, x: u32, y: u32) -> [u8; 4] {
        let start = ((y * capture.width + x) * 4) as usize;
        capture.pixels[start..start + 4].try_into().unwrap()
    }
    fn pattern(x: u32, y: u32) -> [u8; 4] {
        [(x % 256) as u8, (y / 256) as u8, (y % 256) as u8, 255]
    }

    #[test]
    fn tall_original_keeps_native_strokes_and_crops_only_visible_rows() {
        let original = png(720, 20_000, pattern);
        let mut fetched = Vec::new();
        let image = capture(
            rect(0., 0., 360., 640.),
            3,
            &[
                PageRect {
                    index: 3,
                    rect: rect(0., -1000., 360., 10_000.),
                },
                PageRect {
                    index: 99,
                    rect: rect(0., 30_000., 360., 100.),
                },
            ],
            [0, 0, 0, 255],
            |index| {
                fetched.push(index);
                assert_eq!(index, 3);
                Ok(original.clone())
            },
        )
        .unwrap();
        assert_eq!((image.width, image.height), (720, 1280));
        assert_ne!((image.width, image.height), (1080, 1920)); // device DPR 3
        assert_eq!(fetched, [3]);
        for (x, y) in [(0, 0), (1, 1), (359, 777), (719, 1279)] {
            assert_eq!(at(&image, x, y), pattern(x, y + 2000));
        }
    }

    #[test]
    fn fractional_native_crop_names_the_first_exported_source_row() {
        let original = png(10, 20, |x, y| [x as u8, y as u8, 0, 255]);
        let pages = [PageRect {
            index: 7,
            rect: rect(0., 0., 10., 20.),
        }];
        let image = capture(rect(1.6, 5.6, 4., 3.), 7, &pages, [0; 4], |_| {
            Ok(original.clone())
        })
        .unwrap();
        assert_eq!((image.width, image.height), (5, 4));
        assert_eq!(at(&image, 0, 0), [1, 5, 0, 255]);
        assert_eq!(image.source_start, Some(SourceStart { index: 7, y: 5 }));
        // Reversal changes original coordinates after the exact same admitted
        // native sample has been chosen for painting and naming.
        let reversed = capture_with_options(
            rect(1.6, 5.6, 4., 3.),
            7,
            &pages,
            [0; 4],
            reader_engine::DisplayOptions {
                reverse_page: true,
                ..Default::default()
            },
            |_| Ok(original.clone()),
        )
        .unwrap();
        assert_eq!(at(&reversed, 0, 0), [8, 14, 0, 255]);
        assert_eq!(reversed.source_start, Some(SourceStart { index: 7, y: 14 }));
    }

    #[test]
    fn selected_device_scale_renders_original_rows_across_offscreen_page_boundary() {
        let originals = [
            png(20, 100, |x, y| [x as u8, y as u8, 0, 255]),
            png(20, 100, |x, y| [x as u8, y as u8, 100, 255]),
        ];
        let pages = [
            PageRect {
                index: 0,
                rect: rect(0., -100., 40., 200.),
            },
            PageRect {
                index: 1,
                rect: rect(0., 100., 40., 200.),
            },
        ];
        let mut fetched = Vec::new();
        let image = capture_at_scale(
            rect(4., 80., 24., 80.),
            0,
            &pages,
            [0; 4],
            Default::default(),
            Some(1.),
            |index| {
                fetched.push(index);
                Ok(originals[index].clone())
            },
        )
        .unwrap();
        assert_eq!((image.width, image.height), (24, 80));
        assert_eq!(fetched, [0, 1]);
        assert_eq!(at(&image, 0, 0), [2, 90, 0, 255]);
        assert_eq!(at(&image, 0, 20), [2, 0, 100, 255]);
        assert_eq!(at(&image, 23, 79), [13, 29, 100, 255]);
        // A downsampled preview can cover a tall selected range while output
        // export still rejects the same range at an excessive device scale.
        let preview = capture_at_scale(
            rect(0., -100., 40., 400.),
            0,
            &pages,
            [0; 4],
            Default::default(),
            Some(0.1),
            |index| Ok(originals[index].clone()),
        )
        .unwrap();
        assert_eq!((preview.width, preview.height), (4, 40));
        assert!(
            capture_at_scale(
                rect(0., -100., 40., 400.),
                0,
                &pages,
                [0; 4],
                Default::default(),
                Some(30.),
                |index| Ok(originals[index].clone())
            )
            .is_err()
        );
    }

    #[test]
    fn fractional_gutter_and_scroll_still_preserve_exact_reference_pixels() {
        let original = png(720, 80, |x, y| {
            [if x.is_multiple_of(2) { 0 } else { 255 }, y as u8, 0, 255]
        });
        let scale = 720. / 374.;
        let top = (3.25f64 * scale).floor();
        let image = capture(
            rect(0., 0., 390., 20.),
            0,
            &[PageRect {
                index: 0,
                rect: rect(8., -3.25, 374., 80. / scale),
            }],
            [17, 19, 23, 255],
            |_| Ok(original.clone()),
        )
        .unwrap();
        let left = (-8f64 * scale).floor();
        assert_eq!(image.width, (((390. - 8.) * scale).ceil() - left) as u32);
        let first_column = -left as u32;
        for x in 0..720 {
            assert_eq!(
                at(&image, first_column + x, 0),
                [if x.is_multiple_of(2) { 0 } else { 255 }, top as u8, 0, 255]
            );
        }
    }

    #[test]
    fn mixed_native_widths_use_bilinear_sampling_and_fetch_reference_once() {
        for (reference_width, other_width, expected) in [
            (8, 4, vec![0, 5, 15, 25, 35, 45, 55, 60]),
            (4, 8, vec![10, 50, 90, 130]),
        ] {
            let originals = [
                png(other_width, other_width / 2, |x, _| {
                    [(x * 20) as u8, 0, 0, 255]
                }),
                png(reference_width, reference_width / 2, |x, _| {
                    [0, 0, (x * 30) as u8, 255]
                }),
            ];
            let mut fetched = Vec::new();
            let image = capture(
                rect(0., 0., 4., 4.),
                1,
                &[
                    PageRect {
                        index: 0,
                        rect: rect(0., 0., 4., 2.),
                    },
                    PageRect {
                        index: 1,
                        rect: rect(0., 2., 4., 2.),
                    },
                ],
                [0, 0, 0, 255],
                |index| {
                    fetched.push(index);
                    Ok(originals[index].clone())
                },
            )
            .unwrap();
            assert_eq!(fetched, [1, 0]);
            assert_eq!(
                (image.width, image.height),
                (reference_width, reference_width)
            );
            for (x, red) in expected.into_iter().enumerate() {
                assert_eq!(at(&image, x as u32, 0)[0], red);
            }
            assert_eq!(
                at(&image, reference_width - 1, reference_width - 1),
                [0, 0, ((reference_width - 1) * 30) as u8, 255]
            );
        }
    }

    #[test]
    fn window_origin_pan_and_book_letterboxes_keep_theme_background() {
        let background = [11, 17, 23, 255];
        let original = png(8, 4, pattern);
        let image = capture(
            rect(100., 200., 4., 4.),
            3,
            &[PageRect {
                index: 3,
                rect: rect(99., 201., 4., 2.),
            }],
            background,
            |_| Ok(original.clone()),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (8, 8));
        assert_eq!(at(&image, 0, 2), pattern(2, 0));
        assert_eq!(at(&image, 5, 5), pattern(7, 3));
        for (x, y) in [(0, 0), (4, 1), (6, 3), (7, 7), (4, 6)] {
            assert_eq!(at(&image, x, y), background);
        }
        let portrait = png(4, 8, pattern);
        let image = capture(
            rect(100., 200., 4., 4.),
            0,
            &[PageRect {
                index: 0,
                rect: rect(101., 200., 2., 4.),
            }],
            background,
            |_| Ok(portrait.clone()),
        )
        .unwrap();
        assert_eq!(at(&image, 2, 0), pattern(0, 0));
        assert_eq!(at(&image, 5, 7), pattern(3, 7));
        for x in [0, 1, 6, 7] {
            assert_eq!(at(&image, x, 4), background);
        }
    }

    #[test]
    fn alpha_filtering_avoids_dark_fringes_and_preserves_paint_order() {
        let reference = png(4, 1, |_, _| [0, 0, 0, 0]);
        let foreground = png(2, 1, |x, _| {
            if x == 0 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        for background in [[0, 0, 0, 255], [0, 0, 0, 0]] {
            let image = capture(
                rect(0., 0., 4., 1.),
                0,
                &[
                    PageRect {
                        index: 0,
                        rect: rect(0., 0., 4., 1.),
                    },
                    PageRect {
                        index: 1,
                        rect: rect(0., 0., 4., 1.),
                    },
                ],
                background,
                |index| {
                    Ok(if index == 0 {
                        reference.clone()
                    } else {
                        foreground.clone()
                    })
                },
            )
            .unwrap();
            if background[3] == 255 {
                assert_eq!(
                    (0..4).map(|x| at(&image, x, 0)[0]).collect::<Vec<_>>(),
                    [255, 191, 64, 0]
                );
            } else {
                assert_eq!(at(&image, 1, 0), [255, 255, 255, 191]);
                assert_eq!(at(&image, 2, 0), [255, 255, 255, 64]);
            }
        }
    }

    #[test]
    fn oversized_outputs_and_invalid_geometry_fail_before_other_fetches() {
        let original = png(4, 4, |_, _| [255; 4]);
        for viewport in [
            rect(0., 0., 2000., 2000.),
            rect(0., 0., 5000., 1.),
            rect(0., 0., 1., 20_000.),
        ] {
            let mut calls = 0;
            let error = capture(
                viewport,
                0,
                &[PageRect {
                    index: 0,
                    rect: rect(0., 0., 1., 1.),
                }],
                [0; 4],
                |_| {
                    calls += 1;
                    Ok(original.clone())
                },
            )
            .err()
            .unwrap();
            assert!(error.contains("Use Device"));
            assert_eq!(calls, 1);
        }
        for pages in [
            vec![PageRect {
                index: 0,
                rect: rect(0., 0., f64::NAN, 1.),
            }],
            vec![
                PageRect {
                    index: 0,
                    rect: rect(0., 0., 1., 1.)
                };
                2
            ],
            vec![PageRect {
                index: 0,
                rect: rect(0., 10., 1., 1.),
            }],
        ] {
            let mut calls = 0;
            assert!(
                capture(rect(0., 0., 1., 1.), 0, &pages, [0; 4], |_| {
                    calls += 1;
                    Ok(original.clone())
                })
                .is_err()
            );
            assert_eq!(calls, 0);
        }
    }
    fn bordered_pixel(x: u32, y: u32) -> [u8; 4] {
        if (2..10).contains(&x) && (2..6).contains(&y) {
            [(x * 20) as u8, (y * 20) as u8, 40, 255]
        } else {
            [255; 4]
        }
    }

    #[test]
    fn chapter_options_crop_and_rtl_split_use_effective_native_width_and_filtered_pixels() {
        let original = png(12, 8, bordered_pixel);
        let cropped = capture_with_options(
            rect(0., 0., 4., 2.),
            0,
            &[PageRect {
                index: 0,
                rect: rect(0., 0., 4., 2.),
            }],
            [0, 0, 0, 255],
            reader_engine::DisplayOptions {
                crop_borders: true,
                ..Default::default()
            },
            |_| Ok(original.clone()),
        )
        .unwrap();
        assert_eq!((cropped.width, cropped.height), (8, 4));
        assert_eq!(cropped.source_start, Some(SourceStart { index: 0, y: 2 }));
        assert_eq!(at(&cropped, 0, 0), bordered_pixel(2, 2));
        assert_eq!(at(&cropped, 7, 3), bordered_pixel(9, 5));

        let split = capture_with_options(
            rect(0., 0., 4., 8.),
            0,
            &[PageRect {
                index: 0,
                rect: rect(0., 0., 4., 8.),
            }],
            [0, 0, 0, 255],
            reader_engine::DisplayOptions {
                crop_borders: true,
                split_wide: true,
                rtl: true,
                grayscale: true,
                invert_colors: true,
                ..Default::default()
            },
            |_| Ok(original.clone()),
        )
        .unwrap();
        assert_eq!((split.width, split.height), (4, 8));
        // Right paper half first, then left; grayscale precedes inversion.
        assert_eq!(at(&split, 0, 0), [191, 191, 191, 255]);
        assert_eq!(at(&split, 3, 3), [138, 138, 138, 255]);
        assert_eq!(at(&split, 0, 4), [215, 215, 215, 255]);
        assert_eq!(at(&split, 3, 7), [162, 162, 162, 255]);
    }

    #[test]
    fn chapter_options_rotate_in_both_directions_and_reverse_without_intermediate_images() {
        let original = png(12, 8, bordered_pixel);
        for (flip, reverse, top_left, bottom_right) in [
            (false, false, (2, 5), (9, 2)),
            (true, false, (9, 2), (2, 5)),
            (false, true, (9, 2), (2, 5)),
        ] {
            let image = capture_with_options(
                rect(0., 0., 4., 8.),
                0,
                &[PageRect {
                    index: 0,
                    rect: rect(0., 0., 4., 8.),
                }],
                [0, 0, 0, 255],
                reader_engine::DisplayOptions {
                    crop_borders: true,
                    rotate_wide: true,
                    flip_wide: flip,
                    reverse_page: reverse,
                    ..Default::default()
                },
                |_| Ok(original.clone()),
            )
            .unwrap();
            assert_eq!((image.width, image.height), (4, 8));
            assert_eq!(
                image.source_start,
                Some(SourceStart {
                    index: 0,
                    y: top_left.1
                })
            );
            assert_eq!(at(&image, 0, 0), bordered_pixel(top_left.0, top_left.1));
            assert_eq!(
                at(&image, 3, 7),
                bordered_pixel(bottom_right.0, bottom_right.1)
            );
        }
    }

    #[test]
    fn chapter_options_filter_the_foreground_before_alpha_compositing() {
        let original = png(2, 2, |_, _| [100, 50, 0, 128]);
        let image = capture_with_options(
            rect(0., 0., 2., 2.),
            0,
            &[PageRect {
                index: 0,
                rect: rect(0., 0., 2., 2.),
            }],
            [0; 4],
            reader_engine::DisplayOptions {
                color_filter: true,
                filter_color: 0xc86464,
                filter_intensity: 50,
                ..Default::default()
            },
            |_| Ok(original.clone()),
        )
        .unwrap();
        assert_eq!(at(&image, 0, 0), [150, 75, 50, 128]);
    }

    #[test]
    fn cropped_multi_page_capture_clips_and_preserves_paint_order_fetching_reference_once() {
        let originals = [0u8, 100].map(|red| {
            png(12, 8, |x, y| {
                if (2..10).contains(&x) && (2..6).contains(&y) {
                    [red + x as u8, y as u8, 0, 255]
                } else {
                    [255; 4]
                }
            })
        });
        let mut fetched = Vec::new();
        let image = capture_with_options(
            rect(2., 0., 4., 4.),
            1,
            &[
                PageRect {
                    index: 0,
                    rect: rect(0., -2., 8., 4.),
                },
                PageRect {
                    index: 1,
                    rect: rect(0., 2., 8., 4.),
                },
            ],
            [0, 0, 0, 255],
            reader_engine::DisplayOptions {
                crop_borders: true,
                ..Default::default()
            },
            |index| {
                fetched.push(index);
                Ok(originals[index].clone())
            },
        )
        .unwrap();
        assert_eq!(fetched, [1, 0]);
        assert_eq!((image.width, image.height), (4, 4));
        assert_eq!(at(&image, 0, 0), [4, 4, 0, 255]);
        assert_eq!(at(&image, 3, 1), [7, 5, 0, 255]);
        assert_eq!(at(&image, 0, 2), [104, 2, 0, 255]);
        assert_eq!(at(&image, 3, 3), [107, 3, 0, 255]);
    }

    #[test]
    fn split_sampling_clamps_at_paper_seams_while_resampling_other_pages() {
        let originals = [
            png(2, 8, |_, _| [0; 4]),
            png(4, 2, |x, y| {
                [
                    if x < 2 {
                        10 + 10 * y as u8
                    } else {
                        200 + 10 * y as u8
                    },
                    0,
                    0,
                    255,
                ]
            }),
        ];
        let image = capture_with_options(
            rect(0., 0., 2., 8.),
            0,
            &[
                PageRect {
                    index: 0,
                    rect: rect(0., 0., 2., 8.),
                },
                PageRect {
                    index: 1,
                    rect: rect(0., 0., 2., 8.),
                },
            ],
            [0, 0, 0, 255],
            reader_engine::DisplayOptions {
                split_wide: true,
                ..Default::default()
            },
            |index| Ok(originals[index].clone()),
        )
        .unwrap();
        assert_eq!(
            (0..8).map(|y| at(&image, 0, y)[0]).collect::<Vec<_>>(),
            [10, 13, 18, 20, 20, 203, 208, 210]
        );
    }

    #[test]
    fn options_preserve_separate_decode_and_retained_output_memory_admission() {
        let options = reader_engine::DisplayOptions {
            crop_borders: true,
            ..Default::default()
        };
        let page = PageRect {
            index: 0,
            rect: rect(0., 0., 4., 4.),
        };
        // Native decoder output and RGBA conversion must both fit before output.
        let original = png(4, 3, |_, _| [10, 20, 30, 255]);
        assert!(
            capture_with_budget(
                rect(0., 0., 4., 4.),
                0,
                &[page],
                [0; 4],
                options,
                64,
                None,
                |_| Ok(original.clone())
            )
            .is_err()
        );
        // Initial decode fits 128 bytes exactly; retained original + output do not.
        let original = png(4, 4, |_, _| [10, 20, 30, 255]);
        assert!(
            capture_with_budget(
                rect(0., 0., 4., 5.),
                0,
                &[page],
                [0; 4],
                options,
                128,
                None,
                |_| Ok(original.clone())
            )
            .is_err()
        );
        // A later original fits the full budget, but its decode must leave room
        // for the already allocated output. No reference decode overlaps it.
        let originals = [original, png(7, 4, |_, _| [30, 40, 50, 255])];
        let mut fetched = Vec::new();
        let error = capture_with_budget(
            rect(0., 0., 4., 4.),
            0,
            &[
                page,
                PageRect {
                    index: 1,
                    rect: page.rect,
                },
            ],
            [0; 4],
            options,
            160 + originals
                .iter()
                .map(|bytes| bytes.len() as u64)
                .max()
                .unwrap(),
            None,
            |index| {
                fetched.push(index);
                Ok(originals[index].clone())
            },
        )
        .err()
        .unwrap();
        assert_eq!(fetched, [0, 1]);
        assert!(error.contains("limits"));
        // The unchanged Default path retains its previous reduced decode budget.
        assert!(
            capture_with_budget(
                rect(0., 0., 4., 4.),
                0,
                &[page],
                [0; 4],
                reader_engine::DisplayOptions::default(),
                128,
                None,
                |_| Ok(originals[0].clone())
            )
            .is_err()
        );
    }
}

/// Render artwork at a frozen logical viewport scale, or at native chapter
/// scale when `scale` is None. Preview scale is independent of export scale.
pub fn capture_at_scale(
    viewport: Rect,
    reference: usize,
    pages: &[PageRect],
    background: [u8; 4],
    options: reader_engine::DisplayOptions,
    scale: Option<f64>,
    fetch: impl FnMut(usize) -> Result<Vec<u8>, String>,
) -> Result<Capture, String> {
    capture_with_budget(
        viewport,
        reference,
        pages,
        background,
        options,
        MEMORY_LIMIT,
        scale,
        fetch,
    )
}

pub mod selection;

/// Same compositor with an explicit resident-memory reservation for originals
/// prefetched while resolving incoming page geometry.
#[allow(clippy::too_many_arguments)]
pub fn capture_at_scale_with_memory<B: reader_engine::PageSource>(
    viewport: Rect,
    reference: usize,
    pages: &[PageRect],
    background: [u8; 4],
    options: reader_engine::DisplayOptions,
    scale: Option<f64>,
    memory_limit: u64,
    fetch: impl FnMut(usize) -> Result<B, String>,
) -> Result<Capture, String> {
    capture_with_budget(
        viewport,
        reference,
        pages,
        background,
        options,
        memory_limit.min(MEMORY_LIMIT),
        scale,
        fetch,
    )
}

/// Same compositor, decoding originals through `decode` so a caller can serve
/// repeated renders from a decoded-original cache. The decoder receives the
/// page index, the fetched bytes and the admission limits for that decode, and
/// must enforce the limits for anything it returns. Pages the caller retains
/// must be covered by `memory_limit`.
#[allow(clippy::too_many_arguments)]
pub fn capture_at_scale_with_decoder<B: reader_engine::PageSource>(
    viewport: Rect,
    reference: usize,
    pages: &[PageRect],
    background: [u8; 4],
    options: reader_engine::DisplayOptions,
    scale: Option<f64>,
    memory_limit: u64,
    fetch: impl FnMut(usize) -> Result<B, String>,
    decode: impl FnMut(usize, &B, reader_engine::DecodeLimits) -> Result<Decoded, reader_engine::Error>,
) -> Result<Capture, String> {
    capture_core(
        viewport,
        reference,
        pages,
        background,
        options,
        memory_limit.min(MEMORY_LIMIT),
        scale,
        fetch,
        decode,
    )
}

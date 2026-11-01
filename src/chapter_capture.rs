//! Compose the visible reader area from original chapter pixels, independently
//! of the window's device scale factor or retained display textures.
use std::collections::BTreeSet;

const PIXEL_LIMIT: u64 = 8_000_000;
const MEMORY_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Rect {
    fn valid(self) -> bool {
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
    fn right(self) -> f64 {
        self.x + self.width
    }
    fn bottom(self) -> f64 {
        self.y + self.height
    }
    fn intersection(self, other: Self) -> Option<Self> {
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

#[derive(Clone, Copy, Debug)]
pub(crate) struct PageRect {
    pub index: usize,
    pub rect: Rect,
}

pub(crate) struct Capture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
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

/// Rectangles use one common logical coordinate space. Paint order follows
/// `pages`; each intersecting source is fetched once. No device bitmap is used.
pub(crate) fn capture(
    viewport: Rect,
    reference_index: usize,
    pages: &[PageRect],
    background: [u8; 4],
    mut fetch: impl FnMut(usize) -> Result<Vec<u8>, String>,
) -> Result<Capture, String> {
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
    let limits = reader_engine::DecodeLimits::default();
    let dimensions = reader_engine::inspect_image(&reference_bytes, limits)
        .map_err(|error| format!("Cannot inspect original chapter image: {error}"))?;
    let scale = f64::from(dimensions.width) / reference.rect.width;
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
    let remaining = MEMORY_LIMIT
        .checked_sub(output_bytes)
        .ok_or_else(budget_error)?;
    let limits = reader_engine::DecodeLimits {
        max_decoded_bytes: remaining,
        ..limits
    };
    // Admit and decode the reference before allocating the output. If it is
    // first in paint order, retain that decode; otherwise drop it before any
    // other page is decoded and reuse its fetched bytes when its turn arrives.
    let reference_image =
        reader_engine::decode_image(&reference_bytes, limits).map_err(|error| {
            format!(
                "Original chapter image exceeds capture admission: {error}. Use Device resolution."
            )
        })?;
    let mut reference_image = if visible
        .first()
        .is_some_and(|page| page.index == reference_index)
    {
        Some(reference_image)
    } else {
        drop(reference_image);
        None
    };
    let mut reference_bytes = Some(reference_bytes);
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_bytes as usize)
        .map_err(|_| budget_error())?;
    output.resize(output_bytes as usize, 0);
    for pixel in output.as_chunks_mut::<4>().0 {
        *pixel = background;
    }
    for page in visible {
        let image = if page.index == reference_index {
            let bytes = reference_bytes.take().ok_or_else(geometry_error)?;
            if let Some(image) = reference_image.take() {
                image
            } else {
                reader_engine::decode_image(&bytes, limits).map_err(|error| {
                    format!("Cannot decode original chapter image: {error}. Use Device resolution.")
                })?
            }
        } else {
            let bytes = fetch(page.index)?;
            reader_engine::decode_image(&bytes, limits).map_err(|error| {
                format!("Cannot decode original chapter image: {error}. Use Device resolution.")
            })?
        };
        paint(&mut output, &raster, page, &image)?;
        // No decoded page survives into the next iteration.
    }
    Ok(Capture {
        width,
        height,
        pixels: output,
    })
}

fn paint(
    output: &mut [u8],
    raster: &Raster,
    page: &PageRect,
    image: &image::RgbaImage,
) -> Result<(), String> {
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
    let mut step_x = f64::from(image.width()) / (rect.width * scale);
    let mut step_y = f64::from(image.height()) / (rect.height * scale);
    let (offset_x, offset_y) = if page.index == raster.reference_index {
        // Ignore only layout float rounding at a true native 1:1 ratio.
        step_x = 1.;
        if (step_y - 1.).abs() < 1e-6 {
            step_y = 1.;
        }
        (
            raster.reference_origin.0 - 0.5,
            raster.reference_origin.1 * step_y - 0.5,
        )
    } else {
        (
            (viewport.x - rect.x) / rect.width * f64::from(image.width()) - 0.5,
            (viewport.y - rect.y) / rect.height * f64::from(image.height()) - 0.5,
        )
    };
    if [step_x, step_y, offset_x, offset_y]
        .iter()
        .any(|value| !value.is_finite())
    {
        return Err(geometry_error());
    }
    for y in start_y..end_y {
        let source_y =
            ((f64::from(y) + 0.5) * step_y + offset_y).clamp(0., f64::from(image.height() - 1));
        for x in start_x..end_x {
            let source_x =
                ((f64::from(x) + 0.5) * step_x + offset_x).clamp(0., f64::from(image.width() - 1));
            let target = ((u64::from(y) * u64::from(width) + u64::from(x)) * 4) as usize;
            blend(&mut output[target..target + 4], image, source_x, source_y);
        }
    }
    Ok(())
}

/// Bilinear filtering uses premultiplied colors to avoid dark fringes around
/// transparent source pixels. Integer source coordinates remain exact.
fn blend(target: &mut [u8], image: &image::RgbaImage, x: f64, y: f64) {
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let fx = x - f64::from(x0);
    let fy = y - f64::from(y0);
    let first = image.get_pixel(x0, y0).0;
    if fx == 0. && fy == 0. && first[3] == 255 {
        target.copy_from_slice(&first);
        return;
    }
    let x1 = (x0 + 1).min(image.width() - 1);
    let y1 = (y0 + 1).min(image.height() - 1);
    let mut color = [0.; 3];
    let mut alpha = 0.;
    for (pixel, weight) in [
        (first, (1. - fx) * (1. - fy)),
        (image.get_pixel(x1, y0).0, fx * (1. - fy)),
        (image.get_pixel(x0, y1).0, (1. - fx) * fy),
        (image.get_pixel(x1, y1).0, fx * fy),
    ] {
        let coverage = f64::from(pixel[3]) / 255. * weight;
        alpha += coverage;
        for channel in 0..3 {
            color[channel] += f64::from(pixel[channel]) * coverage;
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
}

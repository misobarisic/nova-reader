//! The strip container: one stored file holding a tall page as independently
//! encoded row strips. Everything about the layout lives in this file and
//! nothing in it is visible outside `page_store`, so no other module can see
//! a strip, a strip table or the magic. Callers see a `PageImage`.
//!
//! ```text
//! magic      "NRSTRIP\0"                 8
//! version    u16 LE (= 1)                2
//! reserved   u16 LE (= 0)                2
//! width      u32 LE                      4
//! height     u32 LE                      4
//! strip_rows u32 LE, multiple of 16      4
//! count      u32 LE                      4
//! check      u64 LE, FNV-1a of all bytes before it and the table
//! table      count x { rows u32 LE, len u32 LE }
//! payload    the strips back to back, each a complete image file
//! ```
//!
//! The container replaces the whole page under its usual key or filename, so
//! eviction, clearing, atomic publication and checksum verification treat it as
//! the single file it is.
use reader_engine::{DecodeLimits, Error, PageDimensions, Result};
use std::ops::Range;

pub(super) const MAGIC: &[u8; 8] = b"NRSTRIP\0";
const VERSION: u16 = 1;
const FIXED: usize = 8 + 2 + 2 + 4 + 4 + 4 + 4 + 8;
const ENTRY: usize = 8;
/// A page cannot be taller than the engine's largest height, so this bounds
/// the table even for the smallest strips.
const MAX_STRIPS: u32 = 65_536 / 16;

fn fnv1a(parts: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in parts.iter().copied().flatten() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Strips are scrolled through, so decode speed is what they are for. Lossy
/// WebP is the cache's default codec, and libwebp (already linked for encoding)
/// decodes it several times faster than the pure-Rust decoder behind
/// `reader_engine::decode_image`. Anything libwebp declines (animation, a
/// stream it cannot read, dimensions outside the limits) takes the engine path,
/// which reports the error.
fn decode_webp(bytes: &[u8], limits: DecodeLimits) -> Option<image::RgbaImage> {
    if bytes.len() > limits.max_encoded_bytes
        || !(bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
    {
        return None;
    }
    let features = webp::BitstreamFeatures::new(bytes)?;
    if features.has_animation() {
        return None;
    }
    let (width, height) = (features.width(), features.height());
    reader_engine::admit_dimensions(PageDimensions { width, height }, limits).ok()?;
    let decoded = webp::Decoder::new(bytes).decode()?;
    let pixels = if decoded.is_alpha() {
        decoded.to_vec()
    } else {
        decoded
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect()
    };
    image::RgbaImage::from_raw(width, height, pixels)
}

struct Strip {
    rows: u32,
    bytes: Range<usize>,
}

pub(super) struct Container {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    strip_rows: u32,
    strips: Vec<Strip>,
}

pub(super) fn looks_like(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// Assemble a container from strips, each a complete encoded image of the
/// page's width. All strips but the last have exactly `strip_rows` rows.
pub(super) fn build(
    width: u32,
    height: u32,
    strip_rows: u32,
    strips: &[(u32, Vec<u8>)],
) -> std::result::Result<Vec<u8>, String> {
    let count = u32::try_from(strips.len()).map_err(|_| "too many strips")?;
    let mut table = Vec::with_capacity(strips.len() * ENTRY);
    let mut payload_len = 0usize;
    for (rows, bytes) in strips {
        let len = u32::try_from(bytes.len()).map_err(|_| "strip too large")?;
        table.extend_from_slice(&rows.to_le_bytes());
        table.extend_from_slice(&len.to_le_bytes());
        payload_len += bytes.len();
    }
    let mut out = Vec::with_capacity(FIXED + table.len() + payload_len);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&strip_rows.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    let check = fnv1a(&[&out, &table]);
    out.extend_from_slice(&check.to_le_bytes());
    out.extend_from_slice(&table);
    for (_, bytes) in strips {
        out.extend_from_slice(bytes);
    }
    // Never write what the reader would reject.
    Container::parse(out.clone()).map_err(|error| error.to_string())?;
    Ok(out)
}

impl Container {
    /// Validate the header and table against the file length. Strip payloads
    /// are validated when decoded, exactly as a whole image's pixels are.
    pub(super) fn parse(bytes: Vec<u8>) -> Result<Self> {
        let invalid = || Error::Layout;
        let u32_at = |at: usize| -> Result<u32> {
            Ok(u32::from_le_bytes(
                bytes
                    .get(at..at + 4)
                    .ok_or_else(invalid)?
                    .try_into()
                    .unwrap(),
            ))
        };
        if bytes.len() < FIXED || !looks_like(&bytes) {
            return Err(invalid());
        }
        let version = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
        let reserved = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
        if version != VERSION || reserved != 0 {
            return Err(invalid());
        }
        let (width, height) = (u32_at(12)?, u32_at(16)?);
        let (strip_rows, count) = (u32_at(20)?, u32_at(24)?);
        if width == 0
            || height == 0
            || strip_rows == 0
            || !strip_rows.is_multiple_of(16)
            || count == 0
            || count > MAX_STRIPS
            || count != height.div_ceil(strip_rows)
        {
            return Err(invalid());
        }
        let table_end = FIXED + count as usize * ENTRY;
        let table = bytes.get(FIXED..table_end).ok_or_else(invalid)?;
        let stored = u64::from_le_bytes(bytes[28..36].try_into().unwrap());
        if stored != fnv1a(&[&bytes[..28], table]) {
            return Err(invalid());
        }
        let mut strips = Vec::with_capacity(count as usize);
        let mut at = table_end;
        let mut remaining = height;
        for entry in table.as_chunks::<ENTRY>().0 {
            let rows = u32::from_le_bytes(entry[..4].try_into().unwrap());
            let len = u32::from_le_bytes(entry[4..].try_into().unwrap()) as usize;
            if rows != strip_rows.min(remaining) || len == 0 {
                return Err(invalid());
            }
            let end = at.checked_add(len).ok_or_else(invalid)?;
            strips.push(Strip {
                rows,
                bytes: at..end,
            });
            at = end;
            remaining -= rows;
        }
        // No trailing or missing bytes: the table accounts for the whole file.
        if at != bytes.len() {
            return Err(invalid());
        }
        Ok(Self {
            bytes,
            width,
            height,
            strip_rows,
            strips,
        })
    }

    pub(super) fn encoded_len(&self) -> usize {
        self.bytes.len()
    }
    pub(super) fn strip_rows(&self) -> u32 {
        self.strip_rows
    }
    /// The container file itself, as published.
    pub(super) fn stored_bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// The page's own size, before any admission policy.
    pub(super) fn size(&self) -> PageDimensions {
        PageDimensions {
            width: self.width,
            height: self.height,
        }
    }

    fn decode_strip(&self, index: usize, limits: DecodeLimits) -> Result<image::RgbaImage> {
        let strip = &self.strips[index];
        let bytes = &self.bytes[strip.bytes.clone()];
        let image = match decode_webp(bytes, limits) {
            Some(image) => image,
            None => reader_engine::decode_image(bytes, limits)?,
        };
        // A strip that does not match its table entry is corruption, never a
        // reason to composite misaligned rows.
        if image.width() != self.width || image.height() != strip.rows {
            return Err(Error::Layout);
        }
        Ok(image)
    }

    /// Decode `rows`, touching only the strips they intersect.
    pub(super) fn decode_rows(
        &self,
        rows: Range<u32>,
        limits: DecodeLimits,
    ) -> Result<image::RgbaImage> {
        if rows.start >= rows.end || rows.end > self.height {
            return Err(Error::Layout);
        }
        let first = (rows.start / self.strip_rows) as usize;
        let last = ((rows.end - 1) / self.strip_rows) as usize;
        let strip_top = |index: usize| index as u32 * self.strip_rows;
        if first == last
            && rows.start == strip_top(first)
            && rows.end - rows.start == self.strips[first].rows
        {
            return self.decode_strip(first, limits);
        }
        let stride = self.width as usize * 4;
        let mut out = Vec::new();
        out.try_reserve_exact(stride * (rows.end - rows.start) as usize)
            .map_err(|_| Error::Limit)?;
        for index in first..=last {
            let strip = self.decode_strip(index, limits)?;
            let top = strip_top(index);
            let from = rows.start.max(top) - top;
            let to = rows.end.min(top + strip.height()) - top;
            out.extend_from_slice(&strip.as_raw()[from as usize * stride..to as usize * stride]);
        }
        image::RgbaImage::from_raw(self.width, rows.end - rows.start, out).ok_or(Error::Layout)
    }

    /// Decode the whole page. The caller has already admitted its dimensions.
    pub(super) fn decode(&self, limits: DecodeLimits) -> Result<image::RgbaImage> {
        self.decode_rows(0..self.height, limits)
    }

    /// Fully decode every strip in turn, keeping only one in memory. This is
    /// the check a freshly downloaded page gets: truncated or forged strips
    /// fail here, not on first view.
    pub(super) fn validate(&self, limits: DecodeLimits) -> Result<()> {
        for index in 0..self.strips.len() {
            self.decode_strip(index, limits)?;
        }
        Ok(())
    }
}

#[cfg(test)]
impl Container {
    pub(super) fn strip_count(&self) -> usize {
        self.strips.len()
    }
    pub(super) fn strip_rows_at(&self, index: usize) -> u32 {
        self.strips[index].rows
    }
    pub(super) fn strip_range(&self, index: usize) -> Range<usize> {
        self.strips[index].bytes.clone()
    }
    pub(super) fn strip_bytes(&self, index: usize) -> &[u8] {
        &self.bytes[self.strip_range(index)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32, height: u32, alpha: bool) -> image::RgbaImage {
        image::RgbaImage::from_fn(width, height, |x, y| {
            let n = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
            image::Rgba([
                (x * 255 / width) as u8,
                (y * 255 / height) as u8,
                (n >> 9) as u8 / 4 + 100,
                if alpha { 255 - (x % 7) as u8 * 30 } else { 255 },
            ])
        })
    }

    #[test]
    fn libwebp_strips_decode_like_the_engine_and_odd_streams_defer_to_it() {
        let limits = DecodeLimits::default();
        for (lossless, alpha) in [(false, false), (false, true), (true, false), (true, true)] {
            let image = picture(61, 130, alpha);
            let bytes = webp::Encoder::from_rgba(image.as_raw(), 61, 130)
                .encode_simple(lossless, 85.)
                .unwrap()
                .to_vec();
            let fast = decode_webp(&bytes, limits).expect("libwebp reads its own output");
            let engine = reader_engine::decode_image(&bytes, limits).unwrap();
            assert_eq!(fast.dimensions(), engine.dimensions());
            if lossless {
                assert_eq!(fast, image, "lossless stays exact");
                assert_eq!(fast, engine);
            } else {
                // Two correct VP8 decoders may round chroma differently.
                let worst = fast
                    .as_raw()
                    .iter()
                    .zip(engine.as_raw())
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                assert!(worst <= 8, "alpha {alpha}: decoders differ by {worst}");
            }
        }
        // Not WebP, truncated, or over the limits: the engine decides.
        let image = picture(40, 40, false);
        let webp = webp::Encoder::from_rgba(image.as_raw(), 40, 40)
            .encode_simple(false, 85.)
            .unwrap()
            .to_vec();
        assert!(decode_webp(b"\x89PNG\r\n", limits).is_none());
        assert!(decode_webp(&webp[..webp.len() / 2], limits).is_none());
        assert!(
            decode_webp(
                &webp,
                DecodeLimits {
                    max_width: 39,
                    ..limits
                }
            )
            .is_none()
        );
        assert!(
            decode_webp(
                &webp,
                DecodeLimits {
                    max_encoded_bytes: 10,
                    ..limits
                }
            )
            .is_none()
        );
    }
}

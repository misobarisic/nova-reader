//! The engine's view of a stored page: something with dimensions that can hand
//! out decoded rows. A whole encoded response is one such source; a page stored
//! as independently encoded row strips is another. The engine never learns how a
//! source is laid out on disk, only the row granularity at which decoding it is
//! cheap, so the display pipeline treats both identically.
use crate::{DecodeLimits, Error, PageDimensions, Result, decode_image, inspect_image};
use std::{ops::Range, sync::Arc};

pub trait PageSource: Send + Sync {
    /// Page dimensions, admitted exactly as [`inspect_image`] admits a whole
    /// image. A split page must not become admissible (or inadmissible) merely
    /// because of how it is stored.
    fn dimensions(&self, limits: DecodeLimits) -> Result<PageDimensions>;
    /// Stored size, for caller memory accounting.
    fn encoded_len(&self) -> usize;
    /// Rows per independently decodable band, or `None` when only the whole
    /// page can be decoded. Bands start at multiples of this value.
    fn row_granularity(&self) -> Option<u32> {
        None
    }
    /// Decode `rows` at the page's own width. Cost is proportional to the bands
    /// the range touches when `row_granularity` is set, and to the whole page
    /// otherwise.
    fn decode_rows(&self, rows: Range<u32>, limits: DecodeLimits) -> Result<image::RgbaImage>;
    /// Decode the entire page.
    fn decode(&self, limits: DecodeLimits) -> Result<image::RgbaImage>;
}

/// A whole encoded response (PNG, JPEG, WebP, AVIF or JPEG XL).
impl PageSource for Vec<u8> {
    fn dimensions(&self, limits: DecodeLimits) -> Result<PageDimensions> {
        inspect_image(self, limits)
    }
    fn encoded_len(&self) -> usize {
        self.len()
    }
    fn decode_rows(&self, rows: Range<u32>, limits: DecodeLimits) -> Result<image::RgbaImage> {
        let image = decode_image(self, limits)?;
        if rows.start >= rows.end || rows.end > image.height() {
            return Err(Error::Layout);
        }
        if rows.start == 0 && rows.end == image.height() {
            return Ok(image);
        }
        Ok(
            image::imageops::crop_imm(&image, 0, rows.start, image.width(), rows.end - rows.start)
                .to_image(),
        )
    }
    fn decode(&self, limits: DecodeLimits) -> Result<image::RgbaImage> {
        decode_image(self, limits)
    }
}

/// Anything that can be shared as a [`PageSource`] with a stable identity: an
/// `Arc` of a concrete source, or an already type-erased one. The display
/// decoder keys its retained pixels by this identity.
pub trait SharedPage {
    fn shared(&self) -> Arc<dyn PageSource>;
    fn page(&self) -> &dyn PageSource;
    /// Address of the shared allocation, for identity comparison.
    fn identity(&self) -> *const ();
}
impl<S: PageSource + 'static> SharedPage for Arc<S> {
    fn shared(&self) -> Arc<dyn PageSource> {
        self.clone()
    }
    fn page(&self) -> &dyn PageSource {
        &**self
    }
    fn identity(&self) -> *const () {
        Arc::as_ptr(self).cast()
    }
}
impl SharedPage for Arc<dyn PageSource> {
    fn shared(&self) -> Arc<dyn PageSource> {
        self.clone()
    }
    fn page(&self) -> &dyn PageSource {
        &**self
    }
    fn identity(&self) -> *const () {
        Arc::as_ptr(self).cast()
    }
}

//! Reader delegates JPEG XL to Nova Kit's shared bounded codec.
use super::{DecodeLimits, Error, PageDimensions, Result};

pub(super) fn inspect(bytes: &[u8], limits: DecodeLimits) -> Result<PageDimensions> {
    nova_image::inspect_jxl(bytes, limits).map_err(Error::from)
}

pub(super) fn decode(bytes: &[u8], limits: DecodeLimits) -> Result<image::RgbaImage> {
    nova_image::decode_jxl(bytes, limits).map_err(Error::from)
}

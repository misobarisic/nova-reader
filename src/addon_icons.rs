//! Package decoration decoded on workers; retained image handles stay on the UI
//! thread, like Nova's addon logos. Bundled icons never make network requests.
use reader_sources::{SourceDescriptor, SourceRegistry};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::{cell::RefCell, collections::BTreeMap};

pub struct PreparedIcon {
    id: String,
    digest: String,
    pixels: SharedPixelBuffer<Rgba8Pixel>,
}
thread_local! {
    static ICONS: RefCell<BTreeMap<String, (String, Image)>> = RefCell::default();
}

pub fn prepare(registry: &SourceRegistry, addons: &[SourceDescriptor]) -> Vec<PreparedIcon> {
    use sha2::{Digest, Sha256};
    // At most 8 MiB of retained 64×64 icons, independent of source variants.
    addons
        .iter()
        .take(512)
        .filter_map(|addon| {
            let bytes = registry.icon(&addon.id).ok()??;
            let limits = reader_engine::DecodeLimits {
                max_encoded_bytes: 256 * 1024,
                max_width: 512,
                max_height: 512,
                max_pixels: 512 * 512,
                max_decoded_bytes: 4 * 1024 * 1024,
            };
            let image = reader_engine::decode_display_image(&bytes, limits, 64, 64).ok()?;
            let pixels =
                SharedPixelBuffer::clone_from_slice(image.as_raw(), image.width(), image.height());
            Some(PreparedIcon {
                id: addon.id.clone(),
                digest: format!("{:x}", Sha256::digest(&bytes)),
                pixels,
            })
        })
        .collect()
}

/// Publish only with the same current-generation gate as the addon list.
pub fn publish(prepared: Vec<PreparedIcon>) {
    ICONS.with_borrow_mut(|icons| {
        let mut previous = std::mem::take(icons);
        for icon in prepared {
            let image = previous
                .remove(&icon.id)
                .filter(|(digest, _)| digest == &icon.digest)
                .map(|(_, image)| image)
                .unwrap_or_else(|| Image::from_rgba8(icon.pixels));
            icons.insert(icon.id, (icon.digest, image));
        }
    });
}

pub fn image(id: &str) -> Image {
    ICONS.with_borrow(|icons| {
        icons
            .get(id)
            .map(|(_, image)| image.clone())
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn malformed_icon_pixels_fail_without_affecting_source_metadata() {
        let bytes = include_bytes!("../examples/addons/mangafire/icon.png");
        let image = reader_engine::decode_display_image(
            bytes,
            reader_engine::DecodeLimits::default(),
            64,
            64,
        )
        .unwrap();
        assert_eq!(image.dimensions(), (64, 64));
        assert!(
            reader_engine::decode_display_image(
                &bytes[..24],
                reader_engine::DecodeLimits::default(),
                64,
                64
            )
            .is_err()
        );
    }
}

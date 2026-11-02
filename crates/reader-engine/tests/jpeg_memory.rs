use image::{ExtendedColorType, ImageEncoder};
use reader_engine::{DecodeLimits, decode_image};

#[test]
fn eight_bit_decode_matches_reference_with_only_one_rgba_output_budget() {
    for (color, channels) in [
        (ExtendedColorType::L8, 1),
        (ExtendedColorType::La8, 2),
        (ExtendedColorType::Rgb8, 3),
        (ExtendedColorType::Rgba8, 4),
    ] {
        let source: Vec<_> = (0..12 * channels).map(|v| ((v * 37) % 256) as u8).collect();
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&source, 4, 3, color)
            .unwrap();
        let expected = image::load_from_memory(&bytes).unwrap().to_rgba8();
        let limits = DecodeLimits {
            max_decoded_bytes: 48,
            ..Default::default()
        };
        assert_eq!(decode_image(&bytes, limits).unwrap(), expected);
        assert!(
            decode_image(
                &bytes,
                DecodeLimits {
                    max_decoded_bytes: 47,
                    ..limits
                }
            )
            .is_err()
        );
    }
}

#[test]
fn four_k_by_six_k_jpeg_fits_the_unchanged_default_memory_budget() {
    let source = image::RgbImage::from_pixel(4096, 6144, image::Rgb([40, 80, 120]));
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
        .encode_image(&source)
        .unwrap();
    drop(source);
    assert!(bytes.len() < DecodeLimits::default().max_encoded_bytes);
    let decoded = decode_image(&bytes, DecodeLimits::default()).unwrap();
    assert_eq!(decoded.dimensions(), (4096, 6144));
    assert_eq!(decoded.as_raw().len(), 4096 * 6144 * 4);
    let pixel = decoded.get_pixel(2048, 3072);
    assert!(
        pixel[0].abs_diff(40) <= 2 && pixel[1].abs_diff(80) <= 2 && pixel[2].abs_diff(120) <= 2
    );
    assert_eq!(pixel[3], 255);
    drop(decoded);
    let display =
        reader_engine::decode_display_image(&bytes, DecodeLimits::default(), 1170, 2000).unwrap();
    assert_eq!(display.width(), 1170);
    assert!(display.height() <= 2000);
}

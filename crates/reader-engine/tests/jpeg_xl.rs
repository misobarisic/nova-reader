use reader_engine::{DecodeLimits, decode_image, inspect_image, is_jpeg_xl};
const RGB: &[u8] = include_bytes!("fixtures/jxl/rgb.jxl");
const RGBA: &[u8] = include_bytes!("fixtures/jxl/rgba.jxl");

#[test]
fn jpeg_xl_preserves_native_rgb_alpha_and_admission() {
    for bytes in [RGB, RGBA] {
        assert!(is_jpeg_xl(bytes));
        let size = inspect_image(bytes, DecodeLimits::default()).unwrap();
        assert_eq!((size.width, size.height), (8, 6));
        let decoded = decode_image(bytes, DecodeLimits::default()).unwrap();
        for y in 0..6 {
            for x in 0..8 {
                let expected = if bytes == RGB {
                    [x as u8 * 30, y as u8 * 40, (x + y) as u8 * 16, 255]
                } else {
                    [40, 120, 200, (x + y) as u8 * 19]
                };
                assert_eq!(decoded.get_pixel(x, y).0, expected);
            }
        }
        assert!(
            inspect_image(
                bytes,
                DecodeLimits {
                    max_width: 7,
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            decode_image(
                bytes,
                DecodeLimits {
                    max_decoded_bytes: 64,
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            inspect_image(
                bytes,
                DecodeLimits {
                    max_encoded_bytes: bytes.len() - 1,
                    ..Default::default()
                }
            )
            .is_err()
        );
        for end in [2, bytes.len() / 2, bytes.len() - 1] {
            assert!(decode_image(&bytes[..end], DecodeLimits::default()).is_err());
        }
    }
    assert!(!is_jpeg_xl(b"not an image"));
}

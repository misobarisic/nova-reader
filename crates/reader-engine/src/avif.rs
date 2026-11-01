//! Reader's AVIF boundary delegates to Nova Kit's shared bounded codec.
use super::{DecodeLimits, Error, PageDimensions, Result};

pub(super) fn inspect(bytes: &[u8], limits: DecodeLimits) -> Result<PageDimensions> {
    nova_image::inspect_avif(bytes, limits).map_err(Error::from)
}

pub(super) fn decode(bytes: &[u8], limits: DecodeLimits) -> Result<image::RgbaImage> {
    nova_image::decode_avif(bytes, limits).map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    const TINY: &[u8] = include_bytes!("../tests/fixtures/tiny.avif");

    #[test]
    fn independent_avifenc_fixture_decodes_with_correct_colors() {
        let image = decode(TINY, DecodeLimits::default()).unwrap();
        assert_eq!(image.dimensions(), (16, 16));
        // Reference pixels decoded independently with Pillow/libavif. Allow a
        // few units for the YUV converter's integer rounding differences.
        for (position, expected) in [
            ((0, 0), [255u8, 0, 2, 255]),
            ((8, 8), [118, 0, 141, 255]),
            ((15, 15), [2, 2, 249, 255]),
        ] {
            let actual = image.get_pixel(position.0, position.1);
            assert!(
                actual
                    .0
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.abs_diff(expected) <= 3),
                "reference pixel {position:?}: {actual:?} != {expected:?}"
            );
        }
        let display = crate::decode_display_image(TINY, DecodeLimits::default(), 8, 8).unwrap();
        assert_eq!(display.dimensions(), (8, 8));
    }

    #[test]
    fn avif_rejects_corrupt_payload_and_truncation_without_aborting() {
        // Upstream rav1d 1.1.0 aborts on this exact malformed frame. The pinned
        // BSD fork repairs its missing-frame-header cleanup path.
        let mut corrupt = TINY.to_vec();
        corrupt[280] ^= 1;
        assert!(decode(&corrupt, DecodeLimits::default()).is_err());
        let mut metadata = TINY.to_vec();
        metadata[209] ^= 1; // pixi channel count must match its bounded payload
        assert!(decode(&metadata, DecodeLimits::default()).is_err());
        for length in [0, 8, 24, 100, TINY.len() - 1] {
            assert!(inspect(&TINY[..length], DecodeLimits::default()).is_err());
        }
    }

    #[test]
    fn avif_admission_rejects_dimensions_bytes_and_working_budget_before_decode() {
        let limits = DecodeLimits::default();
        for limits in [
            DecodeLimits {
                max_width: 15,
                ..limits
            },
            DecodeLimits {
                max_height: 15,
                ..limits
            },
            DecodeLimits {
                max_pixels: 255,
                ..limits
            },
            DecodeLimits {
                max_encoded_bytes: TINY.len() - 1,
                ..limits
            },
            DecodeLimits {
                max_decoded_bytes: 1024,
                ..limits
            },
        ] {
            assert!(matches!(inspect(TINY, limits), Err(Error::Limit)));
        }
    }

    #[test]
    #[ignore = "targeted process-isolated AVIF corruption sweep"]
    fn malformed_fixture_does_not_abort_decoder_process() {
        const PROBE: &str = "NOVA_READER_AVIF_CORRUPTION_PROBE";
        if let Ok(probe) = std::env::var(PROBE) {
            let probe = probe.parse::<usize>().unwrap();
            let mut bytes = TINY.to_vec();
            bytes[probe / 8] ^= 1 << (probe % 8);
            let _ = crate::decode_image(&bytes, DecodeLimits::default());
            return;
        }
        let executable = std::env::current_exe().unwrap();
        for byte in 0..TINY.len() {
            for bit in [0, 7] {
                let output = std::process::Command::new(&executable)
                    .args([
                        "--ignored",
                        "--exact",
                        "avif::tests::malformed_fixture_does_not_abort_decoder_process",
                    ])
                    .env(PROBE, (byte * 8 + bit).to_string())
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "AVIF mutation byte {byte}, bit {bit} aborted: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

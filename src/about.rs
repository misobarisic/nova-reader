//! Nova's actual About runtime, copied from immutable revision
//! b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda:
//! src/app/about.rs and the license source catalog at the top of src/app.rs.
//! Reader adapts its workspace names, identity, and standalone Slint event-loop
//! dispatcher; decompression, text chunking, source ordering and fallbacks stay
//! the same. Navigation is wired by Reader's existing More shell.
use crate::{AppWindow, LicenseSource};
use slint::{ComponentHandle, SharedString, VecModel};
use std::{rc::Rc, thread};

pub(crate) const OPEN_SOURCE_LICENSE_CATALOG_ZSTD: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/nova_license_catalog.zst"));
include!(concat!(env!("OUT_DIR"), "/nova_license_catalog_size.rs"));

pub(crate) fn open_source_license_sources() -> Vec<LicenseSource> {
    const WORKSPACE_CRATES: [&str; 8] = [
        "nova-reader",
        "reader-capture",
        "reader-domain",
        "reader-sources",
        "reader-engine",
        "reader-downloads",
        "reader-catalogue",
        "nova-reader-ui",
    ];

    let mut sources = include_str!(concat!(env!("OUT_DIR"), "/nova_license_sources.tsv"))
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let label = fields.next()?;
            let license = fields.next()?;
            let url = fields.next()?;
            if label.is_empty() {
                return None;
            }
            let package_name = label.split_whitespace().next()?;
            if WORKSPACE_CRATES.contains(&package_name) {
                return None;
            }
            let url = if url.starts_with("https://") {
                url.to_string()
            } else {
                let mut crate_parts = label.split_whitespace();
                let name = crate_parts.next()?;
                let version = crate_parts.next()?;
                format!("https://crates.io/crates/{name}/{version}")
            };
            Some(LicenseSource {
                label: label.into(),
                license: license.into(),
                url: url.into(),
            })
        })
        .collect::<Vec<_>>();

    sources.sort_by_key(|a| a.label.to_lowercase());
    sources.dedup_by(|a, b| a.label == b.label && a.url == b.url);
    sources
}

pub(crate) fn prepare_license_text(app: &AppWindow) {
    let weak = app.as_weak();
    thread::spawn(move || {
        let chunks = match decode_license_catalog() {
            Ok(catalog) => license_text_chunks(&catalog),
            Err(error) => {
                eprintln!("Nova Reader: could not decode bundled license catalog: {error}");
                return;
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_license_text_chunks(Rc::new(VecModel::from(chunks)).into());
            }
        });
    });
}

fn decode_license_catalog() -> std::io::Result<String> {
    // Allocate once at the compile-time size, and pass the same bound to zstd.
    // No guessed capacity or growing output buffer is needed for bundled data.
    let mut bytes = vec![0; OPEN_SOURCE_LICENSE_CATALOG_SIZE];
    let written =
        zstd::bulk::decompress_to_buffer(OPEN_SOURCE_LICENSE_CATALOG_ZSTD, &mut bytes[..])?;
    if written != bytes.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bundled license catalog size mismatch",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

// Bound every Text layout independently, including catalogs with unbroken
// license identifiers. Slice only at Unicode scalar boundaries and retain all
// characters so the model remains an exact representation of the legal text.
pub(crate) fn license_text_chunks(catalog: &str) -> Vec<SharedString> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < catalog.len() {
        let (mut end, mut last_newline, mut last_space, mut lines) = (start, None, None, 0);
        for (offset, ch) in catalog[start..].char_indices().take(512) {
            end = start + offset + ch.len_utf8();
            if ch.is_whitespace() {
                last_space = Some(end);
            }
            if ch == '\n' {
                last_newline = Some(end);
                lines += 1;
                if lines == 16 {
                    break;
                }
            }
        }
        // Prefer a complete line, then a word boundary. Hard-split only
        // unbroken tokens; every split remains within the same work bound.
        let boundary = if end == catalog.len() {
            end
        } else {
            last_newline.or(last_space).unwrap_or(end)
        };
        chunks.push(catalog[start..boundary].into());
        start = boundary;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::{decode_license_catalog, license_text_chunks, open_source_license_sources};

    #[test]
    fn bundled_license_catalog_decodes_without_changing_legal_text() {
        let original = include_str!(concat!(env!("OUT_DIR"), "/nova_license_catalog.txt"));
        let decoded = decode_license_catalog().unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.len(), super::OPEN_SOURCE_LICENSE_CATALOG_SIZE);
        assert!(super::OPEN_SOURCE_LICENSE_CATALOG_ZSTD.len() < decoded.len() / 2);
        assert_eq!(
            license_text_chunks(&decoded)
                .iter()
                .map(|s| s.as_str())
                .collect::<String>(),
            original
        );
    }

    #[test]
    fn license_chunks_preserve_utf8_content_and_bound_text_layout_work() {
        for catalog in [
            String::new(),
            "ž漢🙂".repeat(1000),
            "line\n".repeat(1000),
            format!("{}\n{}", "GPL ž🙂\n".repeat(20000), "X".repeat(10000)),
        ] {
            let chunks = license_text_chunks(&catalog);
            assert_eq!(
                chunks.iter().map(|s| s.as_str()).collect::<String>(),
                catalog
            );
            for chunk in chunks {
                assert!(!chunk.is_empty());
                assert!(chunk.chars().count() <= 512);
                assert!(chunk.chars().filter(|&ch| ch == '\n').count() <= 16);
            }
        }
    }

    #[test]
    fn bundled_catalog_retains_complete_reader_and_vendor_notices() {
        let catalog = decode_license_catalog().unwrap();
        for text in [
            include_str!("../LICENSE"),
            include_str!("../THIRD_PARTY_NOTICES.md"),
            include_str!("../assets/licenses/avif-decoder-BSD-2-Clause.txt"),
            include_str!("../crates/reader-engine/AVIF-NOTICE.txt"),
            include_str!("../assets/fonts/LICENSE.txt"),
        ] {
            assert!(
                catalog.contains(text),
                "bundled legal text must remain complete and unchanged"
            );
        }
    }

    #[test]
    fn bundled_source_rows_preserve_provenance_order_and_https_fallbacks() {
        let catalog = include_str!(concat!(env!("OUT_DIR"), "/nova_license_sources.tsv"));
        let sources = open_source_license_sources();
        assert!(!sources.is_empty());
        for pair in sources.windows(2) {
            assert!(pair[0].label.to_lowercase() <= pair[1].label.to_lowercase());
            assert!(pair[0].label != pair[1].label || pair[0].url != pair[1].url);
        }
        for source in &sources {
            let fields = catalog
                .lines()
                .filter_map(|line| {
                    let fields = line.splitn(3, '\t').collect::<Vec<_>>();
                    (fields.len() == 3 && fields[0] == source.label.as_str()).then_some(fields)
                })
                .find(|fields| {
                    if fields[2].starts_with("https://") {
                        fields[2] == source.url.as_str()
                    } else {
                        let parts = fields[0].split_whitespace().collect::<Vec<_>>();
                        parts.len() >= 2
                            && source.url.as_str()
                                == format!("https://crates.io/crates/{}/{}", parts[0], parts[1])
                    }
                })
                .expect(
                    "source row must retain a bundled provenance URL or Nova's crates.io fallback",
                );
            assert_eq!(source.license, fields[1]);
            assert!(source.url.starts_with("https://"));
            assert!(
                ![
                    "nova-reader",
                    "reader-capture",
                    "reader-domain",
                    "reader-sources",
                    "reader-engine",
                    "reader-downloads",
                    "reader-catalogue",
                    "nova-reader-ui"
                ]
                .contains(&source.label.split_whitespace().next().unwrap())
            );
        }
        // Nova filters its own nova-storage workspace crate. Reader consumes
        // that crate from Kit, so its dependency provenance must stay visible.
        assert!(
            sources
                .iter()
                .any(|source| source.label.starts_with("nova-storage "))
        );
        assert!(
            !sources
                .iter()
                .any(|source| source.label.starts_with("reader-capture ")),
            "workspace crates are first-party, not third-party credits"
        );
        // Provenance rows must name the revision Cargo actually pins.
        let pin = include_str!("../Cargo.toml")
            .split("nova-kit\", rev = \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap();
        let project = include_str!("../assets/open_source_project_sources.tsv");
        assert!(project.contains(pin), "kit revision {pin} must be credited");
        assert!(!project.contains("f2b7ebe"), "stale kit revision");
        assert!(project.contains("Nova shared sync ("));
        for row in include_str!("../assets/open_source_project_sources.tsv").lines() {
            let fields = row.splitn(3, '\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 3, "project provenance must use TSV fields");
            assert!(sources.iter().any(|source| {
                source.label == fields[0] && source.license == fields[1] && source.url == fields[2]
            }));
        }
    }
}

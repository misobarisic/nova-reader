//! Exercise the production service callback before durable hashes are recorded.
use nova_reader::{
    image_storage::{ImageCompression, ImageStorageFormat, ImageStoragePreferences},
    services::Services,
};
use reader_sources::CancelToken;
use serde_json::json;
use std::{
    io::{Cursor, Write},
    path::Path,
};

fn fixture_package() -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/addons/neutral-fixture");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let mut names = vec!["manifest.json".to_owned(), "main.js".to_owned()];
    names.extend(
        manifest["assets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap().to_owned()),
    );
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in names {
        zip.start_file(
            &name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&std::fs::read(root.join(name)).unwrap())
            .unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn save_preferences(settings: &ImageStoragePreferences) {
    nova_storage::try_set_str(
        "reader:image-storage",
        &serde_json::to_string(settings).unwrap(),
    )
    .unwrap();
}
#[test]
fn only_new_download_pages_follow_compression_and_keep_offline_integrity() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::open(directory.path()).unwrap();
    services.install(&fixture_package()).unwrap();
    let source = services.sources.source("neutral-fixture").unwrap();
    let item = source.browse(&json!({"page":1})).unwrap().items.remove(0);
    let series = services.series("neutral-fixture", item);
    let (_, chapters) = services.details(&series, &CancelToken::default()).unwrap();
    let chapter = chapters
        .iter()
        .find(|chapter| chapter.id.local_id.ends_with("mixed"))
        .unwrap();
    let cancel = CancelToken::default();
    services.prepare_download(&chapter.id, &cancel).unwrap();
    let mut calls = 0;
    let interrupted = services.downloads.run(&chapter.id, |page| {
        calls += 1;
        if calls == 2 {
            return Err("intentional interruption".into());
        }
        source
            .fetch_page(&page.url, &page.headers, &cancel)
            .map_err(|error| error.to_string())
    });
    assert!(interrupted.is_err());
    assert_eq!(
        services
            .downloads
            .job(&chapter.id)
            .unwrap()
            .completed_pages(),
        1
    );
    let (_, pages) = services.pages(&chapter.id, &cancel).unwrap();
    let originals = pages
        .iter()
        .map(|page| {
            source
                .fetch_page(&page.url, &page.headers, &cancel)
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(
        originals
            .iter()
            .all(|bytes| image::guess_format(bytes).unwrap() == image::ImageFormat::Png)
    );
    let mut preferences = ImageStoragePreferences {
        downloads: ImageCompression {
            enabled: true,
            format: ImageStorageFormat::Jpeg,
            quality: 35,
            jxl_lossless_jpeg: false,
            lossless_inputs: false,
            ..ImageCompression::downloads_default()
        },
        ..ImageStoragePreferences::default()
    };
    save_preferences(&preferences);
    services.download(&chapter.id, &cancel).unwrap();
    let offline = services.downloads.offline(&chapter.id).unwrap();
    let encoded = offline
        .pages
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        encoded[0], originals[0],
        "verified partial pages must remain byte-identical"
    );
    for (saved, original) in encoded.iter().skip(1).zip(originals.iter().skip(1)) {
        assert_eq!(
            image::guess_format(saved).unwrap(),
            image::ImageFormat::Jpeg
        );
        let limits = reader_engine::DecodeLimits::default();
        assert_eq!(
            reader_engine::inspect_image(saved, limits).unwrap(),
            reader_engine::inspect_image(original, limits).unwrap()
        );
    }
    preferences.downloads.format = ImageStorageFormat::Webp;
    save_preferences(&preferences);
    services.download(&chapter.id, &cancel).unwrap();
    for (path, previous) in offline.pages.iter().zip(&encoded) {
        assert_eq!(
            &std::fs::read(path).unwrap(),
            previous,
            "completed downloads are never rewritten"
        );
    }
    // Turning compression off affects a later download, preserving source bytes.
    preferences.downloads.enabled = false;
    save_preferences(&preferences);
    let other = chapters
        .iter()
        .find(|candidate| candidate.id != chapter.id)
        .unwrap();
    services.download(&other.id, &cancel).unwrap();
    let (_, resources) = services.pages(&other.id, &cancel).unwrap();
    let local = services.downloads.offline(&other.id).unwrap();
    for (resource, path) in resources.iter().zip(local.pages) {
        assert_eq!(
            std::fs::read(path).unwrap(),
            source
                .fetch_page(&resource.url, &resource.headers, &cancel)
                .unwrap()
        );
    }
    preferences.downloads.enabled = true;
    preferences.downloads.format = ImageStorageFormat::Avif;
    save_preferences(&preferences);
    let avif_chapter = chapters
        .iter()
        .find(|candidate| candidate.id.local_id.ends_with("wide"))
        .unwrap();
    // Follow the production download path, including hashing/offline validation:
    // low quality must not damage recognized lossless input pixels in either codec.
    let (_, lossless_resources) = services.pages(&avif_chapter.id, &cancel).unwrap();
    let limits = reader_engine::DecodeLimits::default();
    let source_pixels = lossless_resources
        .iter()
        .map(|page| {
            let bytes = source
                .fetch_page(&page.url, &page.headers, &cancel)
                .unwrap();
            reader_engine::decode_image(&bytes, limits)
                .unwrap()
                .into_raw()
        })
        .collect::<Vec<_>>();
    for format in [ImageStorageFormat::Webp, ImageStorageFormat::JxlLossy] {
        preferences.downloads.format = format;
        preferences.downloads.quality = 10;
        preferences.downloads.lossless_inputs = true;
        save_preferences(&preferences);
        services.download(&avif_chapter.id, &cancel).unwrap();
        let saved = services.downloads.offline(&avif_chapter.id).unwrap();
        for (path, original) in saved.pages.iter().zip(&source_pixels) {
            let bytes = std::fs::read(path).unwrap();
            if format == ImageStorageFormat::Webp {
                assert_eq!(
                    image::guess_format(&bytes).unwrap(),
                    image::ImageFormat::WebP
                );
            } else {
                assert!(nova_image::is_jpeg_xl(&bytes));
            }
            assert_eq!(
                reader_engine::decode_image(&bytes, limits)
                    .unwrap()
                    .as_raw(),
                original
            );
        }
        services.downloads.remove(&avif_chapter.id).unwrap();
    }
    preferences.downloads.format = ImageStorageFormat::Avif;
    preferences.downloads.quality = 35;
    save_preferences(&preferences);
    services.download(&avif_chapter.id, &cancel).unwrap();
    let avif_offline = services.downloads.offline(&avif_chapter.id).unwrap();
    let avif_bytes = avif_offline
        .pages
        .iter()
        .map(std::fs::read)
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    for bytes in &avif_bytes {
        assert_eq!(
            image::guess_format(bytes).unwrap(),
            image::ImageFormat::Avif
        );
        assert_eq!(
            reader_engine::decode_image(bytes, reader_engine::DecodeLimits::default())
                .unwrap()
                .dimensions(),
            (1800, 600)
        );
    }
    preferences.downloads.enabled = false;
    save_preferences(&preferences);
    services.sources.remove("neutral-fixture").unwrap();
    services.download(&avif_chapter.id, &cancel).unwrap();
    for (path, original) in avif_offline.pages.iter().zip(&avif_bytes) {
        assert_eq!(
            &std::fs::read(path).unwrap(),
            original,
            "existing AVIF bundles remain unchanged after policy/source removal"
        );
    }
    assert_eq!(
        services.downloads.offline(&chapter.id).unwrap().pages.len(),
        encoded.len()
    );
    // Deleting a source-free offline bundle removes only its durable files and
    // job. The library, resume position, bookmarks and metadata stay intact.
    services.add_library(&series.id).unwrap();
    services
        .domain
        .update(|state| {
            state.set_progress(reader_domain::ReadingProgress {
                chapter: chapter.id.clone(),
                anchor: reader_domain::ResumeAnchor::new(pages[0].id.clone(), 0, 0.375)?,
                mode: reader_domain::ReadingMode::Webtoon,
                last_read_at: 12345,
                completed: false,
            });
            state.bookmarks.push(reader_domain::Bookmark {
                chapter: chapter.id.clone(),
                anchor: reader_domain::ResumeAnchor::new(pages[0].id.clone(), 0, 0.375)?,
                note: "Keep my bookmark".into(),
            });
            Ok(())
        })
        .unwrap();
    let before = services.domain.snapshot().unwrap();
    let bundle = offline.pages[0].parent().unwrap().to_path_buf();
    // List-status refreshes read manifests only. Temporarily withholding one
    // large page must neither hash payloads nor rewrite its persisted status.
    let held = bundle.join("temporarily-held.page");
    std::fs::rename(&offline.pages[0], &held).unwrap();
    assert_eq!(
        services.downloads.statuses().unwrap().get(&chapter.id),
        Some(&reader_downloads::JobStatus::Complete)
    );
    std::fs::rename(held, &offline.pages[0]).unwrap();
    let other_offline = services.downloads.offline(&other.id).unwrap();
    let other_bytes = other_offline
        .pages
        .iter()
        .map(std::fs::read)
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    services.downloads.remove(&chapter.id).unwrap();
    assert!(!bundle.exists());
    assert!(services.downloads.job(&chapter.id).is_err());
    assert!(services.downloads.offline(&chapter.id).is_err());
    assert_eq!(services.domain.snapshot().unwrap(), before);
    assert_eq!(
        services.downloads.offline(&other.id).unwrap().pages,
        other_offline.pages
    );
    for (path, bytes) in other_offline.pages.iter().zip(other_bytes) {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

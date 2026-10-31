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
    services.sources.remove("neutral-fixture").unwrap();
    assert_eq!(
        services.downloads.offline(&chapter.id).unwrap().pages.len(),
        encoded.len()
    );
}

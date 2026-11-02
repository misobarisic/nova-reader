//! Device persistence and live admission through an already-open download store.
use nova_reader::{
    image_limits::{self, Settings},
    services::Services,
};
use reader_domain::{ChapterId, SeriesId};
use reader_downloads::PageResource;
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    process::Command,
};

fn chapter(id: &str) -> ChapterId {
    ChapterId::new(SeriesId::new("local", "limits"), id)
}
fn page() -> PageResource {
    PageResource {
        id: "page".into(),
        url: String::new(),
        headers: BTreeMap::new(),
    }
}
fn png(width: u32) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(width, 4, image::Rgba([7, 9, 11, 255]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}
#[test]
fn device_image_limits_persist_reset_and_update_live_operations() {
    const MODE: &str = "READER_IMAGE_LIMIT_TEST_MODE";
    const DIRECTORY: &str = "READER_IMAGE_LIMIT_TEST_DIRECTORY";
    if let Ok(mode) = std::env::var(MODE) {
        let directory = std::path::PathBuf::from(std::env::var(DIRECTORY).unwrap());
        if mode == "unavailable" {
            let before = reader_engine::current_limits();
            assert!(
                Settings {
                    width: 64,
                    ..Settings::default()
                }
                .save()
                .is_err()
            );
            assert_eq!(reader_engine::current_limits(), before);
            return;
        }
        let services = Services::open(&directory).unwrap();
        match mode.as_str() {
            "write" => {
                assert_eq!(Settings::load(), Settings::default());
                assert_eq!(
                    reader_engine::current_limits(),
                    reader_engine::DecodeLimits::default()
                );
                let settings = Settings {
                    width: 64,
                    encoded_mib: 64,
                    ..Settings::default()
                };
                settings.save().unwrap();
                let revision = reader_engine::limits_revision();
                settings.save().unwrap();
                assert_eq!(
                    reader_engine::limits_revision(),
                    revision,
                    "unchanged saves keep ongoing work valid"
                );
                let invalid = Settings {
                    width: 0,
                    ..settings
                };
                assert!(invalid.save().is_err());
                assert_eq!(Settings::load(), settings);
                assert_eq!(reader_engine::current_limits(), settings.decode_limits());
            }
            "live" => {
                let mut settings = Settings::load();
                assert_eq!(settings.width, 64);
                assert_eq!(
                    reader_engine::current_limits(),
                    settings.decode_limits(),
                    "startup installs persisted policy"
                );
                let bytes = png(80);
                let id = chapter("live");
                services
                    .downloads
                    .enqueue(id.clone(), vec![page()])
                    .unwrap();
                assert!(services.downloads.run(&id, |_| Ok(bytes.clone())).is_err());
                settings.width = 128;
                settings.save().unwrap();
                services.downloads.retry(&id).unwrap();
                services.downloads.run(&id, |_| Ok(bytes.clone())).unwrap();
                assert!(services.downloads.offline(&id).is_ok());
                // A transfer holds one policy even if settings change in its fetch callback.
                let id = chapter("snapshot");
                services
                    .downloads
                    .enqueue(
                        id.clone(),
                        vec![
                            page(),
                            PageResource {
                                id: "second".into(),
                                ..page()
                            },
                        ],
                    )
                    .unwrap();
                services
                    .downloads
                    .run(&id, |_| {
                        Settings {
                            width: 1,
                            ..settings
                        }
                        .save()
                        .unwrap();
                        Ok(bytes.clone())
                    })
                    .unwrap();
                assert_eq!(reader_engine::current_limits().max_width, 1);
                settings.save().unwrap();
                // Decoder cache cannot bypass a newly tightened policy for the same response.
                let response = std::sync::Arc::new(bytes);
                let mut decoder = reader_engine::DisplayDecoder::default();
                decoder
                    .presentation(
                        &response,
                        reader_engine::current_limits(),
                        Default::default(),
                    )
                    .unwrap();
                Settings {
                    width: 1,
                    ..settings
                }
                .save()
                .unwrap();
                assert!(
                    decoder
                        .presentation(
                            &response,
                            reader_engine::current_limits(),
                            Default::default()
                        )
                        .is_err()
                );
                settings.save().unwrap();
                // Bounded local reads and archive metadata now admit >32 MiB when raised.
                let mut large = png(80);
                large.resize(32 * 1024 * 1024 + 1, 0);
                let folder = directory.join("input");
                std::fs::create_dir_all(&folder).unwrap();
                std::fs::write(folder.join("page.png"), &large).unwrap();
                settings.encoded_mib = 32;
                settings.save().unwrap();
                assert!(
                    services
                        .downloads
                        .import_folder(chapter("small-folder"), &folder)
                        .is_err()
                );
                settings.encoded_mib = 64;
                settings.save().unwrap();
                services
                    .downloads
                    .import_folder(chapter("large-folder"), &folder)
                    .unwrap();
                let archive = directory.join("input.cbz");
                let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
                zip.start_file(
                    "page.png",
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
                zip.write_all(&large).unwrap();
                zip.finish().unwrap();
                services
                    .downloads
                    .import_cbz(chapter("large-cbz"), &archive)
                    .unwrap();
                Settings::reset().unwrap();
            }
            "reset" => {
                assert_eq!(Settings::load(), Settings::default());
                assert_eq!(
                    reader_engine::current_limits(),
                    reader_engine::DecodeLimits::default()
                );
                nova_storage::try_set_str("reader:image-limits", r#"{"width":0}"#).unwrap();
                image_limits::install();
                assert_eq!(
                    reader_engine::current_limits(),
                    reader_engine::DecodeLimits::default()
                );
                assert_eq!(
                    nova_storage::try_get_str("reader:image-limits")
                        .unwrap()
                        .unwrap(),
                    r#"{"width":0}"#
                );
            }
            _ => panic!("unknown test phase"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for mode in ["unavailable", "write", "live", "reset"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "device_image_limits_persist_reset_and_update_live_operations",
                "--nocapture",
            ])
            .env(MODE, mode)
            .env(DIRECTORY, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

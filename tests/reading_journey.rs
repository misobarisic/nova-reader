//! A real process boundary tests killed-writer recovery and source-free offline
//! opening against exactly the same service paths used by the UI.
use nova_reader::services::Services;
use reader_domain::{ReadingMode, ReadingProgress, ResumeAnchor};
use reader_downloads::PageResource;
use reader_sources::CancelToken;
use serde_json::json;
use std::{
    io::{Cursor, Write},
    path::Path,
    process::Command,
};

fn package(root: &Path) -> Vec<u8> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let mut names = vec![
        "manifest.json".to_owned(),
        manifest["entry"].as_str().unwrap().to_owned(),
    ];
    names.extend(
        manifest["assets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned()),
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

#[test]
fn restart_download_and_source_free_offline_journey() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("fixture.zip");
    std::fs::write(
        &zip,
        package(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("examples/addons/neutral-fixture")
                .as_path(),
        ),
    )
    .unwrap();
    for phase in ["interrupt", "recover", "offline"] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "journey_child", "--nocapture"])
            .env("READER_JOURNEY_PHASE", phase)
            .env("READER_JOURNEY_PATH", dir.path().join("data"))
            .env("READER_JOURNEY_ZIP", &zip)
            .status()
            .unwrap();
        assert!(status.success(), "journey phase {phase} failed");
    }
}

#[test]
#[ignore = "spawned with a separate database/process by the journey test"]
fn journey_child() {
    let Some(phase) = std::env::var_os("READER_JOURNEY_PHASE") else {
        return;
    };
    let services =
        Services::open(Path::new(&std::env::var_os("READER_JOURNEY_PATH").unwrap())).unwrap();
    if phase == "interrupt" {
        services
            .install(&std::fs::read(std::env::var_os("READER_JOURNEY_ZIP").unwrap()).unwrap())
            .unwrap();
        let source = services.sources.source("neutral-fixture").unwrap();
        let listing = source.browse(&json!({"page":1})).unwrap();
        let series = services.series("neutral-fixture", listing.items[0].clone());
        let (series, chapters) = services.details(&series, &CancelToken::default()).unwrap();
        services.add_library(&series.id).unwrap();
        let chapter = chapters
            .into_iter()
            .find(|c| c.id.local_id.ends_with("mixed"))
            .unwrap();
        services
            .domain
            .update(|state| {
                state.set_progress(ReadingProgress {
                    chapter: chapter.id.clone(),
                    anchor: ResumeAnchor::new("1", 1, 0.123456).unwrap(),
                    mode: ReadingMode::Webtoon,
                    last_read_at: 42,
                    completed: false,
                });
                Ok(())
            })
            .unwrap();
        let (_, pages) = services
            .pages(&chapter.id, &CancelToken::default())
            .unwrap();
        services
            .downloads
            .enqueue(
                chapter.id.clone(),
                pages
                    .into_iter()
                    .map(|p| PageResource {
                        id: p.id,
                        url: p.url,
                        headers: p.headers,
                    })
                    .collect(),
            )
            .unwrap();
        let mut calls = 0;
        let _ = services.downloads.run(&chapter.id, |page| {
            calls += 1;
            if calls == 2 {
                std::process::exit(0);
            }
            source
                .fetch_page(&page.url, &page.headers, &CancelToken::default())
                .map_err(|e| e.to_string())
        });
        panic!("download did not reach interruption");
    }
    let state = services.domain.snapshot().unwrap();
    let progress = &state.progress[0];
    assert_eq!(progress.anchor.offset_millionths, 123456);
    assert_eq!(progress.anchor.page_id, "1");
    if phase == "recover" {
        let job = services.downloads.job(&progress.chapter).unwrap();
        assert_eq!(job.completed_pages(), 1);
        assert_ne!(job.status, reader_downloads::JobStatus::Complete);
        services
            .download(&progress.chapter, &CancelToken::default())
            .unwrap();
        services.sources.remove("neutral-fixture").unwrap();
    } else {
        assert!(services.sources.source("neutral-fixture").is_err());
        let offline = services.downloads.offline(&progress.chapter).unwrap();
        assert_eq!(offline.pages.len(), 6);
        for page in offline.pages {
            let bytes = std::fs::read(page).unwrap();
            reader_engine::decode_display_image(
                &bytes,
                reader_engine::DecodeLimits::default(),
                512,
                4096,
            )
            .unwrap();
        }
        assert!(!progress.completed);
        assert_eq!(state.library.len(), 1);
    }
}

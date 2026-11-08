use super::*;
use crate::services::Services;
use reader_domain::{
    Bookmark, Category, Chapter, ChapterId, HistoryEntry, LibraryEntry, ReadingMode,
    ReadingProgress, ResumeAnchor, Series, SeriesId,
};
use serde_json::json;
use std::io::{Cursor, Write};

/// Storage and the sync owner are per-process, so each scenario re-executes
/// itself in a child that owns a fresh directory.
pub(super) fn isolated(name: &str, body: impl FnOnce(&Path)) {
    const CHILD: &str = "READER_BACKUP_UNIT_CHILD";
    if let Some(directory) = std::env::var_os(CHILD) {
        body(Path::new(&directory));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{name} failed\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A source package with one secret and two public settings.
pub(super) fn package(id: &str, version: &str) -> Vec<u8> {
    let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":id,"name":"Fixture","version":version,"language":"en","entry":"main.js","permissions":{"domains":[]},"configuration":[{"key":"token","name":"Token","kind":"secret","default":""},{"key":"label","name":"Label","kind":"text","default":"public"},{"key":"other","name":"Other","kind":"text","default":"other-default"}],"capabilities":["browse","search","filters","details","chapters","pages"]});
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("main.js", b"globalThis.source = {};".to_vec()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

pub(super) fn series(source: &str, local: &str) -> Series {
    Series {
        id: SeriesId::new(source, local),
        title: format!("Title {local}"),
        publication_status: Default::default(),
        description: String::new(),
        authors: vec![],
        genres: vec![],
        cover_url: None,
        canonical_url: None,
    }
}
pub(super) fn chapter(series: &Series, local: &str) -> Chapter {
    Chapter {
        id: ChapterId::new(series.id.clone(), local),
        title: format!("Chapter {local}"),
        label: format!("Chapter {local}"),
        number: Some(local.to_owned()),
        volume: None,
        source_order: 0,
        language: None,
        scanlator: None,
        uploaded_at: None,
    }
}
pub(super) fn progress(chapter: &Chapter, at: i64, completed: bool) -> ReadingProgress {
    ReadingProgress {
        chapter: chapter.id.clone(),
        anchor: ResumeAnchor::new("page-2", 2, 0.25).unwrap(),
        mode: ReadingMode::RightToLeft,
        last_read_at: at,
        completed,
    }
}

/// A library entry in category "Favorites", a progress record, a history row
/// and a bookmark for one chapter of one title.
pub(super) fn fill(services: &Services) -> (Series, Chapter) {
    let title = series("fixture", "one");
    let first = chapter(&title, "1");
    let (title2, first2) = (title.clone(), first.clone());
    services
        .domain
        .update_manual_progress(|state| {
            state.series.push(title2.clone());
            state.chapters.push(first2.clone());
            state.categories.push(Category {
                id: "category-local".into(),
                name: "Favorites".into(),
            });
            state.library.push(LibraryEntry {
                reading_status: Default::default(),
                series: title2.id.clone(),
                categories: ["category-local".to_owned()].into(),
                preferences: None,
                bookmarked: true,
                added_at: 5,
            });
            state.progress.push(progress(&first2, 100, true));
            state.history.push(HistoryEntry {
                chapter: first2.id.clone(),
                read_at: 100,
            });
            state.bookmarks.push(Bookmark {
                chapter: first2.id.clone(),
                anchor: ResumeAnchor::new("page-3", 3, 0.5).unwrap(),
                note: "note".into(),
            });
            Ok(())
        })
        .unwrap();
    (title, first)
}

#[test]
fn an_export_holds_every_component_and_no_sync_identity() {
    isolated(
        "backup::tests::an_export_holds_every_component_and_no_sync_identity",
        |directory| {
            let services = Services::open(&directory.join("data")).unwrap();
            let (title, first) = fill(&services);
            let second = crate::profiles::create("Second").unwrap();
            services.install(&package("fixture", "1.0.0")).unwrap();
            services
                .sources
                .set_settings("fixture", json!({"token":"hunter2","label":"mine"}))
                .unwrap();
            crate::reading_preferences::set_show_adult(true).unwrap();
            crate::reading_preferences::set_chapter_descending(&title.id, false).unwrap();
            let mut limits = crate::image_limits::Settings::default();
            limits.encoded_mib = 48;
            limits.save().unwrap();

            let profiles = crate::profiles::list().unwrap();
            let selection = Selection::everything(&profiles, Files::default());
            let path = directory.join("everything.nova-reader-backup");
            let exported = export(&services, &selection, &path).unwrap();
            assert_eq!(exported.profiles, 2);
            assert!(archive::is_zip(&path).unwrap());

            let decoded = archive::decode_file(&path).unwrap();
            assert_eq!(decoded.manifest.format, archive::FORMAT);
            assert!(decoded.unsupported.is_empty());
            let default = decoded
                .manifest
                .profiles
                .iter()
                .find(|p| p.source_profile_id == "reader")
                .unwrap();
            let get = |profile: &str, name: &str| {
                &decoded.payloads[&(profile.to_owned(), name.to_owned())]
            };
            let id = &default.archive_profile_id;
            let key = |kind, id: &dyn std::fmt::Display| components::key(kind, &id.to_string());
            let chapter_json = serde_json::to_string(&first.id).unwrap();
            assert!(
                get(id, "library")
                    .records
                    .contains_key(&key("entry", &serde_json::to_string(&title.id).unwrap()))
            );
            let entry = &get(id, "library").records
                [&key("entry", &serde_json::to_string(&title.id).unwrap())];
            assert_eq!(entry["categories"], json!(["favorites"]));
            assert!(
                get(id, "categories")
                    .records
                    .contains_key(&key("category", &"favorites"))
            );
            assert_eq!(
                get(id, "progress").records[&key("progress", &chapter_json)]["last_read_at"],
                100
            );
            assert!(
                get(id, "history")
                    .records
                    .contains_key(&key("history", &chapter_json))
            );
            assert_eq!(get(id, "bookmarks").records.len(), 3);
            let settings = get(id, "settings");
            assert_eq!(
                settings.records[&key("kv", &"reader:show-adult")],
                json!(true)
            );
            assert!(
                get(id, "title_settings")
                    .records
                    .keys()
                    .any(|k| k.contains("reader:chapter-order:"))
            );
            // Addon credentials travel in plaintext, apart from the public settings.
            let secrets = get(id, "addon_secrets");
            assert_eq!(
                secrets.records[&key("secret", &"fixture")],
                json!({"token":"hunter2"})
            );
            let addons = get(id, "addons");
            assert_eq!(
                addons.records[&key("addon", &"fixture")]["settings"],
                json!({"label":"mine","other":"other-default"})
            );
            assert!(!serde_json::to_string(addons).unwrap().contains("hunter2"));
            assert!(
                decoded
                    .manifest
                    .profiles
                    .iter()
                    .any(|p| p.source_profile_id == second.id)
            );

            // Device state: the package, device settings, and never sync state.
            let device = get(archive::DEVICE, "device_settings");
            assert_eq!(
                device.records[&key("kv", &"reader:image-limits")]["encoded_mib"],
                48
            );
            assert!(device.records.keys().all(|k| {
                let (kind, id) = components::split(k).unwrap();
                !id.starts_with("sync:") && (kind == "kv" || kind == "device")
            }));
            assert!(
                !device
                    .records
                    .contains_key(&key("kv", &"reader:installed-sources:v1"))
            );
            let packages = get(archive::DEVICE, "packages");
            assert_eq!(packages.records.len(), 1);
            assert_eq!(decoded.packages.len(), 1);
            let everything =
                serde_json::to_string(&decoded.payloads.values().collect::<Vec<_>>()).unwrap();
            for forbidden in ["sync:identity", "sync:peers", "active_profile"] {
                assert!(!everything.contains(forbidden), "{forbidden}");
            }
            // A failed export leaves no staging file next to the destination.
            let bad = directory.join("missing").join("x.nova-reader-backup");
            assert!(export(&services, &selection, &bad).is_err());
            let leftovers = std::fs::read_dir(directory)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(".nova-reader-backup-")
                })
                .count();
            assert_eq!(leftovers, 0);
        },
    );
}

#[test]
fn a_corrupt_or_foreign_file_is_not_an_archive() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("x");
    std::fs::write(&path, b"PK\x03\x04 not really").unwrap();
    assert!(archive::is_zip(&path).unwrap());
    assert!(archive::decode_file(&path).is_err());
    std::fs::write(&path, br#"{"schema_version":1}"#).unwrap();
    assert!(!archive::is_zip(&path).unwrap());
}

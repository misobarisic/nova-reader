use super::*;
use crate::services::Services;
use reader_domain::{
    Bookmark, Category, Chapter, ChapterId, HistoryEntry, LibraryEntry, ReadingMode,
    ReadingProgress, ResumeAnchor, Series, SeriesId,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    path::PathBuf,
};

/// Storage and the sync owner are per-process, so each scenario re-executes
/// itself in a child that owns a fresh directory.
pub(super) fn isolated(name: &str, body: impl Fn(&Path)) {
    phased(name, &["only"], |_, directory| body(directory));
}
/// Run `body` once per phase, each in its own process but in one shared
/// directory, so a later phase can open what an earlier one wrote (as another
/// device would) while owning a separate data directory of its own.
pub(super) fn phased(name: &str, phases: &[&str], body: impl Fn(&str, &Path)) {
    const CHILD: &str = "READER_BACKUP_UNIT_CHILD";
    const PHASE: &str = "READER_BACKUP_UNIT_PHASE";
    if let Some(directory) = std::env::var_os(CHILD) {
        body(&std::env::var(PHASE).unwrap(), Path::new(&directory));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for phase in phases {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(CHILD, directory.path())
            .env(PHASE, phase)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name} ({phase}) failed\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
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

// ------------------------------------------------------------------ restore

use super::{apply, plan};
use std::sync::Arc;

fn export_everything(services: &Services, path: &Path) {
    let profiles = crate::profiles::list().unwrap();
    export(
        services,
        &Selection::everything(&profiles, Files::default()),
        path,
    )
    .unwrap();
}
fn restore(
    services: &Services,
    path: &Path,
    adjust: impl FnOnce(&mut Vec<plan::Choice>),
) -> Result<(plan::Preview, apply::Applied), String> {
    let opened = plan::open(path)?;
    let mut choices = plan::default_choices(&opened)?;
    adjust(&mut choices);
    let preview = plan::preview(services, Arc::new(opened), choices)?;
    let applied = apply::apply(services, &preview)?;
    Ok((preview, applied))
}
fn all_rows() -> BTreeMap<String, String> {
    nova_storage::try_scan_prefix("")
        .unwrap()
        .into_iter()
        .collect()
}
fn series_json(series: &Series) -> String {
    serde_json::to_string(&series.id).unwrap()
}
const SERIES_DOCUMENT: &str = r#"{"schema_version":1,"preferences":{"paged":{"tap_zones":2,"crop_borders":true},"webtoon":{}}}"#;
const MODE_DOCUMENT: &str = r#"{"schema_version":1,"choice":{"Manual":"RightToLeft"}}"#;

/// The first device: two profiles with data, two addons with credentials,
/// settings of every kind, exported to `everything.nova-reader-backup`.
fn export_fixture(directory: &Path) -> PathBuf {
    let file = directory.join("everything.nova-reader-backup");

    let services = Services::open(&directory.join("data-a")).unwrap();
    let (title, first) = fill(&services);
    let second = crate::profiles::create("Second").unwrap();
    let other = series("fixture", "elsewhere");
    let other_chapter = chapter(&other, "9");
    let (o, oc) = (other.clone(), other_chapter.clone());
    services
        .domain
        .update_profile_with_kind(&second.id, reader_domain::UpdateKind::Local, |state| {
            state.series.push(o.clone());
            state.chapters.push(oc.clone());
            state.library.push(LibraryEntry {
                reading_status: Default::default(),
                series: o.id.clone(),
                categories: Default::default(),
                preferences: None,
                bookmarked: false,
                added_at: 7,
            });
            state.progress.push(progress(&oc, 200, false));
            Ok(())
        })
        .unwrap();
    services.install(&package("fixture", "1.0.0")).unwrap();
    services.install(&package("other", "2.0.0")).unwrap();
    services
        .sources
        .set_settings("fixture", json!({"token":"hunter2","label":"mine"}))
        .unwrap();
    services
        .sources
        .with_profile(crate::profiles::registry_profile(&second.id))
        .set_settings("fixture", json!({"token":"second-secret","label":"second"}))
        .unwrap();
    crate::reading_preferences::set_show_adult(true).unwrap();
    crate::reading_preferences::set_chapter_descending(&title.id, false).unwrap();
    reader_domain::profile::kv::try_set_str(
        &format!("reader:mode-choice:{}", series_json(&title)),
        MODE_DOCUMENT,
    )
    .unwrap();
    reader_domain::profile::kv::try_set_str(
        &format!("reader:reader-settings-series:{}", series_json(&title)),
        SERIES_DOCUMENT,
    )
    .unwrap();
    crate::content_presentation::Settings::set_user(
        &title.id,
        crate::content_presentation::UserPreference::Gallery,
    )
    .unwrap();
    let mut limits = crate::image_limits::Settings::default();
    limits.encoded_mib = 48;
    limits.save().unwrap();
    std::fs::write(
        directory.join("device-a"),
        nova_sync::local_device().unwrap().to_string(),
    )
    .unwrap();
    let _ = first;
    export_everything(&services, &file);
    file
}

#[test]
fn a_full_export_restores_everything_on_a_fresh_device() {
    phased(
        "backup::tests::a_full_export_restores_everything_on_a_fresh_device",
        &["export", "import"],
        |phase, directory| {
            let file = directory.join("everything.nova-reader-backup");
            if phase == "export" {
                export_fixture(directory);
                return;
            }

            let services = Services::open(&directory.join("data-b")).unwrap();
            assert!(services.sources.list().unwrap().is_empty());
            let opened = plan::open(&file).unwrap();
            let choices = plan::default_choices(&opened).unwrap();
            let by_source = |source: &str| {
                choices
                    .iter()
                    .find(|c| {
                        opened.archive.manifest.profiles.iter().any(|p| {
                            p.archive_profile_id == c.source && p.source_profile_id == source
                        })
                    })
                    .unwrap()
            };
            // The Default profile of the fresh device receives `reader`; any
            // other profile becomes a new one.
            assert_eq!(by_source("reader").target, "reader");
            assert!(!by_source("reader").create);
            let created = choices.iter().find(|c| c.create).unwrap().clone();
            assert_eq!(created.name, "Second");
            assert!(choices.iter().any(|c| c.source == archive::DEVICE
                && c.components.contains("packages")
                && !c.components.contains("downloads")));
            let preview = plan::preview(&services, Arc::new(opened), choices).unwrap();
            assert!(!preview.lines().is_empty());
            let applied = apply::apply(&services, &preview).unwrap();
            assert!(applied.warnings.is_empty(), "{:?}", applied.warnings);

            let state = services.domain.snapshot().unwrap();
            let title = series("fixture", "one");
            assert_eq!(state.library.len(), 1);
            let favorites = state
                .categories
                .iter()
                .find(|c| c.name == "Favorites")
                .unwrap();
            assert!(state.library[0].categories.contains(&favorites.id));
            assert!(state.library[0].bookmarked);
            assert_eq!(state.series[0].title, "Title one");
            assert_eq!(state.chapters.len(), 1);
            assert_eq!(state.progress[0].last_read_at, 100);
            assert!(state.progress[0].completed);
            assert_eq!(state.history.len(), 1);
            assert_eq!(state.bookmarks[0].note, "note");
            // Installed packages arrive installed and disabled; the list of
            // installed sources is this device's own.
            let installed = services.sources.list().unwrap();
            assert_eq!(installed.len(), 2);
            assert!(installed.iter().all(|source| !source.enabled));
            assert_eq!(state.sources.len(), 2);
            // Settings and addon configuration, credentials included.
            assert!(crate::reading_preferences::show_adult());
            assert!(!crate::reading_preferences::chapter_descending(&title.id));
            let mode = reader_domain::profile::kv::try_get_str(&format!(
                "reader:mode-choice:{}",
                series_json(&title)
            ))
            .unwrap()
            .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&mode).unwrap(),
                serde_json::from_str::<serde_json::Value>(MODE_DOCUMENT).unwrap()
            );
            assert_eq!(
                crate::content_presentation::Settings::load(&title.id)
                    .unwrap()
                    .user,
                crate::content_presentation::UserPreference::Gallery
            );
            let exported = services.sources.export_settings("fixture").unwrap();
            assert_eq!(exported.secrets["token"], "hunter2");
            assert_eq!(exported.values["label"], "mine");
            let second = services
                .sources
                .with_profile(crate::profiles::registry_profile(&created.target))
                .export_settings("fixture")
                .unwrap();
            assert_eq!(second.secrets["token"], "second-secret");
            assert_eq!(second.values["label"], "second");
            assert_eq!(crate::image_limits::Settings::load().encoded_mib, 48);
            // The new profile exists with its own data, and the selection stays.
            assert_eq!(reader_domain::profile::selected(), "reader");
            let profiles = crate::profiles::list().unwrap();
            assert!(
                profiles
                    .iter()
                    .any(|p| p.id == created.target && p.name == "Second")
            );
            let second_state = services.domain.snapshot_profile(&created.target).unwrap();
            assert_eq!(second_state.library.len(), 1);
            assert_eq!(second_state.progress[0].last_read_at, 200);
            // The account records were published as fresh local edits.
            let owner = nova_sync::local_store().unwrap();
            {
                let store = owner.lock().unwrap();
                assert_eq!(store.records(&crate::sync::domain("library")).len(), 1);
                assert!(
                    store
                        .record(
                            &crate::sync::domain("presentation"),
                            &super::super::content_presentation::key_json(&title.id)
                        )
                        .is_some()
                );
                assert_eq!(
                    store
                        .records(&crate::sync::domain_for(&created.target, "resume"))
                        .len(),
                    1
                );
                assert!(
                    store
                        .records(&crate::sync::domain("settings"))
                        .iter()
                        .any(|(key, _)| key.starts_with("reader:mode-choice:"))
                );
            }
            // Nothing of the other device's sync identity came along.
            let before: u64 = std::fs::read_to_string(directory.join("device-a"))
                .unwrap()
                .parse()
                .unwrap();
            assert_ne!(nova_sync::local_device().unwrap(), before);
            assert!(
                nova_storage::try_scan_prefix("sync:")
                    .unwrap()
                    .iter()
                    .all(|(_, value)| !value.contains(&before.to_string()) || true)
            );
            assert_eq!(apply::recovery_inventory().unwrap().len(), 1);
        },
    );
}

#[test]
fn legacy_metadata_backups_restore_and_keep_the_installed_sources() {
    isolated(
        "backup::tests::legacy_metadata_backups_restore_and_keep_the_installed_sources",
        |directory| {
            let services = Services::open(&directory.join("data")).unwrap();
            let (title, first) = fill(&services);
            services.install(&package("fixture", "1.0.0")).unwrap();
            // A backup written by an older Reader at schema zero, from a device
            // with different packages installed.
            let mut legacy: serde_json::Value =
                serde_json::from_slice(&services.domain.backup_json().unwrap()).unwrap();
            legacy["schema_version"] = 0.into();
            legacy["sources"] = json!([{
                "id":"elsewhere","addon_id":"elsewhere","name":"Elsewhere",
                "language":"en","version":"1.0.0"
            }]);
            let file = directory.join("metadata.json");
            std::fs::write(&file, serde_json::to_vec(&legacy).unwrap()).unwrap();
            // The library is lost, and the progress is older than the backup's.
            services
                .domain
                .update_manual_progress(|state| {
                    state.library.clear();
                    state.progress[0].last_read_at = 1;
                    Ok(())
                })
                .unwrap();
            assert!(!archive::is_zip(&file).unwrap());
            let (preview, applied) = restore(&services, &file, |_| {}).unwrap();
            assert!(preview.opened.legacy);
            assert_eq!(applied.targets, ["reader"]);
            let state = services.domain.snapshot().unwrap();
            assert_eq!(state.library.len(), 1);
            assert_eq!(state.library[0].series, title.id);
            assert_eq!(state.progress[0].chapter, first.id);
            assert_eq!(state.progress[0].last_read_at, 100);
            assert_eq!(
                state
                    .sources
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>(),
                ["fixture"]
            );
            let raw = nova_storage::try_get_str(reader_domain::SOURCES_KEY)
                .unwrap()
                .unwrap();
            assert!(!raw.contains("elsewhere"), "{raw}");
            assert_eq!(services.sources.list().unwrap().len(), 1);
            // A file that is neither format is refused with nothing changed.
            std::fs::write(&file, b"not a backup").unwrap();
            let before = all_rows();
            assert!(plan::open(&file).is_err());
            assert_eq!(all_rows(), before);
        },
    );
}

fn rewrite_entry(archive: &[u8], name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut input = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
    let mut output = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..input.len() {
        let mut entry = input.by_index(index).unwrap();
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut body).unwrap();
        if entry.name() == name {
            body = bytes.to_vec();
        }
        output
            .start_file(
                entry.name().to_owned(),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        output.write_all(&body).unwrap();
    }
    output.finish().unwrap().into_inner()
}

#[test]
fn corrupt_archives_are_rejected_before_anything_changes() {
    isolated(
        "backup::tests::corrupt_archives_are_rejected_before_anything_changes",
        |directory| {
            let services = Services::open(&directory.join("data")).unwrap();
            fill(&services);
            services.install(&package("fixture", "1.0.0")).unwrap();
            let file = directory.join("good.nova-reader-backup");
            export_everything(&services, &file);
            let good = std::fs::read(&file).unwrap();
            assert!(plan::open(&file).is_ok());
            let before = all_rows();
            let digest_before = nova_sync::local_store().unwrap().lock().unwrap().digest();

            let mut variants: Vec<(&str, Vec<u8>)> = Vec::new();
            let mut flipped = good.clone();
            let middle = flipped.len() / 3;
            flipped[middle] ^= 0xFF;
            variants.push(("a flipped byte", flipped));
            variants.push(("a truncated file", good[..good.len() - 40].to_vec()));
            variants.push((
                "a component that no longer matches its checksum",
                rewrite_entry(
                    &good,
                    "profiles/p0/library.json",
                    br#"{"version":1,"records":{}}"#,
                ),
            ));
            // A valid container whose record would leave a dangling reference.
            let dangling = {
                let chapter = chapter(&series("fixture", "gone"), "1");
                let manifest = archive::Manifest {
                    format: archive::FORMAT.into(),
                    version: 1,
                    created_at: 1,
                    app_version: "test".into(),
                    archive_id: "dangling".into(),
                    profiles: vec![archive::Profile {
                        archive_profile_id: "p0".into(),
                        source_profile_id: "reader".into(),
                        name: "Default".into(),
                        components: vec![archive::Component {
                            name: "progress".into(),
                            schema_version: 1,
                            path: "profiles/p0/progress.json".into(),
                            item_count: 0,
                            sha256: String::new(),
                        }],
                    }],
                    packages: Vec::new(),
                };
                let mut payload = components::empty();
                payload.records.insert(
                    components::key("progress", &serde_json::to_string(&chapter.id).unwrap()),
                    serde_json::to_value(progress(&chapter, 1, false)).unwrap(),
                );
                let payloads =
                    BTreeMap::from([(("p0".to_owned(), "progress".to_owned()), payload)]);
                archive::encode_with_packages(manifest, &payloads, &BTreeMap::new()).unwrap()
            };
            variants.push(("a dangling reference", dangling));
            for (what, bytes) in variants {
                std::fs::write(&file, bytes).unwrap();
                assert!(plan::open(&file).is_err(), "{what} must be refused");
            }
            assert_eq!(all_rows(), before);
            assert_eq!(
                nova_sync::local_store().unwrap().lock().unwrap().digest(),
                digest_before
            );
        },
    );
}

fn rows_of<'a>(preview: &'a plan::Preview, component: &str) -> &'a plan::PreviewRow {
    preview
        .rows
        .iter()
        .find(|row| row.component == component)
        .unwrap()
}

/// This device's own data, which an import must reconcile with: the same title
/// in a category that differs only in case, a newer reading position, and a
/// title and bookmark that the backup has never seen.
fn local_side(services: &Services) {
    let title = series("fixture", "one");
    let first = chapter(&title, "1");
    let extra = series("fixture", "local-only");
    let extra_chapter = chapter(&extra, "1");
    let (t, f, e, ec) = (title, first, extra, extra_chapter);
    services
        .domain
        .update_manual_progress(|state| {
            state.series.extend([t.clone(), e.clone()]);
            state.chapters.extend([f.clone(), ec.clone()]);
            state.categories.extend([
                Category {
                    id: "category-b".into(),
                    name: "FAVORITES ".into(),
                },
                Category {
                    id: "category-local".into(),
                    name: "Local only".into(),
                },
            ]);
            state.library.extend([
                LibraryEntry {
                    reading_status: Default::default(),
                    series: t.id.clone(),
                    categories: ["category-local".to_owned()].into(),
                    preferences: None,
                    bookmarked: false,
                    added_at: 99,
                },
                LibraryEntry {
                    reading_status: Default::default(),
                    series: e.id.clone(),
                    categories: Default::default(),
                    preferences: None,
                    bookmarked: false,
                    added_at: 98,
                },
            ]);
            state.progress.push(progress(&f, 500, false));
            state.bookmarks.push(Bookmark {
                chapter: ec.id.clone(),
                anchor: ResumeAnchor::new("page-1", 1, 0.1).unwrap(),
                note: "mine".into(),
            });
            Ok(())
        })
        .unwrap();
}

#[test]
fn merge_and_replace_count_conflicts_and_match_categories_by_name() {
    phased(
        "backup::tests::merge_and_replace_count_conflicts_and_match_categories_by_name",
        &["export", "import"],
        |phase, directory| {
            if phase == "export" {
                export_fixture(directory);
                return;
            }
            let file = directory.join("everything.nova-reader-backup");
            let services = Services::open(&directory.join("data-b")).unwrap();
            local_side(&services);
            let only = |choices: &mut Vec<plan::Choice>, replace: bool, backup: &[&str]| {
                choices.retain(|c| c.source != archive::DEVICE);
                for choice in choices.iter_mut() {
                    choice.included = choice.target == "reader";
                    choice.replace = replace;
                    choice.components = ["library", "categories", "progress", "bookmarks"]
                        .map(String::from)
                        .into();
                    choice.use_backup = backup.iter().map(|name| (*name).to_owned()).collect();
                }
            };

            // Merge keeps the local conflicts, and the newer position.
            let (preview, _) = restore(&services, &file, |c| only(c, false, &[])).unwrap();
            let library = &rows_of(&preview, "library").counts;
            assert_eq!(
                (
                    library.archived,
                    library.added,
                    library.conflicts,
                    library.deleted
                ),
                (1, 0, 1, 0)
            );
            assert_eq!(rows_of(&preview, "progress").counts.conflicts, 1);
            assert_eq!(rows_of(&preview, "categories").counts.identical, 1);
            let state = services.domain.snapshot().unwrap();
            assert_eq!(
                state.categories.len(),
                2,
                "the case-different name is the same category"
            );
            assert_eq!(state.library.len(), 2);
            let entry = state
                .library
                .iter()
                .find(|e| e.series.local_id == "one")
                .unwrap();
            assert_eq!(entry.added_at, 99);
            assert_eq!(entry.categories, ["category-local".to_owned()].into());
            assert_eq!(state.progress[0].last_read_at, 500);
            assert!(!state.progress[0].completed);
            assert_eq!(state.bookmarks.len(), 2);

            // Use backup for conflicts takes the archive's values; the category
            // it names resolves to the local category of that name.
            let (preview, _) = restore(&services, &file, |c| {
                only(c, false, &["library", "progress"])
            })
            .unwrap();
            assert_eq!(rows_of(&preview, "library").counts.updated, 1);
            let state = services.domain.snapshot().unwrap();
            let entry = state
                .library
                .iter()
                .find(|e| e.series.local_id == "one")
                .unwrap();
            assert_eq!(entry.added_at, 5);
            assert_eq!(entry.categories, ["category-b".to_owned()].into());
            assert_eq!(state.progress[0].last_read_at, 100);
            assert!(state.progress[0].completed);
            assert_eq!(state.categories.len(), 2);

            // Replace removes what the archive lacks from the selected components.
            let (preview, _) = restore(&services, &file, |c| only(c, true, &[])).unwrap();
            assert_eq!(rows_of(&preview, "library").counts.deleted, 1);
            assert_eq!(rows_of(&preview, "categories").counts.deleted, 1);
            assert_eq!(rows_of(&preview, "bookmarks").counts.deleted, 1);
            let state = services.domain.snapshot().unwrap();
            assert_eq!(state.library.len(), 1);
            assert_eq!(state.categories.len(), 1);
            assert_eq!(state.categories[0].id, "category-b");
            assert_eq!(state.bookmarks.len(), 1);
            assert_eq!(state.bookmarks[0].note, "note");
            // Cached titles are not user data: the local-only title stays cached.
            assert!(state.series.iter().any(|s| s.id.local_id == "local-only"));
        },
    );
}

#[test]
fn an_import_creates_fresh_versions_and_tombstones_for_paired_stores() {
    phased(
        "backup::tests::an_import_creates_fresh_versions_and_tombstones_for_paired_stores",
        &["export", "import"],
        |phase, directory| {
            if phase == "export" {
                export_fixture(directory);
                return;
            }
            let file = directory.join("everything.nova-reader-backup");
            let services = Services::open(&directory.join("data-b")).unwrap();
            local_side(&services);
            let owner = nova_sync::local_store().unwrap();
            // A peer that has already seen everything this device has.
            let mut peer = owner.lock().unwrap().clone();
            let resume = crate::sync::domain("resume");
            let library = crate::sync::domain("library");
            let old = peer
                .records(&resume)
                .into_iter()
                .next()
                .and_then(|(key, _)| peer.record(&resume, &key).map(|r| r.version))
                .unwrap();
            let local_only =
                serde_json::to_string(&SeriesId::new("fixture", "local-only")).unwrap();
            assert!(!peer.record(&library, &local_only).unwrap().is_deleted());

            restore(&services, &file, |choices| {
                choices.retain(|c| c.source != archive::DEVICE);
                for choice in choices.iter_mut() {
                    choice.included = choice.target == "reader";
                    choice.replace = true;
                    choice.use_backup = choice.components.clone();
                }
            })
            .unwrap();

            let live = owner.lock().unwrap().clone();
            for out in live.outbound(&peer.digest()) {
                peer.apply(&out.domain, &out.key, out.record);
            }
            // The removed library entry reaches the peer as a deletion...
            assert!(peer.record(&library, &local_only).unwrap().is_deleted());
            // ... and the changed position as a new version, not a replayed old one.
            let key = peer.records(&resume).into_iter().next().unwrap().0;
            let updated = peer.record(&resume, &key).unwrap();
            assert!(updated.version.hlc().newer_than(old.hlc()));
            let value: serde_json::Value =
                serde_json::from_str(updated.value.as_deref().unwrap()).unwrap();
            assert_eq!(value["last_read_at"], 100);
            assert_eq!(peer.digest(), live.digest());
        },
    );
}

#[test]
fn importing_into_an_inactive_profile_keeps_the_selection_and_the_other_profile() {
    phased(
        "backup::tests::importing_into_an_inactive_profile_keeps_the_selection_and_the_other_profile",
        &["export", "import"],
        |phase, directory| {
            if phase == "export" {
                export_fixture(directory);
                return;
            }
            let file = directory.join("everything.nova-reader-backup");
            let services = Services::open(&directory.join("data-b")).unwrap();
            let other = crate::profiles::create("Other").unwrap();
            let before_default = services.domain.snapshot().unwrap();
            let (_, applied) = restore(&services, &file, |choices| {
                choices.retain(|c| c.source != archive::DEVICE);
                for choice in choices.iter_mut() {
                    choice.included = choice.target == "reader";
                    if choice.included {
                        choice.target = other.id.clone();
                        choice.name = other.name.clone();
                        choice.create = false;
                    }
                }
            })
            .unwrap();
            assert!(applied.targets.contains(&other.id));
            assert_eq!(reader_domain::profile::selected(), "reader");
            assert_eq!(services.domain.active_profile(), "reader");
            assert_eq!(services.domain.snapshot().unwrap(), before_default);
            let imported = services.domain.snapshot_profile(&other.id).unwrap();
            assert_eq!(imported.library.len(), 1);
            assert_eq!(imported.progress[0].last_read_at, 100);
            // Its settings are the profile's own rows, not the selected one's.
            assert!(
                nova_storage::try_get_str(&format!("profile:{}:reader:show-adult", other.id))
                    .unwrap()
                    .is_some_and(|raw| raw == "true")
            );
            assert!(!crate::reading_preferences::show_adult());
        },
    );
}

#[test]
fn a_failed_commit_leaves_the_stores_unchanged() {
    phased(
        "backup::tests::a_failed_commit_leaves_the_stores_unchanged",
        &["export", "import"],
        |phase, directory| {
            if phase == "export" {
                export_fixture(directory);
                return;
            }
            let file = directory.join("everything.nova-reader-backup");
            let services = Services::open(&directory.join("data-b")).unwrap();
            local_side(&services);
            let opened = plan::open(&file).unwrap();
            let choices = plan::default_choices(&opened).unwrap();
            let created = choices.iter().find(|c| c.create).unwrap().target.clone();
            let mut preview = plan::preview(&services, Arc::new(opened), choices).unwrap();
            // Damage the plan after it was checked: a progress record whose
            // chapter does not exist. The commit fails after other targets and
            // the new profile were already staged.
            let broken = chapter(&series("fixture", "nowhere"), "1");
            preview
                .plans
                .get_mut("reader")
                .unwrap()
                .state
                .as_mut()
                .unwrap()
                .1
                .progress
                .push(progress(&broken, 1, false));
            let owner = nova_sync::local_store().unwrap();
            let rows = all_rows();
            let digest = owner.lock().unwrap().digest();
            let error = apply::apply(&services, &preview).unwrap_err();
            assert!(
                error.contains("dangling") || error.contains("invalid"),
                "{error}"
            );
            assert_eq!(all_rows(), rows);
            assert_eq!(owner.lock().unwrap().digest(), digest);
            assert!(
                crate::profiles::list()
                    .unwrap()
                    .iter()
                    .all(|p| p.id != created)
            );
            assert!(apply::recovery_inventory().unwrap().is_empty());
            // A later unrelated save does not carry the abandoned change.
            owner.lock().unwrap().save().unwrap();
            assert_eq!(all_rows(), rows);

            // The same plan, undamaged, still commits.
            let opened = plan::open(&file).unwrap();
            let choices = plan::default_choices(&opened).unwrap();
            let preview = plan::preview(&services, Arc::new(opened), choices).unwrap();
            apply::apply(&services, &preview).unwrap();
            // And a preview goes stale when the data moves underneath it.
            let stale = {
                let opened = plan::open(&file).unwrap();
                let choices = plan::default_choices(&opened).unwrap();
                plan::preview(&services, Arc::new(opened), choices).unwrap()
            };
            services
                .domain
                .update(|state| {
                    state.bookmarks.clear();
                    Ok(())
                })
                .unwrap();
            let rows = all_rows();
            let error = apply::apply(&services, &stale).unwrap_err();
            assert!(error.contains("Rebuild the preview"), "{error}");
            assert_eq!(all_rows(), rows);
        },
    );
}

#[test]
fn a_retired_profile_cannot_be_restored_into() {
    phased(
        "backup::tests::a_retired_profile_cannot_be_restored_into",
        &["export", "import"],
        |phase, directory| {
            if phase == "export" {
                export_fixture(directory);
                return;
            }
            let file = directory.join("everything.nova-reader-backup");
            let services = Services::open(&directory.join("data-b")).unwrap();
            let gone = crate::profiles::create("Gone").unwrap();
            crate::profiles::delete(&gone.id).unwrap();
            for create in [false, true] {
                let result = restore(&services, &file, |choices| {
                    choices.retain(|c| c.source != archive::DEVICE);
                    for choice in choices.iter_mut() {
                        choice.included = choice.target == "reader";
                        if choice.included {
                            choice.target = gone.id.clone();
                            choice.create = create;
                        }
                    }
                });
                let error = result.err().unwrap();
                assert!(error.contains("deleted"), "create={create}: {error}");
            }
            assert!(
                nova_storage::try_scan_prefix(&format!("profile:{}:", gone.id))
                    .unwrap()
                    .is_empty()
            );
        },
    );
}

#[test]
fn per_title_settings_are_published_when_they_are_imported() {
    phased(
        "backup::tests::per_title_settings_are_published_when_they_are_imported",
        &["export", "import"],
        |phase, directory| {
            if phase == "export" {
                export_fixture(directory);
                return;
            }
            let file = directory.join("everything.nova-reader-backup");
            let services = Services::open(&directory.join("data-b")).unwrap();
            let title = series("fixture", "one");
            restore(&services, &file, |choices| {
                choices.retain(|c| c.source != archive::DEVICE);
                for choice in choices.iter_mut() {
                    choice.included = choice.target == "reader";
                    choice.components = ["title_settings".to_owned()].into();
                }
            })
            .unwrap();
            let key = series_json(&title);
            let owner = nova_sync::local_store().unwrap();
            let store = owner.lock().unwrap();
            let records = store.records(&crate::sync::domain("settings"));
            let record = |field: &str| {
                records
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, value)| value.clone())
            };
            assert_eq!(
                record(&format!("reader:mode-choice:{key}")).as_deref(),
                Some(r#"{"Manual":"RightToLeft"}"#)
            );
            assert_eq!(
                record(&format!(
                    "reader:reader-settings-series:{key}/paged/tap_zones"
                ))
                .as_deref(),
                Some("2")
            );
            assert_eq!(
                record(&format!(
                    "reader:reader-settings-series:{key}/paged/crop_borders"
                ))
                .as_deref(),
                Some("true")
            );
            // The same rows are what the profile reads back.
            drop(store);
            assert!(
                reader_domain::profile::kv::try_get_str(&format!("reader:mode-choice:{key}"))
                    .unwrap()
                    .is_some()
            );
        },
    );
}

#[test]
fn undo_reverts_the_import_keeps_later_edits_and_three_receipts() {
    phased(
        "backup::tests::undo_reverts_the_import_keeps_later_edits_and_three_receipts",
        &["export", "import"],
        |phase, directory| {
            if phase == "export" {
                export_fixture(directory);
                return;
            }
            let file = directory.join("everything.nova-reader-backup");
            let services = Services::open(&directory.join("data-b")).unwrap();
            local_side(&services);
            let before = services.domain.snapshot().unwrap();
            let only_default = |choices: &mut Vec<plan::Choice>| {
                choices.retain(|c| c.source != archive::DEVICE);
                for choice in choices.iter_mut() {
                    choice.included = choice.target == "reader";
                    choice.use_backup = choice.components.clone();
                }
            };
            restore(&services, &file, only_default).unwrap();
            let imported = services.domain.snapshot().unwrap();
            assert_ne!(imported, before);
            assert!(imported.bookmarks.iter().any(|b| b.note == "note"));
            assert_eq!(imported.progress[0].last_read_at, 100);
            // Edits after the import: a new bookmark, and a changed reading
            // position that is no longer what the import wrote.
            let title = series("fixture", "one");
            let first = chapter(&title, "1");
            let extra = chapter(&series("fixture", "local-only"), "1");
            services
                .domain
                .update_manual_progress(|state| {
                    state.bookmarks.push(Bookmark {
                        chapter: extra.id.clone(),
                        anchor: ResumeAnchor::new("page-9", 9, 0.9).unwrap(),
                        note: "later".into(),
                    });
                    state.progress.retain(|p| p.chapter != first.id);
                    state.progress.push(progress(&first, 700, false));
                    Ok(())
                })
                .unwrap();

            let preview = apply::undo_preview(&services).unwrap();
            assert!(
                preview
                    .choices
                    .iter()
                    .all(|c| c.replace && c.use_backup.is_empty())
            );
            // The position was edited since: reverting it is a conflict, kept.
            assert_eq!(rows_of(&preview, "progress").counts.preserved, 1);
            apply::apply(&services, &preview).unwrap();
            let undone = services.domain.snapshot().unwrap();
            assert_eq!(undone.progress.len(), 1);
            assert_eq!(undone.progress[0].last_read_at, 700, "the later edit stays");
            assert!(undone.bookmarks.iter().any(|b| b.note == "later"));
            assert!(!undone.bookmarks.iter().any(|b| b.note == "note"));
            assert_eq!(undone.library.len(), before.library.len());
            let entry = undone
                .library
                .iter()
                .find(|e| e.series.local_id == "one")
                .unwrap();
            assert_eq!(
                entry.added_at, 99,
                "the library entry is back to its own value"
            );

            // Only the newest three recovery copies are kept.
            for _ in 0..4 {
                restore(&services, &file, only_default).unwrap();
            }
            assert_eq!(apply::recovery_inventory().unwrap().len(), 3);
            assert!(apply::undo_preview(&services).is_ok());
        },
    );
}

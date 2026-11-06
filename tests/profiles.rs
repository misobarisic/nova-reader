//! Per-user profiles: identity, isolation of personal data, migration of the
//! pre-profile layout, sync of profiles and their data, and retirement. Storage
//! and the sync owner are per-process, so each scenario re-executes itself.
use nova_reader::{
    content_presentation::{Presentation, Settings, UserPreference},
    profiles, reader_settings, reading_preferences,
    services::Services,
    sync,
};
use reader_domain::{DomainStore, LibraryEntry, Series, SeriesId, profile};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Cursor, Write},
    process::Command,
};

const CHILD: &str = "READER_PROFILES_TEST_CHILD";

fn isolated(name: &str, body: impl FnOnce(&std::path::Path)) {
    if let Some(directory) = std::env::var_os(CHILD) {
        body(std::path::Path::new(&directory));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
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

fn series(local: &str) -> Series {
    Series {
        id: SeriesId::new("source", local),
        title: local.into(),
        publication_status: Default::default(),
        description: String::new(),
        authors: vec![],
        genres: vec![],
        cover_url: None,
        canonical_url: None,
    }
}
fn entry(id: SeriesId) -> LibraryEntry {
    LibraryEntry {
        series: id,
        categories: BTreeSet::new(),
        preferences: None,
        bookmarked: false,
        added_at: 1,
        reading_status: Default::default(),
    }
}
fn add(domain: &DomainStore, local: &str) {
    let series = series(local);
    domain
        .update(|state| {
            state.library.push(entry(series.id.clone()));
            state.series.push(series);
            Ok(())
        })
        .unwrap();
}
fn titles(domain: &DomainStore) -> Vec<String> {
    let mut titles: Vec<_> = domain
        .snapshot()
        .unwrap()
        .library
        .iter()
        .map(|entry| entry.series.local_id.clone())
        .collect();
    titles.sort();
    titles
}
fn owner() -> nova_sync::Store {
    nova_sync::local_store().unwrap().lock().unwrap().clone()
}
fn exchange(peer: &nova_sync::Store) {
    let owner = nova_sync::local_store().unwrap();
    let mut local = owner.lock().unwrap();
    for out in peer.outbound(&local.digest()) {
        local.apply(&out.domain, &out.key, out.record);
    }
    local.save().unwrap();
}
fn archive(id: &str) -> Vec<u8> {
    let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":id,"name":"Fixture","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":[]},"configuration":[{"key":"label","name":"Label","kind":"text","default":"public"}],"capabilities":["browse","search","filters","details","chapters","pages"]});
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

#[test]
fn profiles_keep_library_progress_and_personal_settings_apart() {
    isolated(
        "profiles_keep_library_progress_and_personal_settings_apart",
        |directory| {
            let services = Services::open(directory).unwrap();
            let domain = &services.domain;
            assert_eq!(domain.active_profile(), profiles::DEFAULT_ID);
            let b = profiles::create("Second").unwrap();
            assert!(profiles::valid_id(&b.id));
            add(domain, "alpha");
            reading_preferences::set_show_adult(true).unwrap();
            reader_settings::update("paged_side_padding", 7).unwrap();
            let global_before = nova_storage::get_str("reader:image-limits");

            profiles::activate(domain, &b.id).unwrap();
            assert_eq!(domain.active_profile(), b.id);
            assert!(titles(domain).is_empty(), "a new profile starts empty");
            assert!(!reading_preferences::show_adult());
            assert_eq!(reader_settings::snapshot().paged.side_padding, 0);
            add(domain, "beta");
            reading_preferences::set_show_adult(false).unwrap();
            reader_settings::update("paged_side_padding", 3).unwrap();
            Settings::set_user(&SeriesId::new("source", "beta"), UserPreference::Gallery).unwrap();
            // Image limits and other device settings stay global.
            assert_eq!(nova_storage::get_str("reader:image-limits"), global_before);
            assert_eq!(profile::key("reader:image-limits"), "reader:image-limits");

            profiles::activate(domain, profiles::DEFAULT_ID).unwrap();
            assert_eq!(titles(domain), ["alpha"], "the first profile is unchanged");
            assert!(reading_preferences::show_adult());
            assert_eq!(reader_settings::snapshot().paged.side_padding, 7);
            assert_eq!(
                Settings::load(&SeriesId::new("source", "beta"))
                    .unwrap()
                    .effective(),
                Presentation::Reader
            );

            profiles::activate(domain, &b.id).unwrap();
            assert_eq!(titles(domain), ["beta"], "switching back restores its data");
            assert!(!reading_preferences::show_adult());
            assert_eq!(reader_settings::snapshot().paged.side_padding, 3);
            assert_eq!(
                Settings::load(&SeriesId::new("source", "beta"))
                    .unwrap()
                    .effective(),
                Presentation::Gallery
            );
            // Each profile persists under its own namespace.
            assert!(nova_storage::get_str("profile:reader:reader:state").is_some());
            assert!(nova_storage::get_str(&format!("profile:{}:reader:state", b.id)).is_some());
            assert!(nova_storage::get_str("reader:state").is_none());
        },
    );
}

#[test]
fn the_pre_profile_layout_migrates_into_the_reader_profile_once() {
    isolated(
        "the_pre_profile_layout_migrates_into_the_reader_profile_once",
        |directory| {
            nova_storage::init_at(directory);
            let first = series("legacy");
            let mut legacy = reader_domain::ReaderState::default();
            legacy.library.push(entry(first.id.clone()));
            legacy.series.push(first.clone());
            let presentation_key = format!(
                "reader:content-presentation:{}",
                serde_json::to_string(&first.id).unwrap()
            );
            let personal = [
                ("reader:state", serde_json::to_string(&legacy).unwrap()),
                ("reader:show-adult", "true".to_owned()),
                ("reader:last-update", "1234".to_owned()),
                ("reader:appearance", "{\"animations\":false}".to_owned()),
                (
                    "reader:disabled-catalogs:\"fixture\"",
                    "[\"c\"]".to_owned(),
                ),
                (
                    presentation_key.as_str(),
                    json!({"schema_version":1,"settings":{"user":"Gallery","last_known_addon_hint":null}}).to_string(),
                ),
            ];
            let global = [
                ("reader:image-limits", "{\"limits\":1}"),
                ("reader:image-storage", "{}"),
                ("reader:captures", "{}"),
                ("reader:cache-gc:artwork:v1", "9"),
                ("sync:settings", "{\"enabled\":false}"),
            ];
            for (key, value) in personal.iter().map(|(k, v)| (*k, v.as_str())).chain(global) {
                nova_storage::try_set_str(key, value).unwrap();
            }
            let services = Services::open(directory).unwrap();
            // Personal rows moved, with nothing lost; device-global rows did not.
            for (key, value) in &personal {
                assert!(
                    nova_storage::get_str(key).is_none(),
                    "{key} must leave the legacy layout"
                );
                let moved = nova_storage::get_str(&format!("profile:reader:{key}"))
                    .unwrap_or_else(|| panic!("{key} was not migrated"));
                match *key {
                    // Startup seeding may normalize these documents' defaults.
                    "reader:state" => {}
                    "reader:appearance" => assert!(moved.contains("\"animations\":false")),
                    _ => assert_eq!(&moved, value),
                }
            }
            for (key, value) in global {
                let kept = nova_storage::get_str(key).unwrap_or_else(|| panic!("{key} vanished"));
                // Synced documents may be normalized at startup; the rest are untouched.
                if !matches!(key, "reader:image-storage" | "reader:captures") {
                    assert_eq!(kept, value, "{key}");
                }
                assert!(nova_storage::get_str(&format!("profile:reader:{key}")).is_none());
            }
            assert_eq!(titles(&services.domain), ["legacy"]);
            assert!(reading_preferences::show_adult());
            assert_eq!(
                Settings::load(&first.id).unwrap().user,
                UserPreference::Gallery
            );
            // Another startup changes nothing.
            let rows = |prefix: &str| nova_storage::try_scan_prefix(prefix).unwrap();
            let before = (rows("profile:"), rows("reader:"), rows("profiles:"));
            assert!(!profile::migrate_legacy().unwrap());
            DomainStore::open(directory).unwrap();
            let after = (rows("profile:"), rows("reader:"), rows("profiles:"));
            assert_eq!(before, after);
            // A newer destination wins; the old row is neither copied over it nor lost.
            nova_storage::try_set_str("reader:show-adult", "false").unwrap();
            assert!(!profile::migrate_legacy().unwrap());
            assert_eq!(
                nova_storage::get_str("reader:show-adult").as_deref(),
                Some("false")
            );
            assert_eq!(
                nova_storage::get_str("profile:reader:reader:show-adult").as_deref(),
                Some("true")
            );
        },
    );
}

#[test]
fn profiles_and_their_libraries_sync_across_devices_and_deletion_is_permanent() {
    isolated(
        "profiles_and_their_libraries_sync_across_devices_and_deletion_is_permanent",
        |directory| {
            let services = Services::open(directory).unwrap();
            let domain = &services.domain;
            add(domain, "mine");
            let default = profiles::list().unwrap();
            assert_eq!(default.len(), 1);
            assert_eq!(default[0].id, profiles::DEFAULT_ID);
            assert_eq!(default[0].name, profiles::DEFAULT_NAME);

            // Device 1 created profile `pkid` and read a title in it.
            let mut peer = nova_sync::Store::default();
            peer.set(
                profiles::DOMAIN,
                "pkid",
                Some(json!({"name":"Kid"}).to_string()),
                0,
                999,
            );
            let kid = series("kid-title");
            peer.set(
                &sync::domain_for("pkid", "series"),
                &serde_json::to_string(&kid.id).unwrap(),
                Some(serde_json::to_string(&kid).unwrap()),
                0,
                999,
            );
            peer.set(
                &sync::domain_for("pkid", "library"),
                &serde_json::to_string(&kid.id).unwrap(),
                Some(serde_json::to_string(&entry(kid.id.clone())).unwrap()),
                0,
                999,
            );
            exchange(&peer);
            assert!(sync::handles_projection_domain(profiles::DOMAIN));
            assert!(sync::handles_projection_domain(&sync::domain_for(
                "pkid", "library"
            )));
            sync::apply_remote(domain).unwrap();
            let listed = profiles::list().unwrap();
            assert_eq!(
                listed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
                [profiles::DEFAULT_ID, "pkid"]
            );
            // The other profile materializes in the background, untouched screen.
            assert_eq!(domain.active_profile(), profiles::DEFAULT_ID);
            assert_eq!(titles(domain), ["mine"]);
            let background = domain.snapshot_profile("pkid").unwrap();
            assert_eq!(background.library.len(), 1);
            assert_eq!(background.library[0].series.local_id, "kid-title");
            profiles::activate(domain, "pkid").unwrap();
            assert_eq!(titles(domain), ["kid-title"]);
            profiles::activate(domain, profiles::DEFAULT_ID).unwrap();

            // Device 1 deletes it.
            peer.set(profiles::DELETIONS_DOMAIN, "pkid", Some("1".into()), 0, 999);
            peer.set(profiles::DOMAIN, "pkid", None, 0, 999);
            exchange(&peer);
            sync::apply_remote(domain).unwrap();
            let listed = profiles::list().unwrap();
            assert_eq!(listed.len(), 1);
            assert!(profiles::is_retired("pkid"));
            assert!(
                nova_storage::try_scan_prefix("profile:pkid:")
                    .unwrap()
                    .is_empty(),
                "the deleted profile's data leaves this device"
            );
            assert!(!sync::handles_projection_domain(&sync::domain_for(
                "pkid", "library"
            )));
            // A live write to a retired profile is refused, and it cannot be recreated.
            let owner_store = nova_sync::local_store().unwrap();
            let accepted = owner_store.lock().unwrap().set(
                &sync::domain_for("pkid", "library"),
                "\"late\"",
                Some("{}".into()),
                0,
                1,
            );
            assert!(!accepted, "a live write to a retired profile is refused");
            assert!(profiles::rename("pkid", "Back").is_err());
            // Records that a stale peer still delivers are retombstoned.
            let mut stale = nova_sync::Store::default();
            stale.set(
                &sync::domain_for("pkid", "series"),
                &serde_json::to_string(&kid.id).unwrap(),
                Some(serde_json::to_string(&kid).unwrap()),
                0,
                5000,
            );
            exchange(&stale);
            profiles::list().unwrap();
            assert!(
                owner()
                    .records(&sync::domain_for("pkid", "series"))
                    .is_empty()
            );
        },
    );
}

#[test]
fn default_profile_seed_never_outranks_a_peers_rename() {
    isolated(
        "default_profile_seed_never_outranks_a_peers_rename",
        |directory| {
            let _services = Services::open(directory).unwrap();
            let record = owner()
                .record(profiles::DOMAIN, profiles::DEFAULT_ID)
                .cloned()
                .unwrap();
            assert_eq!(record.version.ts, 0, "the seed has the neutral version");
            assert_eq!(record.version.counter, 0);
            let mut peer = nova_sync::Store::default();
            peer.set(
                profiles::DOMAIN,
                profiles::DEFAULT_ID,
                Some(json!({"name":"Mina"}).to_string()),
                0,
                999,
            );
            exchange(&peer);
            let listed = profiles::list().unwrap();
            assert_eq!(listed[0].name, "Mina");
            assert_eq!(listed[0].id, profiles::DEFAULT_ID);
        },
    );
}

#[test]
fn retired_ids_are_never_reused_and_a_fresh_one_replaces_a_fully_deleted_account() {
    isolated(
        "retired_ids_are_never_reused_and_a_fresh_one_replaces_a_fully_deleted_account",
        |directory| {
            let services = Services::open(directory).unwrap();
            let a = profiles::create("A").unwrap();
            let b = profiles::create("B").unwrap();
            assert_ne!(a.id, b.id);
            assert_ne!(a.id, profiles::DEFAULT_ID);
            // The selected profile and the last remaining one cannot be deleted.
            assert!(profiles::delete(profiles::DEFAULT_ID).is_err());
            profiles::delete(&a.id).unwrap();
            assert!(profiles::delete(&a.id).is_err());
            let c = profiles::create("C").unwrap();
            assert_ne!(c.id, a.id, "a deleted id is never handed out again");
            // Peers delete every remaining profile (including the first) offline.
            let mut peer = nova_sync::Store::default();
            for id in [profiles::DEFAULT_ID, &b.id, &c.id] {
                peer.set(profiles::DELETIONS_DOMAIN, id, Some("1".into()), 0, 999);
                peer.set(profiles::DOMAIN, id, None, 0, 999);
            }
            exchange(&peer);
            let preferences = profiles::initialize().unwrap();
            let survivors = profiles::list().unwrap();
            assert_eq!(survivors.len(), 1);
            let fresh = &survivors[0];
            for retired in [
                profiles::DEFAULT_ID,
                a.id.as_str(),
                b.id.as_str(),
                c.id.as_str(),
            ] {
                assert_ne!(fresh.id, retired);
                assert!(profiles::is_retired(retired));
            }
            assert_eq!(preferences.active_profile, fresh.id);
            profiles::activate(&services.domain, &fresh.id).unwrap();
            assert_eq!(services.domain.active_profile(), fresh.id);
        },
    );
}

#[test]
fn a_worker_started_before_a_switch_cannot_write_into_the_new_profile() {
    isolated(
        "a_worker_started_before_a_switch_cannot_write_into_the_new_profile",
        |directory| {
            let services = Services::open(directory).unwrap();
            let domain = services.domain.clone();
            let b = profiles::create("B").unwrap();
            let (ready, started) = std::sync::mpsc::channel();
            let (go, release) = std::sync::mpsc::channel::<()>();
            let worker_domain = domain.clone();
            let hinted = SeriesId::new("source", "slow");
            let worker_id = hinted.clone();
            let worker = profiles::spawn(move || {
                ready.send(()).unwrap();
                release.recv().unwrap();
                // A details response arriving late: metadata and a hint.
                let series = series("slow");
                worker_domain
                    .update(|state| {
                        state.series.push(series);
                        Ok(())
                    })
                    .unwrap();
                Settings::record_hint(&worker_id, Some("gallery")).unwrap();
            });
            started.recv().unwrap();
            profiles::activate(&domain, &b.id).unwrap();
            go.send(()).unwrap();
            worker.join().unwrap();
            let in_b = domain.snapshot().unwrap();
            assert!(
                in_b.series.is_empty(),
                "nothing leaked into the new profile"
            );
            assert_eq!(
                Settings::load(&hinted).unwrap().effective(),
                Presentation::Reader
            );
            let in_a = domain.snapshot_profile(profiles::DEFAULT_ID).unwrap();
            assert_eq!(
                in_a.series.len(),
                1,
                "the late result reached its own profile"
            );
            assert!(
                nova_storage::get_str(&format!(
                    "profile:reader:reader:content-presentation:{}",
                    serde_json::to_string(&hinted).unwrap()
                ))
                .is_some()
            );
        },
    );
}

#[test]
fn source_settings_follow_the_profile_and_the_original_directory_is_kept() {
    isolated(
        "source_settings_follow_the_profile_and_the_original_directory_is_kept",
        |directory| {
            let services = Services::open(directory).unwrap();
            services.sources.install_zip(&archive("fixture")).unwrap();
            services
                .sources
                .set_settings("fixture", json!({"label":"first"}))
                .unwrap();
            let settings_dir = |profile: &str| {
                directory
                    .join("addons")
                    .join("settings")
                    .join(format!("{:x}", Sha256::digest(profile.as_bytes())))
            };
            // The first profile maps to the registry's original identity.
            assert!(settings_dir("default").join("fixture.json").exists());
            let b = profiles::create("B").unwrap();
            profiles::activate(&services.domain, &b.id).unwrap();
            assert_eq!(
                services.sources.settings("fixture").unwrap()["label"],
                "public"
            );
            services
                .sources
                .set_settings("fixture", json!({"label":"second"}))
                .unwrap();
            assert!(settings_dir(&b.id).join("fixture.json").exists());
            profiles::activate(&services.domain, profiles::DEFAULT_ID).unwrap();
            assert_eq!(
                services.sources.settings("fixture").unwrap()["label"],
                "first"
            );
            // Installed packages are shared; deleting a profile removes its settings only.
            profiles::delete(&b.id).unwrap();
            assert!(!settings_dir(&b.id).exists());
            assert!(settings_dir("default").join("fixture.json").exists());
            assert_eq!(services.sources.list().unwrap().len(), 1);
        },
    );
}

#[test]
fn a_peers_addon_removal_in_one_profile_keeps_what_another_profile_lists() {
    use nova_reader::sync_addons;
    use std::sync::{Arc, atomic::AtomicBool};
    isolated(
        "a_peers_addon_removal_in_one_profile_keeps_what_another_profile_lists",
        |directory| {
            let services = Services::open(directory).unwrap();
            services.sources.install_zip(&archive("fixture")).unwrap();
            services.sources.set_enabled("fixture", true).unwrap();
            sync_addons::publish(&services).unwrap();
            let other = profiles::create("Other").unwrap();
            // The other profile also lists the addon (published when selected).
            profiles::activate(&services.domain, &other.id).unwrap();
            sync_addons::publish(&services).unwrap();
            profiles::activate(&services.domain, profiles::DEFAULT_ID).unwrap();
            let mut settings = nova_sync::read_settings();
            settings.enabled = true;
            nova_sync::write_settings(&settings);
            // A peer removes it from the first profile only.
            let mut peer = nova_sync::Store::default();
            peer.set(&sync_addons::domain(), "fixture", None, 0, 999);
            exchange(&peer);
            sync_addons::reconcile(&services, Arc::new(AtomicBool::new(false))).unwrap();
            assert_eq!(
                services.sources.list().unwrap().len(),
                1,
                "still listed elsewhere"
            );
            // Once no live profile lists it, the removal applies.
            let mut peer = nova_sync::Store::default();
            peer.set(
                &sync::domain_for(&other.id, "addons"),
                "fixture",
                None,
                0,
                1000,
            );
            exchange(&peer);
            sync_addons::reconcile(&services, Arc::new(AtomicBool::new(false))).unwrap();
            assert!(services.sources.list().unwrap().is_empty());
        },
    );
}

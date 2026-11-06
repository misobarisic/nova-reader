//! Entry presentation (the user's Default/Reader/Gallery choice and the addon's
//! hint) travels with the account, so a synced entry opens the same way on every
//! paired device. These tests stand in for "device B": the process owns the
//! local store and a plain `nova_sync::Store` plays the peer.
use nova_reader::{
    content_presentation::{Presentation, Settings, UserPreference},
    services::Services,
    sync,
};
use reader_domain::SeriesId;
use serde_json::json;
use std::process::Command;

const CHILD: &str = "READER_PRESENTATION_SYNC_CHILD";

/// Storage and the sync owner are process-wide, so each scenario gets a fresh
/// process (and data directory) by re-executing exactly itself.
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

fn domain() -> String {
    sync::domain("presentation")
}
fn id(local: &str) -> SeriesId {
    SeriesId::new("source", local)
}
fn key(series: &SeriesId) -> String {
    serde_json::to_string(series).unwrap()
}
fn wire(user: &str, hint: Option<&str>) -> String {
    json!({"schema_version":1,"user":user,"hint":hint}).to_string()
}
fn open(directory: &std::path::Path) -> Services {
    nova_storage::init_at(directory);
    Services::open(directory).unwrap()
}
fn dto(local: &str, presentation: Option<&str>) -> reader_sources::Series {
    let mut value = json!({"id":local,"title":local});
    if let Some(hint) = presentation {
        value["presentation"] = hint.into();
    }
    serde_json::from_value(value).unwrap()
}
fn owner() -> nova_sync::Store {
    nova_sync::local_store().unwrap().lock().unwrap().clone()
}
fn peer_put(peer: &mut nova_sync::Store, series: &SeriesId, value: Option<String>) {
    peer.set(&domain(), &key(series), value, 0, 999);
}
fn exchange(peer: &nova_sync::Store) {
    let owner = nova_sync::local_store().unwrap();
    let mut local = owner.lock().unwrap();
    for out in peer.outbound(&local.digest()) {
        local.apply(&out.domain, &out.key, out.record);
    }
    local.save().unwrap();
}
fn record_value(series: &SeriesId) -> Option<serde_json::Value> {
    owner()
        .record(&domain(), &key(series))
        .and_then(|record| record.value.as_deref())
        .map(|raw| serde_json::from_str(raw).unwrap())
}

#[test]
fn synced_hint_makes_the_entry_open_as_gallery_on_another_device() {
    isolated(
        "synced_hint_makes_the_entry_open_as_gallery_on_another_device",
        |directory| {
            let services = open(directory);
            let series = id("collection");
            // Nothing is known here yet: this is the device that never fetched
            // details from the source.
            assert_eq!(
                Settings::load(&series).unwrap().effective(),
                Presentation::Reader
            );
            let mut peer = nova_sync::Store::default();
            peer_put(&mut peer, &series, Some(wire("default", Some("gallery"))));
            exchange(&peer);
            sync::apply_remote(&services.domain).unwrap();
            let settings = Settings::load(&series).unwrap();
            assert_eq!(settings.last_known_addon_hint.as_deref(), Some("gallery"));
            assert_eq!(settings.effective(), Presentation::Gallery);
            // Remote arrivals must wake the projection worker.
            assert!(sync::handles_projection_domain(&domain()));
            // The projection is durable, not just a cache entry.
            let raw =
                nova_storage::get_str(&format!("reader:content-presentation:{}", key(&series)))
                    .expect("projected presentation is persisted");
            assert!(raw.contains("gallery"), "{raw}");
            // A projection never echoes back as a local edit.
            let before = owner().digest();
            sync::apply_remote(&services.domain).unwrap();
            assert_eq!(owner().digest(), before);
        },
    );
}

#[test]
fn explicit_user_choice_wins_over_hints_across_devices() {
    isolated(
        "explicit_user_choice_wins_over_hints_across_devices",
        |directory| {
            let services = open(directory);
            let forced_gallery = id("forced-gallery");
            let forced_reader = id("forced-reader");
            // This device saw a reader hint; the account carries Gallery.
            Settings::record_hint(&forced_gallery, Some("reader")).unwrap();
            // This device saw a gallery hint; the account carries Reader.
            Settings::record_hint(&forced_reader, Some("gallery")).unwrap();
            let mut peer = nova_sync::Store::default();
            peer_put(&mut peer, &forced_gallery, Some(wire("gallery", None)));
            peer_put(
                &mut peer,
                &forced_reader,
                Some(wire("reader", Some("gallery"))),
            );
            exchange(&peer);
            sync::apply_remote(&services.domain).unwrap();
            let gallery = Settings::load(&forced_gallery).unwrap();
            assert_eq!(gallery.user, UserPreference::Gallery);
            assert_eq!(gallery.effective(), Presentation::Gallery);
            let reader = Settings::load(&forced_reader).unwrap();
            assert_eq!(reader.user, UserPreference::Reader);
            assert_eq!(reader.effective(), Presentation::Reader);
        },
    );
}

#[test]
fn listing_without_a_hint_keeps_the_synced_hint_but_a_detail_refresh_withdraws_it() {
    isolated(
        "listing_without_a_hint_keeps_the_synced_hint_but_a_detail_refresh_withdraws_it",
        |directory| {
            let services = open(directory);
            let series = id("collection");
            let mut peer = nova_sync::Store::default();
            peer_put(&mut peer, &series, Some(wire("default", Some("gallery"))));
            exchange(&peer);
            sync::apply_remote(&services.domain).unwrap();
            assert_eq!(
                Settings::load(&series).unwrap().effective(),
                Presentation::Gallery
            );
            let published = owner().record(&domain(), &key(&series)).unwrap().clone();
            // Many catalogues omit the optional field from browse/search rows.
            services.series_listing("source", dto("collection", None));
            assert_eq!(
                Settings::load(&series).unwrap().effective(),
                Presentation::Gallery,
                "a browse row without a hint is not a withdrawal"
            );
            assert_eq!(
                owner().record(&domain(), &key(&series)),
                Some(&published),
                "and publishes nothing"
            );
            // A listing that does carry the hint is still captured.
            services.series_listing("source", dto("collection", Some("gallery")));
            assert_eq!(
                Settings::load(&series).unwrap().effective(),
                Presentation::Gallery
            );
            // The full details response is authoritative.
            services.series("source", dto("collection", None));
            assert_eq!(
                Settings::load(&series).unwrap().effective(),
                Presentation::Reader
            );
            assert_eq!(
                record_value(&series).unwrap()["hint"],
                serde_json::Value::Null
            );
        },
    );
}

#[test]
fn malformed_presentation_records_are_rejected_by_the_policy() {
    isolated(
        "malformed_presentation_records_are_rejected_by_the_policy",
        |directory| {
            let services = open(directory);
            let good = id("good");
            let mut peer = nova_sync::Store::default();
            peer_put(&mut peer, &good, Some(wire("gallery", Some("gallery"))));
            // Unknown fields from a newer peer are retained, not rejected.
            let extended = id("extended");
            peer_put(
                &mut peer,
                &extended,
                Some(
                    json!({"schema_version":1,"user":"reader","hint":null,"future":{"a":1}})
                        .to_string(),
                ),
            );
            exchange(&peer);
            assert!(owner().record(&domain(), &key(&good)).is_some());
            assert!(owner().record(&domain(), &key(&extended)).is_some());
            let invalid: Vec<(String, Option<String>)> = vec![
                (key(&id("not-json")), Some("{".into())),
                (key(&id("not-object")), Some("[1]".into())),
                (key(&id("bad-user")), Some(wire("admin", None))),
                (key(&id("bad-hint")), Some(wire("default", Some("future-mode")))),
                (
                    key(&id("version-zero")),
                    Some(json!({"schema_version":0,"user":"default","hint":null}).to_string()),
                ),
                (
                    key(&id("no-version")),
                    Some(json!({"user":"default","hint":null}).to_string()),
                ),
                (
                    key(&id("no-user")),
                    Some(json!({"schema_version":1,"hint":null}).to_string()),
                ),
                (
                    key(&id("huge")),
                    Some(json!({"schema_version":1,"user":"default","hint":null,"pad":"x".repeat(70_000)}).to_string()),
                ),
                ("not a series id".into(), Some(wire("gallery", None))),
                (
                    serde_json::to_string(&SeriesId::new("", "empty-source")).unwrap(),
                    Some(wire("gallery", None)),
                ),
                ("not a series id".into(), None),
            ];
            let mut peer = nova_sync::Store::default();
            for (key, value) in &invalid {
                peer.set(&domain(), key, value.clone(), 0, 999);
            }
            exchange(&peer);
            for (key, _) in &invalid {
                assert!(
                    owner().record(&domain(), key).is_none(),
                    "accepted invalid record {key}"
                );
            }
            sync::apply_remote(&services.domain).unwrap();
            assert_eq!(
                Settings::load(&good).unwrap().effective(),
                Presentation::Gallery
            );
            assert_eq!(
                Settings::load(&id("bad-user")).unwrap(),
                Settings::default()
            );
        },
    );
}

#[test]
fn local_choices_publish_the_documented_record_and_stay_idle_when_unchanged() {
    isolated(
        "local_choices_publish_the_documented_record_and_stay_idle_when_unchanged",
        |directory| {
            let _services = open(directory);
            let series = id("collection");
            Settings::record_hint(&series, Some("gallery")).unwrap();
            assert_eq!(
                record_value(&series),
                Some(json!({"schema_version":1,"user":"default","hint":"gallery"}))
            );
            Settings::set_user(&series, UserPreference::Reader).unwrap();
            assert_eq!(
                record_value(&series),
                Some(json!({"schema_version":1,"user":"reader","hint":"gallery"}))
            );
            Settings::set_user(&series, UserPreference::Gallery).unwrap();
            assert_eq!(
                record_value(&series),
                Some(json!({"schema_version":1,"user":"gallery","hint":"gallery"}))
            );
            let digest = owner().digest();
            Settings::set_user(&series, UserPreference::Gallery).unwrap();
            Settings::record_hint(&series, Some("gallery")).unwrap();
            assert_eq!(owner().digest(), digest, "unchanged values publish nothing");
            // An unrecognized addon token is never published.
            let other = id("other");
            Settings::record_hint(&other, Some("future-mode")).unwrap();
            assert!(record_value(&other).is_none());
            // Local KV and the account record commit together.
            assert!(
                nova_storage::get_str(&format!("reader:content-presentation:{}", key(&series)))
                    .unwrap()
                    .contains("Gallery")
            );
        },
    );
}

#[test]
fn choices_made_before_sync_existed_are_seeded_once() {
    isolated(
        "choices_made_before_sync_existed_are_seeded_once",
        |directory| {
            nova_storage::init_at(directory);
            let chosen = id("chosen");
            let hinted = id("hinted");
            let untouched = id("untouched");
            for (series, user, hint) in [
                (&chosen, "Reader", Some("gallery")),
                (&hinted, "Default", Some("gallery")),
                (&untouched, "Default", None),
            ] {
                nova_storage::try_set_str(
                    &format!("reader:content-presentation:{}", key(series)),
                    &json!({"schema_version":1,"settings":{"user":user,"last_known_addon_hint":hint}}).to_string(),
                )
                .unwrap();
            }
            let _services = Services::open(directory).unwrap();
            assert_eq!(
                record_value(&chosen),
                Some(json!({"schema_version":1,"user":"reader","hint":"gallery"}))
            );
            assert_eq!(
                record_value(&hinted),
                Some(json!({"schema_version":1,"user":"default","hint":"gallery"}))
            );
            assert!(
                record_value(&untouched).is_none(),
                "defaults are not choices"
            );
        },
    );
}

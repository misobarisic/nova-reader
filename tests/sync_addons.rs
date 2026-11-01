use nova_reader::{
    services::Services,
    sync,
    sync_addons::{self, Descriptor},
};
use serde_json::json;
use std::{
    io::{Cursor, Write},
    sync::{Arc, atomic::AtomicBool},
};
fn archive(id: &str, version: &str, domains: &[&str]) -> Vec<u8> {
    let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":id,"name":"Fixture","version":version,"language":"en","entry":"main.js","permissions":{"domains":domains},"configuration":[{"key":"token","name":"Token","kind":"secret","default":""},{"key":"label","name":"Label","kind":"text","default":"public"},{"key":"other","name":"Other","kind":"text","default":"other-default"}],"capabilities":["browse","search","filters","details","chapters","pages"]});
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
fn reconcile_during_local_change(services: &Services) {
    let worker_services = services.clone();
    let (done, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        done.send(sync_addons::reconcile(
            &worker_services,
            Arc::new(AtomicBool::new(false)),
        ))
        .unwrap();
    });
    result
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("reconcile must skip an unpublished local mutation")
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn desired_packages_keep_secrets_and_permissions_local_and_publish_removals() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::open(directory.path()).unwrap();
    sync::initialize(&services.domain).unwrap();
    let bytes = archive("fixture", "1.0.0", &["example.com"]);
    services.sources.install_zip(&bytes).unwrap();
    services.sources.set_enabled("fixture", true).unwrap();
    services
        .sources
        .set_settings("fixture", json!({"token":"never-share","label":"portable"}))
        .unwrap();
    sync_addons::initialize(&services).unwrap();
    let owner = nova_sync::local_store().unwrap();
    let raw = owner
        .lock()
        .unwrap()
        .record(&sync_addons::domain(), "fixture")
        .unwrap()
        .value
        .clone()
        .unwrap();
    assert!(!raw.contains("never-share"));
    assert!(!raw.contains("token"));
    let mut desired: Descriptor = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        desired.settings,
        json!({"label":"portable","other":"other-default"})
    );
    assert!(nova_sync::engine().is_none());
    // Receiving desired enablement never grants execution on a device that
    // has explicitly disabled the same archive.
    services.sources.set_enabled("fixture", false).unwrap();
    desired.settings =
        json!({"label":"remote public", "other":"other-default", "token":"remote secret"});
    owner.lock().unwrap().set(
        &sync_addons::domain(),
        "fixture",
        Some(serde_json::to_string(&desired).unwrap()),
        0,
        999,
    );
    let mut settings = nova_sync::read_settings();
    settings.enabled = true;
    nova_sync::write_settings(&settings);
    let status = sync_addons::reconcile(&services, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(status.confirmation_required, 1);
    assert_eq!(
        services.sources.settings("fixture").unwrap(),
        json!({"label":"remote public","other":"other-default"})
    );
    assert!(!services.sources.list().unwrap()[0].enabled);
    assert_eq!(
        services
            .sources
            .settings_secrets("fixture")
            .unwrap()
            .get("token"),
        Some(&true)
    );
    // A disjoint remote field arrives but has not materialized locally yet.
    // The next local text edit must preserve it and the desired account grant.
    desired.settings = json!({"label":"remote public","other":"remote other"});
    owner.lock().unwrap().set(
        &sync_addons::domain(),
        "fixture",
        Some(serde_json::to_string(&desired).unwrap()),
        0,
        999,
    );

    // A reconciler starts after the filesystem/settings mutation but before
    // its outgoing snapshot. It must not overwrite the edit or acknowledge it
    // as a remote baseline. Waiting for completion inside the mutation makes
    // this schedule deterministic, without sleeps or scheduler assumptions.
    sync_addons::local_change(&services, || {
        services
            .sources
            .set_settings("fixture", json!({"label":"local intent"}))
            .map_err(|e| e.to_string())?;
        reconcile_during_local_change(&services);
        assert_eq!(
            services.sources.settings("fixture").unwrap(),
            json!({"label":"local intent","other":"other-default"})
        );
        Ok(())
    })
    .unwrap();
    let published: Descriptor = serde_json::from_str(
        owner
            .lock()
            .unwrap()
            .record(&sync_addons::domain(), "fixture")
            .unwrap()
            .value
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        published.settings,
        json!({"label":"local intent","other":"remote other"})
    );
    assert!(
        published.enabled,
        "a local permission denial is not an account disable"
    );
    assert!(!services.sources.list().unwrap()[0].enabled);
    // A second edit before projection proves the saved baseline remains local
    // actual state, rather than the merged outgoing account descriptor.
    sync_addons::local_change(&services, || {
        services
            .sources
            .set_settings("fixture", json!({"label":"local second"}))
            .map_err(|e| e.to_string())
    })
    .unwrap();
    let second: Descriptor = serde_json::from_str(
        owner
            .lock()
            .unwrap()
            .record(&sync_addons::domain(), "fixture")
            .unwrap()
            .value
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert!(second.enabled);
    assert_eq!(
        second.settings,
        json!({"label":"local second","other":"remote other"})
    );
    sync_addons::reconcile(&services, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(
        services.sources.settings("fixture").unwrap(),
        json!({"label":"local second","other":"remote other"})
    );
    // Local permission confirmation advances this device's granted baseline.
    sync_addons::local_change(&services, || {
        services
            .sources
            .set_enabled("fixture", true)
            .map_err(|e| e.to_string())
    })
    .unwrap();

    sync_addons::local_change(&services, || {
        services
            .sources
            .remove("fixture")
            .map_err(|e| e.to_string())?;
        // A missing archive still has desired remote state until publication.
        // Reconciliation must not erase the prior baseline and swallow removal.
        reconcile_during_local_change(&services);
        Ok(())
    })
    .unwrap();
    assert!(
        owner
            .lock()
            .unwrap()
            .record(&sync_addons::domain(), "fixture")
            .unwrap()
            .is_deleted()
    );
    desired.id = "../escape".into();
    let mut guard = owner.lock().unwrap();
    let raw = serde_json::to_string(&desired).unwrap();
    assert!(!guard.set(
        &sync_addons::domain(),
        "../escape",
        Some(raw.clone()),
        0,
        999
    ));
    assert!(guard.record(&sync_addons::domain(), "../escape").is_none());
    let record = nova_sync::Record {
        value: Some(raw),
        version: nova_sync::Version::new(1, 0, 999, false),
    };
    assert!(!sync_addons::validate_record("../escape", &record));
    drop(guard);
}

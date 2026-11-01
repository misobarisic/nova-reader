//! Durable Reader projection, independent of endpoint/network availability.
use nova_reader::sync;
use reader_domain::*;
use std::collections::BTreeSet;

fn series(source: &str, local: &str) -> Series {
    Series {
        id: SeriesId::new(source, local),
        title: local.into(),
        publication_status: Default::default(),
        description: String::new(),
        authors: vec![],
        genres: vec![],
        cover_url: None,
        canonical_url: None,
    }
}
fn library(id: SeriesId) -> LibraryEntry {
    LibraryEntry {
        series: id,
        categories: BTreeSet::new(),
        preferences: None,
        bookmarked: false,
        added_at: 1,
        reading_status: Default::default(),
    }
}
fn remote_put(
    store: &mut nova_sync::Store,
    kind: &str,
    id: &impl serde::Serialize,
    value: &impl serde::Serialize,
) {
    store.set(
        &sync::domain(kind),
        &serde_json::to_string(id).unwrap(),
        Some(serde_json::to_string(value).unwrap()),
        0,
        999,
    );
}
fn owner_snapshot() -> nova_sync::Store {
    nova_sync::local_store().unwrap().lock().unwrap().clone()
}
fn exchange(remote: &nova_sync::Store) {
    let owner = nova_sync::local_store().unwrap();
    let mut local = owner.lock().unwrap();
    for out in remote.outbound(&local.digest()) {
        local.apply(&out.domain, &out.key, out.record);
    }
    local.save().unwrap();
}
#[test]
fn durable_records_preserve_offline_edits_actions_deletions_and_source_identity() {
    const PHASE: &str = "READER_SYNC_DATA_TEST_PHASE";
    if std::env::var_os(PHASE).is_none() {
        let directory = tempfile::tempdir().unwrap();
        for phase in ["write", "restart"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "durable_records_preserve_offline_edits_actions_deletions_and_source_identity",
                    "--nocapture",
                ])
                .env(PHASE, phase)
                .env("READER_SYNC_DATA_TEST_DIRECTORY", directory.path())
                .status()
                .unwrap();
            assert!(status.success(), "sync phase {phase} failed");
        }
        return;
    }
    let directory =
        std::path::PathBuf::from(std::env::var_os("READER_SYNC_DATA_TEST_DIRECTORY").unwrap());
    let domain = DomainStore::open(&directory).unwrap();
    sync::initialize(&domain).unwrap();
    if std::env::var(PHASE).unwrap() == "restart" {
        let state = domain.snapshot().unwrap();
        assert_eq!(state.library.len(), 1);
        assert_eq!(state.library[0].series.source_id, "source-b");
        assert!(state.bookmarks.is_empty());
        assert!(state.history.is_empty());
        let digest = nova_sync::local_store().unwrap().lock().unwrap().digest();
        assert_eq!(
            serde_json::to_string(&digest).unwrap(),
            nova_storage::get_str("reader:test:sync-digest").unwrap(),
            "restart must not echo or resurrect data"
        );
        return;
    }
    assert!(!nova_sync::read_settings().enabled);
    assert!(nova_sync::engine().is_none());
    let first = series("source-a", "same-id");
    let second = series("source-b", "same-id");
    domain
        .update(|state| {
            state.series.push(first.clone());
            state.library.push(library(first.id.clone()));
            Ok(())
        })
        .unwrap();
    let mut peer = nova_sync::Store::default();
    remote_put(&mut peer, "series", &second.id, &second);
    remote_put(
        &mut peer,
        "library",
        &second.id,
        &library(second.id.clone()),
    );
    exchange(&peer);
    // Pending remote membership must not become a local deletion when metadata
    // changes before projection.
    domain
        .update(|state| {
            state.series[0].description = "new metadata".into();
            Ok(())
        })
        .unwrap();
    sync::apply_remote(&domain).unwrap();
    assert_eq!(domain.snapshot().unwrap().library.len(), 2);
    domain
        .update(|state| {
            state.categories.push(Category {
                id: "category-device-a".into(),
                name: "Before".into(),
            });
            state
                .library
                .iter_mut()
                .find(|entry| entry.series == first.id)
                .unwrap()
                .categories
                .insert("category-device-a".into());
            Ok(())
        })
        .unwrap();
    // Category renames retain membership identity across offline peers.
    let mut category_peer = owner_snapshot();
    let renamed = Category {
        id: "category-device-a".into(),
        name: "After".into(),
    };
    remote_put(&mut category_peer, "categories", &renamed.id, &renamed);
    exchange(&category_peer);
    sync::apply_remote(&domain).unwrap();
    let categorized = domain.snapshot().unwrap();
    assert_eq!(categorized.categories[0].name, "After");
    assert!(
        categorized
            .library
            .iter()
            .find(|entry| entry.series == first.id)
            .unwrap()
            .categories
            .contains("category-device-a")
    );
    let chapter = ChapterId::new(first.id.clone(), "chapter");
    domain
        .update(|state| {
            state.chapters.push(Chapter {
                id: chapter.clone(),
                title: "Chapter".into(),
                label: "1".into(),
                number: None,
                volume: None,
                source_order: 0,
                language: None,
                scanlator: None,
                uploaded_at: None,
            });
            state.set_progress(ReadingProgress {
                chapter: chapter.clone(),
                anchor: ResumeAnchor::new("stable-page", 0, 0.4)?,
                mode: ReadingMode::Webtoon,
                last_read_at: 100,
                completed: true,
            });
            state.bookmarks.push(Bookmark {
                chapter: chapter.clone(),
                anchor: ResumeAnchor::new("stable-page", 0, 0.4)?,
                note: "old".into(),
            });
            Ok(())
        })
        .unwrap();
    domain
        .update_manual_progress(|state| {
            let p = &mut state.progress[0];
            p.completed = false;
            p.last_read_at = 200;
            Ok(())
        })
        .unwrap();
    domain
        .update_checkpoint(|state| {
            let mut p = state.progress[0].clone();
            p.completed = true;
            p.last_read_at = 150;
            state.set_progress(p);
            Ok(())
        })
        .unwrap();
    assert!(!domain.snapshot().unwrap().progress[0].completed);
    let unread_backup = domain.backup_json().unwrap();
    domain
        .update_manual_progress(|state| {
            state.progress[0].completed = true;
            state.progress[0].last_read_at = 300;
            Ok(())
        })
        .unwrap();
    domain.restore_json(&unread_backup).unwrap();
    assert!(
        !domain.snapshot().unwrap().progress[0].completed,
        "backup restore carries explicit completion intent"
    );
    let bookmark_domain = sync::domain("bookmarks");
    let owner = nova_sync::local_store().unwrap();
    let bookmark_key = owner.lock().unwrap().records(&bookmark_domain)[0].0.clone();
    domain
        .update(|state| {
            state.bookmarks[0].note = "edited".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        owner.lock().unwrap().records(&bookmark_domain)[0].0,
        bookmark_key
    );
    let stale = owner.lock().unwrap().clone();
    domain
        .update(|state| {
            state.bookmarks.clear();
            state.library.retain(|e| e.series != first.id);
            state.history.clear();
            Ok(())
        })
        .unwrap();
    domain
        .update_checkpoint(|state| {
            let p = state.progress[0].clone();
            state.set_progress(p);
            Ok(())
        })
        .unwrap();
    exchange(&stale);
    sync::apply_remote(&domain).unwrap();
    let snapshot = domain.snapshot().unwrap();
    assert!(snapshot.bookmarks.is_empty());
    assert!(snapshot.history.is_empty());
    assert_eq!(snapshot.library.len(), 1);
    assert_eq!(snapshot.library[0].series, second.id);
    domain
        .update(|state| {
            state
                .series
                .iter_mut()
                .find(|s| s.id == first.id)
                .unwrap()
                .title = "refreshed".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(domain.snapshot().unwrap().library.len(), 1);
    let before = owner.lock().unwrap().digest();
    domain.update(|_| Ok(())).unwrap();
    assert_eq!(
        owner.lock().unwrap().digest(),
        before,
        "unchanged local snapshots must remain idle"
    );
    sync::apply_remote(&domain).unwrap();
    assert_eq!(
        owner.lock().unwrap().digest(),
        before,
        "projection must not echo"
    );
    let reloaded = nova_sync::Store::try_load().unwrap();
    assert_eq!(reloaded.digest(), before);
    assert!(
        reloaded
            .record(&bookmark_domain, &bookmark_key)
            .unwrap()
            .is_deleted()
    );
    assert_eq!(
        nova_storage::get_str(STATE_KEY).unwrap(),
        serde_json::to_string(&domain.snapshot().unwrap()).unwrap()
    );
    nova_storage::try_set_str(
        "reader:test:sync-digest",
        &serde_json::to_string(&before).unwrap(),
    )
    .unwrap();
}

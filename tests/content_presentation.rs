use nova_reader::content_presentation::{Presentation, Settings, UserPreference};
use reader_domain::SeriesId;
use std::process::Command;

#[test]
fn presentation_survives_restart_and_offline_without_changing_explicit_choices() {
    const PHASE: &str = "READER_PRESENTATION_PHASE";
    const DIRECTORY: &str = "READER_PRESENTATION_DIRECTORY";
    if let Ok(phase) = std::env::var(PHASE) {
        let a = SeriesId::new("source-a", "same-local");
        let b = SeriesId::new("source-b", "same-local");
        nova_storage::init_at(std::path::Path::new(&std::env::var(DIRECTORY).unwrap()));
        match phase.as_str() {
            "write" => {
                assert_eq!(
                    Settings::load(&a).unwrap().effective(),
                    Presentation::Reader
                );
                let services = nova_reader::services::Services::open(std::path::Path::new(
                    &std::env::var(DIRECTORY).unwrap(),
                ))
                .unwrap();
                let dto = serde_json::from_value(serde_json::json!({
                    "id": "same-local", "title": "Gallery", "presentation": "gallery"
                }))
                .unwrap();
                let domain = services.series("source-a", dto);
                assert_eq!(domain.id, a);
                assert!(
                    serde_json::to_value(domain)
                        .unwrap()
                        .get("presentation")
                        .is_none()
                );
                // Listing conversion captures the hint before any details fetch.
                assert_eq!(
                    Settings::load(&a).unwrap().effective(),
                    Presentation::Gallery
                );
                let backup = services.domain.backup_json().unwrap();
                Settings::set_user(&a, UserPreference::Reader).unwrap();
                Settings::record_hint(&b, Some("gallery")).unwrap();
                assert_eq!(services.domain.backup_json().unwrap(), backup);
                let setting_key = format!(
                    "reader:content-presentation:{}",
                    serde_json::to_string(&a).unwrap()
                );
                assert!(!nova_reader::sync_settings::supported(
                    &setting_key,
                    &serde_json::json!({"user":"Gallery"})
                ));
                assert!(!nova_reader::sync::handles_projection_domain(
                    &nova_reader::sync::domain("content_presentation")
                ));
                let concurrent = SeriesId::new("source", "concurrent");
                std::thread::scope(|scope| {
                    scope
                        .spawn(|| Settings::set_user(&concurrent, UserPreference::Reader).unwrap());
                    scope.spawn(|| Settings::record_hint(&concurrent, Some("gallery")).unwrap());
                });
                let latest = Settings::load(&concurrent).unwrap();
                assert_eq!(latest.user, UserPreference::Reader);
                assert_eq!(latest.last_known_addon_hint.as_deref(), Some("gallery"));
            }
            "offline" => {
                // No source installation, fetch, domain migration or chapter is needed.
                let saved = Settings::load(&a).unwrap();
                assert_eq!(saved.user, UserPreference::Reader);
                assert_eq!(saved.last_known_addon_hint.as_deref(), Some("gallery"));
                assert_eq!(saved.effective(), Presentation::Reader);
                assert_eq!(
                    Settings::load(&b).unwrap().effective(),
                    Presentation::Gallery
                );
                assert_eq!(
                    Settings::record_hint(&a, Some("future-mode"))
                        .unwrap()
                        .effective(),
                    Presentation::Reader
                );
                assert_eq!(
                    Settings::set_user(&a, UserPreference::Default)
                        .unwrap()
                        .effective(),
                    Presentation::Reader
                );
                Settings::set_user(&a, UserPreference::Gallery).unwrap();
                assert_eq!(
                    Settings::record_hint(&a, None).unwrap().effective(),
                    Presentation::Gallery
                );
                assert_eq!(
                    Settings::record_hint(&b, None).unwrap().effective(),
                    Presentation::Reader
                );
            }
            "reload" => {
                assert_eq!(Settings::load(&a).unwrap().user, UserPreference::Gallery);
                assert_eq!(
                    Settings::load(&b).unwrap().effective(),
                    Presentation::Reader
                );
                let invalid = SeriesId::new("invalid", "same-local");
                let key = format!(
                    "reader:content-presentation:{}",
                    serde_json::to_string(&invalid).unwrap()
                );
                for raw in [
                    "broken",
                    r#"{"schema_version":999,"settings":{"user":"Gallery","last_known_addon_hint":"gallery"}}"#,
                ] {
                    nova_storage::try_set_str(&key, raw).unwrap();
                    assert_eq!(Settings::load(&invalid).unwrap(), Settings::default());
                    assert_eq!(
                        nova_storage::try_get_str(&key).unwrap().as_deref(),
                        Some(raw)
                    );
                }
            }
            _ => panic!("unknown phase"),
        }
        return;
    }
    let root = tempfile::tempdir().unwrap();
    for phase in ["write", "offline", "reload"] {
        assert!(
            Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("presentation_survives_restart_and_offline_without_changing_explicit_choices")
                .env(PHASE, phase)
                .env(DIRECTORY, root.path())
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn source_hint_is_optional_forward_compatible_and_not_a_domain_field() {
    for (hint, expected) in [
        (None, None),
        (Some("gallery"), Some("gallery")),
        (Some("future-mode"), None),
    ] {
        let mut value = serde_json::json!({"id":"one-shot","title":"A single chapter"});
        if let Some(hint) = hint {
            value["presentation"] = hint.into();
        }
        let dto: reader_sources::Series = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(dto.presentation.as_deref(), expected);
        let settings = Settings {
            user: UserPreference::Default,
            last_known_addon_hint: dto.presentation,
        };
        assert_eq!(
            settings.effective(),
            if hint == Some("gallery") {
                Presentation::Gallery
            } else {
                Presentation::Reader
            }
        );
        // The previous reader DTO ignores this additive field.
        #[derive(serde::Deserialize)]
        struct OldSeries {
            id: String,
            title: String,
        }
        let old: OldSeries = serde_json::from_value(value).unwrap();
        assert_eq!(old.id, "one-shot");
        assert_eq!(old.title, "A single chapter");
    }
}

#[test]
fn malformed_and_oversized_hints_do_not_reject_source_entries() {
    for hint in [
        serde_json::Value::Null,
        serde_json::json!(42),
        serde_json::json!({"future":"gallery"}),
        serde_json::json!(["gallery"]),
        serde_json::json!("x".repeat(100_000)),
    ] {
        let dto: reader_sources::Series = serde_json::from_value(serde_json::json!({
            "id":"entry", "title":"Entry", "presentation":hint
        }))
        .unwrap();
        assert_eq!(dto.presentation, None);
    }
}

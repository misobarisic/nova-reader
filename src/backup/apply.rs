//! Committing a previewed restore as one durable transaction, and undoing it.
//!
//! Under the domain lock and the record owner, every target's records, rows and
//! the recovery receipt are staged on a copy of the owner and saved once; the
//! live owner is replaced only if that save succeeds, so a failed restore
//! leaves no trace. What cannot be part of the database transaction (installing
//! addon packages, addon configuration files) follows the commit and reports
//! its own failures without undoing the restore.
use super::{
    archive::{Archive, DEVICE, Manifest, Payload, Profile},
    components::{self, Rows},
    plan::{self, Choice, Opened, Preview, TargetPlan},
    profile_rows, revision,
};
use crate::services::Services;
use nova_reader_ui::backend_text as text;
use reader_domain::UpdateKind;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const RECOVERY_PREFIX: &str = "backup:recovery:v1:";
/// Recovery receipts kept, newest last.
const KEEP: usize = 3;
const STATE_COMPONENTS: &[&str] = &["library", "categories", "progress", "history", "bookmarks"];

#[derive(Clone, Serialize, Deserialize)]
struct RecoveryComponent {
    target: String,
    name: String,
    before: Payload,
    after: Payload,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    id: String,
    checksum: String,
    created_at: u64,
    choices: Vec<Choice>,
    components: Vec<RecoveryComponent>,
}
fn sequence(key: &str) -> u64 {
    key.rsplit_once('-')
        .and_then(|(_, sequence)| sequence.parse().ok())
        .unwrap_or(0)
}

/// What a committed restore tells its caller.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    /// The profiles that received data.
    pub targets: Vec<String>,
    /// Steps after the commit that did not complete.
    pub warnings: Vec<String>,
}

fn invalid(message: impl ToString) -> reader_domain::Error {
    reader_domain::Error::Invalid(message.to_string())
}

/// Commit `preview`. It fails without effect if the stores changed since the
/// preview was built.
pub fn apply(services: &Services, preview: &Preview) -> Result<Applied, String> {
    nova_sync::flush_persistence().map_err(|e| e.to_string())?;
    crate::sync::apply_remote(&services.domain).map_err(|e| e.to_string())?;
    let dev = nova_sync::local_device().map_err(|e| e.to_string())?;
    let handle = nova_sync::local_store().map_err(|e| e.to_string())?;
    let selected_profile = reader_domain::profile::selected();

    services
        .domain
        .exclusive(|domain| {
            let mut live = handle.lock().unwrap_or_else(|e| e.into_inner());
            // Revalidate under the locks: the preview is exact only for the
            // revisions it was built against.
            let existing: Vec<&String> = preview
                .choices
                .iter()
                .filter(|choice| choice.source != DEVICE && !choice.create)
                .map(|choice| &choice.target)
                .collect();
            let mut prefixes: Vec<String> = existing
                .iter()
                .map(|id| format!("profile:{id}:"))
                .collect();
            prefixes.extend(super::DEVICE_ROW_PREFIXES.iter().map(|p| (*p).to_owned()));
            prefixes.push(RECOVERY_PREFIX.into());
            let all: BTreeMap<String, String> = nova_storage::try_scan_prefixes(&prefixes)?
                .into_iter()
                .collect();
            let stale = || {
                invalid(text::tr(
                    "The data changed while the preview was open. Rebuild the preview before importing.",
                ))
            };
            for choice in preview.choices.iter().filter(|c| c.source != DEVICE) {
                plan::check_owner(&choice.target, &live, choice.create).map_err(invalid)?;
                let rows = if choice.create {
                    Rows::new()
                } else {
                    profile_rows(&choice.target, &all)
                };
                if preview.revisions.get(&choice.target)
                    != Some(&revision(&choice.target, &rows, &live))
                {
                    return Err(stale());
                }
            }
            if let Some(expected) = &preview.device_revision {
                let snapshot_rows: Rows = all
                    .iter()
                    .filter(|(key, _)| !key.starts_with("profile:") && !key.starts_with(RECOVERY_PREFIX))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                if &super::device_revision(services, &snapshot_rows).map_err(invalid)? != expected {
                    return Err(stale());
                }
            }

            let mut staged = live.clone();
            let device_selected_documents = preview
                .device
                .as_ref()
                .map(|plan| !plan.synced_documents.is_empty())
                .unwrap_or(false);
            let selected_before = if device_selected_documents {
                Some(
                    reader_domain::profile::pinned(&selected_profile, || {
                        crate::sync_settings::snapshot_fields(&staged)
                    })
                    .map_err(invalid)?,
                )
            } else {
                None
            };
            let mut receipt_components = Vec::new();
            for choice in preview.choices.iter().filter(|c| c.source != DEVICE) {
                let plan = &preview.plans[&choice.target];
                if choice.create {
                    staged.set(
                        crate::profiles::DOMAIN,
                        &choice.target,
                        Some(serde_json::json!({"name": choice.name.trim()}).to_string()),
                        0,
                        dev,
                    );
                }
                let final_state = reader_domain::profile::pinned(&choice.target, || {
                    stage_profile(&mut staged, dev, domain, &choice.target, plan)
                })
                .map_err(invalid)?;
                for (name, before) in &plan.before {
                    let after = match (&final_state, STATE_COMPONENTS.contains(&name.as_str())) {
                        (Some(state), true) => {
                            components::capture_state(name, state).map_err(invalid)?
                        }
                        _ => plan.after[name].clone(),
                    };
                    receipt_components.push(RecoveryComponent {
                        target: choice.target.clone(),
                        name: name.clone(),
                        before: before.clone(),
                        after,
                    });
                }
            }
            if let Some(plan) = &preview.device {
                for (key, value) in &plan.kv {
                    staged.queue_extra(key, value.clone());
                }
                if let Some(open) = plan.open_last_profile {
                    let mut preferences: serde_json::Value = staged
                        .extra_value(super::DEVICE_PREFERENCES_KEY)
                        .map_err(invalid)?
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_else(|| serde_json::json!({}));
                    preferences["open_last_profile_on_startup"] = open.into();
                    staged.queue_extra(
                        super::DEVICE_PREFERENCES_KEY,
                        Some(preferences.to_string()),
                    );
                }
                if let Some(before) = &selected_before {
                    reader_domain::profile::pinned(&selected_profile, || {
                        crate::sync_settings::stage_import(
                            &mut staged,
                            dev,
                            before,
                            &plan.synced_documents,
                        )
                    })
                    .map_err(invalid)?;
                }
                for (name, before) in &plan.before {
                    receipt_components.push(RecoveryComponent {
                        target: DEVICE.into(),
                        name: name.clone(),
                        before: before.clone(),
                        after: plan.after[name].clone(),
                    });
                }
            }

            let id = format!(
                "{}-{}",
                super::now_ms(),
                nova_storage::try_allocate_local_sequence("backup_receipt", 1)?
            );
            let receipt = Receipt {
                id: id.clone(),
                checksum: preview.opened.archive.checksum.clone(),
                created_at: crate::services::now().max(0) as u64,
                choices: preview.choices.clone(),
                components: receipt_components,
            };
            staged.queue_extra(
                &format!("{RECOVERY_PREFIX}{id}"),
                Some(serde_json::to_string(&receipt)?),
            );
            let mut older: Vec<String> = all
                .keys()
                .filter(|key| key.starts_with(RECOVERY_PREFIX))
                .cloned()
                .collect();
            older.sort_by_key(|key| sequence(key));
            while older.len() >= KEEP {
                staged.queue_extra(&older.remove(0), None);
            }
            // A failed save drops the copy: nothing tentative can reach the
            // live owner or a later unrelated save.
            nova_sync::publish(&mut live, staged, |staged| staged.save()).map_err(invalid)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    nova_sync::request_local_sync();
    crate::content_presentation::reset();
    let mut applied = Applied {
        targets: preview
            .choices
            .iter()
            .filter(|choice| choice.source != DEVICE)
            .map(|choice| choice.target.clone())
            .collect(),
        warnings: Vec::new(),
    };
    after_commit(services, preview, &mut applied);
    Ok(applied)
}

/// Stage one profile target. Runs pinned to the target, because the settings
/// documents and the account records resolve their profile from the pin.
fn stage_profile(
    staged: &mut nova_sync::Store,
    dev: u64,
    domain: &reader_domain::Exclusive,
    target: &str,
    plan: &TargetPlan,
) -> nova_sync::Result<Option<reader_domain::ReaderState>> {
    let before_fields = crate::sync_settings::snapshot_fields(staged)?;
    let mut final_state = None;
    if let Some((_, next)) = &plan.state {
        let before = domain
            .read(target)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut next = next.clone();
        crate::sync::stage(
            staged,
            dev,
            target,
            &before,
            &mut next,
            UpdateKind::ManualProgress,
            false,
        )?;
        final_state = Some(next);
    }
    for (logical, value) in &plan.kv {
        staged.queue_extra(
            &reader_domain::profile::key_in(target, logical),
            value.clone(),
        );
    }
    for (series, settings) in &plan.presentation {
        crate::content_presentation::stage_import(staged, dev, target, series, settings.as_ref())?;
    }
    crate::sync_settings::stage_import(staged, dev, &before_fields, &plan.synced_documents)?;
    Ok(final_state)
}

/// The steps that follow the database commit. Each reports its own failure.
fn after_commit(services: &Services, preview: &Preview, applied: &mut Applied) {
    let mut warn = |what: &str, error: String| {
        applied.warnings.push(format!("{what}: {error}"));
    };
    if let Some(plan) = &preview.device {
        for (id, hash) in &plan.packages {
            let Some(bytes) = preview.opened.archive.packages.get(hash) else {
                continue;
            };
            let result = crate::sync_addons::local_change(services, || {
                let source = services
                    .sources
                    .install_zip(bytes)
                    .map_err(|e| e.to_string())?;
                services
                    .domain
                    .update(|state| {
                        state.sources.retain(|saved| saved.addon_id != source.id);
                        state.sources.push(reader_domain::SourceDescriptor {
                            id: source.id.clone(),
                            addon_id: source.id.clone(),
                            name: source.name.clone(),
                            language: source.language.clone(),
                            version: source.version.clone(),
                        });
                        Ok(())
                    })
                    .map_err(|e| e.to_string())
            });
            if let Err(error) = result {
                warn(
                    &format!("{} {id}", text::tr("Could not install addon")),
                    error,
                );
            }
        }
        if plan.kv.contains_key("reader:image-limits") {
            crate::image_limits::install();
        }
        if plan
            .kv
            .keys()
            .any(|key| key.starts_with(components::COMPRESSION_PREFIX))
        {
            crate::source_compression::reset_cache();
        }
    }
    for choice in preview.choices.iter().filter(|c| c.source != DEVICE) {
        let plan = &preview.plans[&choice.target];
        if plan.addon_settings.is_empty()
            && plan.addon_secrets.is_empty()
            && plan.addon_secrets_cleared.is_empty()
        {
            continue;
        }
        let registry = services
            .sources
            .with_profile(crate::profiles::registry_profile(&choice.target));
        let installed: BTreeSet<String> = registry
            .list()
            .map(|list| list.into_iter().map(|source| source.id).collect())
            .unwrap_or_default();
        let ids: BTreeSet<&String> = plan
            .addon_settings
            .keys()
            .chain(plan.addon_secrets.keys())
            .chain(plan.addon_secrets_cleared.iter())
            .collect();
        for id in ids {
            if !installed.contains(id) {
                continue;
            }
            let mut values = plan.addon_settings.get(id).cloned().unwrap_or_default();
            if let Some(secrets) = plan.addon_secrets.get(id) {
                values.extend(secrets.clone());
            }
            if plan.addon_secrets_cleared.contains(id)
                && let Ok(exported) = registry.export_settings(id)
            {
                for key in exported.secrets.keys() {
                    values.entry(key.clone()).or_insert_with(|| "".into());
                }
            }
            match registry.import_settings(id, &values) {
                Ok(0) => {}
                Ok(skipped) => warn(
                    &format!(
                        "{} {id}",
                        text::tr("Some addon settings are no longer supported by")
                    ),
                    skipped.to_string(),
                ),
                Err(error) => warn(
                    &format!("{} {id}", text::tr("Could not apply settings of")),
                    error.to_string(),
                ),
            }
        }
        if let Err(error) = crate::sync_addons::publish_profile(services, &choice.target) {
            warn(&text::tr("Could not publish addon settings"), error);
        }
    }
}

// ---------------------------------------------------------------- recovery

pub(crate) fn recovery_inventory() -> Result<Vec<(String, String)>, String> {
    let mut rows = nova_storage::try_scan_prefix(RECOVERY_PREFIX).map_err(|e| e.to_string())?;
    rows.sort_by_key(|(key, _)| sequence(key));
    Ok(rows)
}
fn latest_receipt() -> Result<Receipt, String> {
    let rows = recovery_inventory()?;
    let (_, raw) = rows
        .last()
        .ok_or_else(|| text::tr("There is no import to undo"))?;
    serde_json::from_str(raw).map_err(|e| e.to_string())
}
/// The preview of undoing the latest import: fields that still equal what it
/// wrote revert, later edits and unrelated new records stay.
pub fn undo_preview(services: &Services) -> Result<Preview, String> {
    let receipt = latest_receipt()?;
    let mut payloads = BTreeMap::new();
    let mut undo_after = BTreeMap::new();
    let mut profiles = Vec::new();
    let mut choices = receipt.choices.clone();
    for choice in &mut choices {
        choice.source = choice.target.clone();
        choice.create = false;
        choice.replace = true;
        // Import conflict grants are not Undo grants.
        choice.use_backup.clear();
        // Installed packages are left installed.
        choice.components.remove("packages");
        profiles.push(Profile {
            archive_profile_id: choice.target.clone(),
            source_profile_id: choice.target.clone(),
            name: choice.name.clone(),
            components: Vec::new(),
        });
    }
    for component in receipt.components {
        if component.name == "packages" {
            continue;
        }
        undo_after.insert(
            (component.target.clone(), component.name.clone()),
            component.after,
        );
        payloads.insert((component.target, component.name), component.before);
    }
    let archive = Archive {
        manifest: Manifest {
            format: super::archive::FORMAT.into(),
            version: 1,
            created_at: receipt.created_at,
            app_version: env!("CARGO_PKG_VERSION").into(),
            archive_id: format!("undo-{}", receipt.id),
            profiles,
            packages: Vec::new(),
        },
        payloads,
        checksum: receipt.checksum,
        unsupported: Vec::new(),
        undo_after: Some(undo_after),
        packages: BTreeMap::new(),
        undo_local: None,
    };
    let opened = Opened {
        archive,
        legacy: false,
    };
    // Undo always gets a fresh exact preview, bound to the revisions of now.
    plan::preview(services, Arc::new(opened), choices)
}

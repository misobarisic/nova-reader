//! Settings → Data: the backup page's choices on the UI thread, and its
//! exports, previews, imports and undo on workers. The archive logic lives in
//! `crate::backup`; this module only turns clicks into calls and results into
//! rows, and reconciles the screen after data changed under it.
use super::*;
use crate::{
    BackupBridge, BackupMapping, BackupOption,
    backup::{
        Selection,
        apply::{self, Applied},
        components::{self, Scope},
        plan::{self, Choice, Opened, Preview},
    },
    profiles,
};

/// How an archive profile is restored, as the page shows it.
#[derive(Clone)]
struct Mapping {
    source: String,
    name: String,
    included: bool,
    target: String,
    target_name: String,
    create: bool,
}

#[derive(Default)]
struct State {
    initialized: bool,
    export_profiles: BTreeSet<String>,
    export_components: BTreeSet<String>,
    export_device: BTreeSet<String>,
    opened: Option<Arc<Opened>>,
    file_label: String,
    mappings: Vec<Mapping>,
    components: BTreeSet<String>,
    device: BTreeSet<String>,
    use_backup: BTreeSet<String>,
    replace: bool,
    preview: Option<Arc<Preview>>,
    undo: bool,
    /// The recovery copies listed in the open deletion confirmation.
    reviewing: Vec<(String, String)>,
}

fn option(id: &str, label: &str, detail: &str, checked: bool) -> BackupOption {
    BackupOption {
        id: id.into(),
        label: label.into(),
        detail: detail.into(),
        checked,
    }
}
fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}
fn definitions(scope: Scope) -> impl Iterator<Item = &'static components::Def> {
    components::COMPONENTS
        .iter()
        .filter(move |def| def.scope == scope)
}

impl State {
    fn live_profiles() -> Vec<profiles::Profile> {
        profiles::list().unwrap_or_default()
    }
    /// Keep the export choices valid against the profiles that exist now.
    fn refresh_export(&mut self) {
        let live = Self::live_profiles();
        if !self.initialized {
            self.initialized = true;
            self.export_components = definitions(Scope::Profile)
                .map(|def| def.id.to_owned())
                .collect();
            self.export_device = definitions(Scope::Device)
                .filter(|def| def.everything)
                .map(|def| def.id.to_owned())
                .collect();
            self.export_profiles = live.iter().map(|p| p.id.clone()).collect();
        }
        self.export_profiles
            .retain(|id| live.iter().any(|profile| &profile.id == id));
    }
    fn selection(&self) -> Selection {
        Selection {
            profiles: self
                .export_profiles
                .iter()
                .map(|id| (id.clone(), self.export_components.clone()))
                .collect(),
            device: self.export_device.clone(),
        }
    }
    fn choices(&self) -> Vec<Choice> {
        let Some(opened) = &self.opened else {
            return Vec::new();
        };
        opened
            .archive
            .manifest
            .profiles
            .iter()
            .filter_map(|profile| {
                let source = &profile.archive_profile_id;
                let present = plan::present(opened, source);
                if source == crate::backup::archive::DEVICE {
                    let components: BTreeSet<String> =
                        self.device.intersection(&present).cloned().collect();
                    return Some(Choice {
                        included: true,
                        source: source.clone(),
                        target: source.clone(),
                        name: profile.name.clone(),
                        create: false,
                        use_backup: self.use_backup.intersection(&components).cloned().collect(),
                        components,
                        replace: self.replace,
                    });
                }
                let mapping = self.mappings.iter().find(|m| &m.source == source)?;
                let components: BTreeSet<String> =
                    self.components.intersection(&present).cloned().collect();
                Some(Choice {
                    included: mapping.included,
                    source: source.clone(),
                    target: mapping.target.clone(),
                    name: mapping.target_name.clone(),
                    create: mapping.create,
                    use_backup: self.use_backup.intersection(&components).cloned().collect(),
                    components,
                    replace: self.replace,
                })
            })
            .collect()
    }
    fn invalidate_preview(&mut self) {
        self.preview = None;
    }
}

fn publish(ui: &AppWindow, state: &State) {
    let bridge = ui.global::<BackupBridge>();
    let live = State::live_profiles();
    bridge.set_export_profiles(model(
        live.iter()
            .map(|profile| {
                option(
                    &profile.id,
                    &profile.name,
                    "",
                    state.export_profiles.contains(&profile.id),
                )
            })
            .collect(),
    ));
    let rows = |scope: Scope, selected: &BTreeSet<String>| {
        definitions(scope)
            .map(|def| {
                option(
                    def.id,
                    &text::tr(def.label),
                    &text::tr(def.detail),
                    selected.contains(def.id),
                )
            })
            .collect::<Vec<_>>()
    };
    bridge.set_export_components(model(rows(Scope::Profile, &state.export_components)));
    bridge.set_export_device(model(rows(Scope::Device, &state.export_device)));
    bridge.set_can_undo(apply::recovery_inventory().is_ok_and(|rows| !rows.is_empty()));
    bridge.set_importing(state.opened.is_some());
    bridge.set_replace(state.replace);
    bridge.set_undoing(state.undo);
    bridge.set_preview_ready(state.preview.is_some());
    bridge.set_file_label(state.file_label.clone().into());
    let Some(opened) = &state.opened else {
        bridge.set_profiles(model(Vec::new()));
        bridge.set_components(model(Vec::new()));
        bridge.set_device_components(model(Vec::new()));
        bridge.set_conflicts(model(Vec::new()));
        bridge.set_mappings(model(Vec::new()));
        bridge.set_preview_rows(model(Vec::new()));
        return;
    };
    let count = |source: &str| -> usize {
        opened
            .archive
            .payloads
            .iter()
            .filter(|((id, _), _)| id == source)
            .map(|(_, payload)| payload.records.len())
            .sum()
    };
    bridge.set_profiles(model(
        state
            .mappings
            .iter()
            .map(|m| {
                option(
                    &m.source,
                    &m.name,
                    &format!("{} {}", count(&m.source), text::tr("records")),
                    m.included,
                )
            })
            .collect(),
    ));
    bridge.set_mappings(model(
        state
            .mappings
            .iter()
            .filter(|m| m.included)
            .map(|m| BackupMapping {
                id: m.source.clone().into(),
                label: format!("{} →", m.name).into(),
                target: if m.create {
                    format!("{}: {}", text::tr("New profile"), m.target_name)
                } else {
                    format!("{}: {}", text::tr("Into profile"), m.target_name)
                }
                .into(),
            })
            .collect(),
    ));
    let present = |scope: Scope| -> BTreeSet<String> {
        opened
            .archive
            .payloads
            .keys()
            .filter(|(source, name)| {
                (source == crate::backup::archive::DEVICE) == (scope == Scope::Device)
                    && components::supported(name)
            })
            .map(|(_, name)| name.clone())
            .collect()
    };
    let list = |scope: Scope, selected: &BTreeSet<String>| {
        let present = present(scope);
        definitions(scope)
            .filter(|def| present.contains(def.id))
            .map(|def| {
                option(
                    def.id,
                    &text::tr(def.label),
                    &text::tr(def.detail),
                    selected.contains(def.id),
                )
            })
            .collect::<Vec<_>>()
    };
    bridge.set_components(model(list(Scope::Profile, &state.components)));
    bridge.set_device_components(model(list(Scope::Device, &state.device)));
    let wanted: BTreeSet<&String> = state.components.iter().chain(&state.device).collect();
    bridge.set_conflicts(model(
        components::COMPONENTS
            .iter()
            .filter(|def| wanted.contains(&def.id.to_owned()) && !components::is_files(def.id))
            .map(|def| {
                option(
                    def.id,
                    &text::tr(def.label),
                    "",
                    state.use_backup.contains(def.id),
                )
            })
            .collect(),
    ));
    bridge.set_preview_rows(model(
        state
            .preview
            .as_ref()
            .map(|preview| preview.lines().into_iter().map(Into::into).collect())
            .unwrap_or_default(),
    ));
}

fn toggle(set: &mut BTreeSet<String>, id: &str) {
    if !set.remove(id) {
        set.insert(id.to_owned());
    }
}

/// Run `work` on a profile-pinned worker with the page busy, then hand its
/// result to `done` on the event loop.
fn run<T: Send + 'static>(
    app: &Arc<Controller>,
    ui: &AppWindow,
    state: &Arc<Mutex<State>>,
    work: impl FnOnce(&Services) -> std::result::Result<T, String> + Send + 'static,
    done: impl FnOnce(&Arc<Controller>, &AppWindow, &mut State, T) + Send + 'static,
) {
    let bridge = ui.global::<BackupBridge>();
    if bridge.get_busy() {
        return;
    }
    bridge.set_busy(true);
    bridge.set_status("".into());
    // Settle the reading position and queued writes first: a backup must see
    // what the screen shows.
    app.flush();
    let services = app.services.clone();
    let (weak, app, state) = (ui.as_weak(), app.clone(), state.clone());
    profiles::spawn(move || {
        let result = work(&services);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let bridge = ui.global::<BackupBridge>();
            bridge.set_busy(false);
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            match result {
                Ok(value) => done(&app, &ui, &mut guard, value),
                Err(error) if text::is_user_cancel(&error) => {}
                Err(error) => bridge.set_status(text::error(&error).into()),
            }
            publish(&ui, &guard);
        });
    });
}

fn size_label(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Everything on screen and in memory that a restore may have changed.
fn refresh_after(app: &Arc<Controller>, ui: &AppWindow) {
    if let Err(error) = crate::reader_settings::init() {
        eprintln!("Nova Reader: could not read reader settings after a restore: {error}");
    }
    app.reset_profile_view(ui);
    app.publish_profiles(ui);
    crate::image_limits_ui::refresh(ui);
    app.refresh_synced(ui);
}

fn applied_message(applied: &Applied) -> String {
    let mut message = text::tr("Backup imported.");
    for warning in &applied.warnings {
        message.push_str(&format!("\n{warning}"));
    }
    message
}

fn finish_open(state: &mut State, opened: Opened, choices: Vec<Choice>, label: String) {
    state.mappings = choices
        .iter()
        .filter(|choice| choice.source != crate::backup::archive::DEVICE)
        .map(|choice| Mapping {
            source: choice.source.clone(),
            name: opened
                .archive
                .manifest
                .profiles
                .iter()
                .find(|p| p.archive_profile_id == choice.source)
                .map_or_else(|| choice.name.clone(), |p| p.name.clone()),
            included: true,
            target: choice.target.clone(),
            target_name: choice.name.clone(),
            create: choice.create,
        })
        .collect();
    state.components = choices
        .iter()
        .filter(|choice| choice.source != crate::backup::archive::DEVICE)
        .flat_map(|choice| choice.components.iter().cloned())
        .collect();
    state.device = choices
        .iter()
        .filter(|choice| choice.source == crate::backup::archive::DEVICE)
        .flat_map(|choice| choice.components.iter().cloned())
        .collect();
    state.use_backup.clear();
    state.replace = opened.legacy;
    state.undo = false;
    state.preview = None;
    state.file_label = label;
    state.opened = Some(Arc::new(opened));
}

fn start_export(
    app: &Arc<Controller>,
    ui: &AppWindow,
    state: &Arc<Mutex<State>>,
    destination: PathBuf,
    // Android stages the archive in the app cache and then copies it to the
    // document the user picks.
    staged: bool,
) {
    let selection = state.lock().unwrap_or_else(|e| e.into_inner()).selection();
    #[cfg(target_os = "android")]
    let weak = ui.as_weak();
    run(
        app,
        ui,
        state,
        move |services| crate::backup::export(services, &selection, &destination),
        move |_, ui, _, exported| {
            let size = size_label(exported.bytes);
            #[cfg(target_os = "android")]
            if staged {
                let staged_file = exported.path.clone();
                let cleanup = staged_file.clone();
                ui.global::<BackupBridge>().set_busy(true);
                crate::android_documents::export_backup(staged_file, move |result| {
                    let _ = std::fs::remove_file(cleanup);
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        let bridge = ui.global::<BackupBridge>();
                        bridge.set_busy(false);
                        bridge.set_status(
                            match result {
                                Ok(()) => format!("{} ({size})", text::tr("Backup saved.")),
                                Err(e) if text::is_user_cancel(&e) => String::new(),
                                Err(e) => text::error(&e),
                            }
                            .into(),
                        );
                    });
                });
                return;
            }
            let _ = staged;
            ui.global::<BackupBridge>()
                .set_status(format!("{} ({size})", text::tr("Backup saved.")).into());
        },
    );
}

fn start_open(app: &Arc<Controller>, ui: &AppWindow, state: &Arc<Mutex<State>>, path: PathBuf) {
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    run(
        app,
        ui,
        state,
        move |_| {
            let opened = plan::open(&path);
            // A copy made for this import (Android) is not needed once read.
            #[cfg(target_os = "android")]
            let _ = std::fs::remove_file(&path);
            let opened = opened?;
            let choices = plan::default_choices(&opened)?;
            Ok((opened, choices))
        },
        move |_, _, state, (opened, choices)| finish_open(state, opened, choices, label),
    );
}

fn start_preview(app: &Arc<Controller>, ui: &AppWindow, state: &Arc<Mutex<State>>) {
    let (opened, choices) = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(opened) = guard.opened.clone() else {
            return;
        };
        (opened, guard.choices())
    };
    run(
        app,
        ui,
        state,
        move |services| plan::preview(services, opened, choices),
        move |_, ui, state, preview| {
            let lines = preview.lines();
            state.preview = Some(Arc::new(preview));
            if lines.is_empty() {
                ui.global::<BackupBridge>()
                    .set_status(text::tr("Nothing would change.").into());
            }
        },
    );
}

fn confirm(ui: &AppWindow, kind: i32, message: String, label: &str) {
    let bridge = ui.global::<BackupBridge>();
    bridge.set_confirm_kind(kind);
    bridge.set_confirm_message(message.into());
    bridge.set_confirm_label(text::tr(label).into());
    bridge.set_confirm_open(true);
}

fn start_apply(app: &Arc<Controller>, ui: &AppWindow, state: &Arc<Mutex<State>>) {
    let preview = state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .preview
        .clone();
    let Some(preview) = preview else { return };
    run(
        app,
        ui,
        state,
        move |services| apply::apply(services, &preview),
        move |app, ui, state, applied| {
            state.preview = None;
            state.undo = false;
            refresh_after(app, ui);
            ui.global::<BackupBridge>()
                .set_status(applied_message(&applied).into());
        },
    );
}

pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) {
    let state = Arc::new(Mutex::new(State::default()));
    let bridge = ui.global::<BackupBridge>();
    macro_rules! wire {
        ($on:ident, |$app:ident, $ui:ident, $state:ident $(, $arg:ident)*| $body:expr) => {{
            let owner = Arc::downgrade(app);
            let weak = ui.as_weak();
            let shared = state.clone();
            bridge.$on(move |$($arg),*| {
                if let (Some($app), Some($ui)) = (owner.upgrade(), weak.upgrade()) {
                    let $state = &shared;
                    $body
                }
            });
        }};
    }
    wire!(on_opened, |_app, ui, state| {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard.refresh_export();
        publish(&ui, &guard);
    });
    wire!(on_export_preset, |_app, ui, state, preset| {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard.refresh_export();
        let profile = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect();
        let (profile_components, device): (BTreeSet<String>, BTreeSet<String>) = match preset {
            0 => (
                definitions(Scope::Profile)
                    .map(|d| d.id.to_owned())
                    .collect(),
                definitions(Scope::Device)
                    .filter(|d| d.everything)
                    .map(|d| d.id.to_owned())
                    .collect(),
            ),
            1 => (
                profile(&["library", "categories", "progress", "history", "bookmarks"]),
                BTreeSet::new(),
            ),
            _ => (
                profile(&[
                    "settings",
                    "title_settings",
                    "addons",
                    "addon_secrets",
                    "sync_overrides",
                ]),
                profile(&["packages", "device_settings"]),
            ),
        };
        guard.export_components = profile_components;
        guard.export_device = device;
        guard.export_profiles = State::live_profiles().into_iter().map(|p| p.id).collect();
        publish(&ui, &guard);
    });
    wire!(on_export_toggle, |_app, ui, state, kind, id| {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        match kind.as_str() {
            "profile" => toggle(&mut guard.export_profiles, &id),
            "component" => toggle(&mut guard.export_components, &id),
            "device" => toggle(&mut guard.export_device, &id),
            _ => {}
        }
        publish(&ui, &guard);
    });
    wire!(on_export_file, |app, ui, state| {
        {
            let guard = state.lock().unwrap_or_else(|e| e.into_inner());
            let selection = guard.selection();
            if selection.profiles.values().all(BTreeSet::is_empty) && selection.device.is_empty() {
                ui.global::<BackupBridge>()
                    .set_status(text::tr("Choose something to export.").into());
                return;
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            use crate::desktop_dialogs::PathRequest;
            let (app, state) = (app.clone(), state.clone());
            crate::desktop_dialogs::pick_path(&ui, PathRequest::BackupSave, move |ui, path| {
                start_export(&app, ui, &state, path, false)
            });
        }
        #[cfg(target_os = "android")]
        match crate::android_documents::cache_dir() {
            Ok(directory) => {
                let staged = directory.join(format!(
                    "nova-reader-backup-staged.{}",
                    crate::backup::EXTENSION
                ));
                start_export(&app, &ui, state, staged, true);
            }
            Err(error) => ui
                .global::<BackupBridge>()
                .set_status(text::error(&error).into()),
        }
    });
    wire!(on_import_file, |app, ui, state| {
        #[cfg(not(target_os = "android"))]
        {
            use crate::desktop_dialogs::PathRequest;
            let (app, state) = (app.clone(), state.clone());
            crate::desktop_dialogs::pick_path(&ui, PathRequest::BackupOpen, move |ui, path| {
                start_open(&app, ui, &state, path)
            });
        }
        #[cfg(target_os = "android")]
        {
            let (weak, app, state) = (ui.as_weak(), app.clone(), state.clone());
            crate::android_documents::pick_import(
                crate::android_documents::ImportKind::BackupFile,
                move |result| {
                    let _ = weak.upgrade_in_event_loop(move |ui| match result {
                        Ok(path) => start_open(&app, &ui, &state, path),
                        Err(e) if text::is_user_cancel(&e) => {}
                        Err(e) => ui
                            .global::<BackupBridge>()
                            .set_status(text::error(&e).into()),
                    });
                },
            );
        }
    });
    wire!(on_import_toggle, |_app, ui, state, kind, id| {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        match kind.as_str() {
            "profile" => {
                if let Some(mapping) = guard.mappings.iter_mut().find(|m| m.source == id.as_str()) {
                    mapping.included = !mapping.included;
                }
            }
            "component" => toggle(&mut guard.components, &id),
            "device" => toggle(&mut guard.device, &id),
            "conflict" => toggle(&mut guard.use_backup, &id),
            _ => {}
        }
        guard.invalidate_preview();
        publish(&ui, &guard);
    });
    wire!(on_cycle_target, |_app, ui, state, source| {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        let live = State::live_profiles();
        let taken: BTreeSet<String> = guard
            .mappings
            .iter()
            .filter(|m| m.included && m.source != source.as_str())
            .map(|m| m.target.clone())
            .collect();
        let Some(index) = guard
            .mappings
            .iter()
            .position(|m| m.source == source.as_str())
        else {
            return;
        };
        // Existing profiles in order, then "new", then around again.
        let mut options: Vec<(String, String, bool)> = live
            .iter()
            .map(|p| (p.id.clone(), p.name.clone(), false))
            .collect();
        let current = options
            .iter()
            .position(|(id, _, _)| id == &guard.mappings[index].target);
        let name = guard.mappings[index].name.clone();
        let fresh = if guard.mappings[index].create {
            guard.mappings[index].target.clone()
        } else {
            match profiles::allocate_id() {
                Ok(id) => id,
                Err(error) => {
                    ui.global::<BackupBridge>()
                        .set_status(text::error(&error).into());
                    return;
                }
            }
        };
        options.push((fresh, name, true));
        let start = if guard.mappings[index].create {
            options.len() - 1
        } else {
            current.unwrap_or(options.len() - 1)
        };
        for step in 1..=options.len() {
            let (id, name, create) = options[(start + step) % options.len()].clone();
            if taken.contains(&id) {
                continue;
            }
            let mapping = &mut guard.mappings[index];
            mapping.target = id;
            mapping.target_name = name;
            mapping.create = create;
            break;
        }
        guard.invalidate_preview();
        publish(&ui, &guard);
    });
    wire!(on_mode_changed, |_app, ui, state, replace| {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard.replace = replace;
        guard.invalidate_preview();
        publish(&ui, &guard);
    });
    wire!(on_build_preview, |app, ui, state| {
        let undoing = state.lock().unwrap_or_else(|e| e.into_inner()).undo;
        if undoing {
            start_undo_preview(&app, &ui, state);
        } else {
            start_preview(&app, &ui, state);
        }
    });
    wire!(on_apply_import, |_app, ui, state| {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.preview.is_some() {
            let undo = guard.undo;
            drop(guard);
            confirm(
                &ui,
                1,
                text::tr(if undo {
                    "Undo this import? Changes made by the undo, including removals, sync to your paired devices."
                } else {
                    "Import these changes? They sync to your paired devices, and any removals do too. You can undo the last three imports."
                }),
                if undo { "Undo" } else { "Import" },
            );
        }
    });
    wire!(on_undo, |app, ui, state| start_undo_preview(
        &app, &ui, state
    ));
    wire!(on_delete_recovery, |_app, ui, state| {
        match apply::recovery_inventory() {
            Ok(rows) if !rows.is_empty() => {
                let lines = apply::recovery_lines(&rows).join("\n");
                state.lock().unwrap_or_else(|e| e.into_inner()).reviewing = rows;
                confirm(
                    &ui,
                    2,
                    format!(
                        "{}\n{lines}",
                        text::tr(
                            "Delete these recovery copies permanently? You will no longer be able to undo these imports."
                        ),
                    ),
                    "Delete",
                );
            }
            Ok(_) => {}
            Err(error) => ui
                .global::<BackupBridge>()
                .set_status(text::error(&error).into()),
        }
    });
    wire!(on_confirm_choice, |app, ui, state, choice| {
        let kind = ui.global::<BackupBridge>().get_confirm_kind();
        if choice != 0 {
            return;
        }
        match kind {
            1 => start_apply(&app, &ui, state),
            2 => {
                let reviewed =
                    std::mem::take(&mut state.lock().unwrap_or_else(|e| e.into_inner()).reviewing);
                let bridge = ui.global::<BackupBridge>();
                match apply::delete_reviewed_recovery(&app.services, &reviewed) {
                    Ok(()) => bridge.set_status(text::tr("Recovery copies deleted.").into()),
                    Err(error) => bridge.set_status(text::error(&error).into()),
                }
                publish(&ui, &state.lock().unwrap_or_else(|e| e.into_inner()));
            }
            _ => {}
        }
    });
}

fn start_undo_preview(app: &Arc<Controller>, ui: &AppWindow, state: &Arc<Mutex<State>>) {
    // An undo is its own import: its choices come from the recovery copy, and
    // the page shows that copy's profiles rather than the open file's.
    let use_backup = state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .use_backup
        .clone();
    run(
        app,
        ui,
        state,
        move |services| {
            let mut preview = apply::undo_preview(services)?;
            if !use_backup.is_empty() {
                // A fresh review with the user's explicit grants.
                let mut choices = preview.choices.clone();
                for choice in &mut choices {
                    choice.use_backup = use_backup
                        .intersection(&choice.components)
                        .cloned()
                        .collect();
                }
                preview = plan::preview(services, preview.opened.clone(), choices)?;
            }
            Ok(preview)
        },
        move |_, _, state, preview| {
            state.undo = true;
            state.opened = Some(preview.opened.clone());
            state.file_label = text::tr("Undo of the last import");
            state.mappings = preview
                .choices
                .iter()
                .filter(|choice| choice.source != crate::backup::archive::DEVICE)
                .map(|choice| Mapping {
                    source: choice.source.clone(),
                    name: choice.name.clone(),
                    included: true,
                    target: choice.target.clone(),
                    target_name: choice.name.clone(),
                    create: false,
                })
                .collect();
            state.components = preview
                .choices
                .iter()
                .filter(|choice| choice.source != crate::backup::archive::DEVICE)
                .flat_map(|choice| choice.components.iter().cloned())
                .collect();
            state.device = preview
                .choices
                .iter()
                .filter(|choice| choice.source == crate::backup::archive::DEVICE)
                .flat_map(|choice| choice.components.iter().cloned())
                .collect();
            state.replace = true;
            state.preview = Some(Arc::new(preview));
        },
    );
}

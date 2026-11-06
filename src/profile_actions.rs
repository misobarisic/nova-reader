//! Profile picker actions on the UI thread: select, create, rename, delete and
//! the startup picker. Durable identity and storage live in `crate::profiles`;
//! this module only reconciles the screen with a selection change.
use super::*;
use crate::{ProfileRow, ProfilesBridge, profiles};

/// The untouched default is stored as "Default" and translated when shown.
fn display_name(profile: &profiles::Profile) -> String {
    if profile.id == profiles::DEFAULT_ID && profile.name == profiles::DEFAULT_NAME {
        text::tr("Default")
    } else {
        profile.name.clone()
    }
}
fn color(rgb: u32) -> slint::Color {
    slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}
fn rows(list: &[profiles::Profile], active: &str) -> Vec<ProfileRow> {
    list.iter()
        .map(|profile| {
            let name = display_name(profile);
            let (background, foreground) = profiles::avatar(&profile.id);
            ProfileRow {
                id: profile.id.clone().into(),
                initial: profiles::initial(&name).into(),
                name: name.into(),
                active: profile.id == active,
                avatar_background: color(background),
                avatar_foreground: color(foreground),
            }
        })
        .collect()
}

impl Controller {
    /// Refresh the picker model from the account directory.
    pub(super) fn publish_profiles(&self, ui: &AppWindow) {
        let bridge = ui.global::<ProfilesBridge>();
        match profiles::list() {
            Ok(list) => {
                let active = reader_domain::profile::selected();
                let name = list
                    .iter()
                    .find(|profile| profile.id == active)
                    .map(display_name)
                    .unwrap_or_default();
                bridge.set_active_name(name.into());
                bridge.set_profiles(ModelRc::new(VecModel::from(rows(&list, &active))));
            }
            Err(error) => bridge.set_error(text::tr(&error).into()),
        }
        bridge.set_remember_last(profiles::device_preferences().open_last_profile_on_startup);
    }

    /// Whether navigation must stay on the startup picker.
    pub(super) fn profile_selection_required(&self, ui: &AppWindow) -> bool {
        ui.global::<ProfilesBridge>().get_require_selection()
    }

    /// Reset everything the previous profile put on screen or in memory, then
    /// reload the new profile's personal preferences.
    fn reset_profile_view(self: &Arc<Self>, ui: &AppWindow) {
        {
            let mut view = self.view();
            // A late refresh started before the switch is recognized as stale.
            let revision = view.download_revision.wrapping_add(1);
            *view = View {
                download_revision: revision,
                source_browser: crate::source_browser::SourceBrowser::load(),
                library_navigation: library_navigation::State::load(),
                ..View::default()
            };
        }
        *self
            .capture_originals
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Default::default();
        self.metadata_refresh.reset();
        ui.set_library_query("".into());
        ui.set_library_scroll_y(0.);
        ui.set_library_restore_index(-1);
        ui.set_source_catalog_open(false);
        ui.set_pages(ModelRc::default());
        ui.set_total_ratio(0.0);
        ui.set_reader_opening(false);
        ui.set_busy(false);
        ui.set_status("".into());
        if let Err(error) = crate::appearance::reload(ui) {
            ui.set_status(format!("{}: {error}", text::tr("Could not read appearance")).into());
        }
        crate::reading_preferences::reload(ui);
        if let Err(error) = crate::sync_settings::refresh_ui(ui) {
            ui.set_status(format!("{}: {error}", text::tr("Could not read settings")).into());
        }
        if let Ok(state) = self.services.domain.snapshot() {
            crate::download_cleanup::refresh(ui, &state);
        }
    }

    /// Switch to `id`, or confirm the current profile at the startup picker.
    pub(super) fn select_profile(self: &Arc<Self>, ui: &AppWindow, id: &str) {
        let bridge = ui.global::<ProfilesBridge>();
        bridge.set_error("".into());
        let current = reader_domain::profile::selected();
        // Persist reading state and stop everything started for the old profile.
        self.flush();
        self.invalidate();
        if id != current
            && let Err(error) = profiles::activate(&self.services.domain, id)
        {
            bridge.set_error(text::tr(&error).into());
            self.publish_profiles(ui);
            return;
        }
        bridge.set_require_selection(false);
        self.reset_profile_view(ui);
        self.publish_profiles(ui);
        self.navigate(ui, 0);
        if id != current {
            self.after_profile_switch(ui);
        }
    }

    /// Work that follows a switch: addon intent, pending remote records and
    /// downloads for the new profile's library.
    fn after_profile_switch(self: &Arc<Self>, ui: &AppWindow) {
        self.refresh_download_states(ui);
        let services = self.services.clone();
        let weak = ui.as_weak();
        let app = self.clone();
        // Pinned to the new profile: its addon settings and pending records.
        profiles::spawn(move || {
            let published = crate::sync_addons::publish(&services);
            let projected = nova_sync::flush_persistence()
                .map_err(|e| e.to_string())
                .and_then(|_| {
                    crate::sync::apply_remote(&services.domain).map_err(|e| e.to_string())
                });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if let Err(error) = published.and(projected) {
                    ui.set_status(
                        format!("{}: {error}", text::tr("Could not apply synced changes")).into(),
                    );
                }
                if let Err(error) = crate::sync_settings::refresh_ui(&ui) {
                    ui.set_status(
                        format!("{}: {error}", text::tr("Could not apply synced settings")).into(),
                    );
                }
                app.refresh_synced(&ui);
            });
        });
    }

    fn create_profile(&self, ui: &AppWindow, name: &str) {
        let bridge = ui.global::<ProfilesBridge>();
        bridge.set_error("".into());
        if let Err(error) = profiles::create(name) {
            bridge.set_error(text::tr(&error).into());
        }
        self.publish_profiles(ui);
    }

    fn rename_profile(&self, ui: &AppWindow, id: &str, name: &str) {
        let bridge = ui.global::<ProfilesBridge>();
        bridge.set_error("".into());
        if let Err(error) = profiles::rename(id, name) {
            bridge.set_error(text::tr(&error).into());
        }
        self.publish_profiles(ui);
    }

    fn delete_profile(self: &Arc<Self>, ui: &AppWindow, id: &str) {
        let bridge = ui.global::<ProfilesBridge>();
        bridge.set_error("".into());
        let survivors = match profiles::list() {
            Ok(list) => list,
            Err(error) => {
                bridge.set_error(text::tr(&error).into());
                return;
            }
        };
        if survivors.len() < 2 {
            bridge.set_error(text::tr("At least one profile must remain.").into());
            return;
        }
        if reader_domain::profile::selected() == id {
            // Never delete the profile on screen: move to a survivor first.
            let Some(next) = survivors.iter().find(|profile| profile.id != id) else {
                return;
            };
            self.select_profile(ui, &next.id.clone());
            if reader_domain::profile::selected() == id {
                return;
            }
            // The user is looking at the picker's page again afterwards.
            self.navigate(ui, 24);
        }
        if let Err(error) = profiles::delete(id) {
            bridge.set_error(text::tr(&error).into());
        }
        self.publish_profiles(ui);
    }

    /// A peer's directory change or deletion arrived: refresh the picker, and if
    /// the selected profile no longer exists, fall back to a survivor and ask.
    pub(super) fn reconcile_profiles(self: &Arc<Self>, ui: &AppWindow) {
        let list = match profiles::list() {
            Ok(list) => list,
            Err(error) => {
                ui.global::<ProfilesBridge>()
                    .set_error(text::tr(&error).into());
                return;
            }
        };
        let current = reader_domain::profile::selected();
        if !list.is_empty() && !list.iter().any(|profile| profile.id == current) {
            let next = list[0].id.clone();
            self.select_profile(ui, &next);
            let bridge = ui.global::<ProfilesBridge>();
            bridge.set_require_selection(true);
            bridge.set_error(
                text::tr("The profile you were using was deleted on another device.").into(),
            );
            self.navigate(ui, 24);
        }
        self.publish_profiles(ui);
    }
}

pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) {
    let bridge = ui.global::<ProfilesBridge>();
    macro_rules! wire {
        ($on:ident, |$app:ident, $ui:ident $(, $arg:ident)*| $body:expr) => {{
            let owner = Arc::downgrade(app);
            let weak = ui.as_weak();
            bridge.$on(move |$($arg),*| {
                if let (Some($app), Some($ui)) = (owner.upgrade(), weak.upgrade()) {
                    $body
                }
            });
        }};
    }
    wire!(on_select_profile, |app, ui, id| app
        .select_profile(&ui, &id));
    wire!(on_create_profile, |app, ui, name| app
        .create_profile(&ui, &name));
    wire!(on_rename_profile, |app, ui, id, name| app
        .rename_profile(&ui, &id, &name));
    wire!(on_delete_profile, |app, ui, id| app
        .delete_profile(&ui, &id));
    wire!(on_remember_last_changed, |app, ui, enabled| {
        let _ = &app;
        let bridge = ui.global::<ProfilesBridge>();
        match profiles::set_open_last_profile(enabled) {
            Ok(()) => {
                bridge.set_error("".into());
                bridge.set_remember_last(enabled);
            }
            Err(error) => bridge.set_error(text::tr(&error).into()),
        }
    });
    app.publish_profiles(ui);
    // With "open last profile" off, the picker owns the first screen.
    if !profiles::device_preferences().open_last_profile_on_startup {
        bridge.set_require_selection(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::{LibraryEntry, Series, SeriesId};

    fn add_title(app: &Controller, local: &str) {
        let id = SeriesId::new("source", local);
        app.services
            .domain
            .update(|state| {
                state.series.push(Series {
                    publication_status: Default::default(),
                    id: id.clone(),
                    title: local.into(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                });
                state.library.push(LibraryEntry {
                    series: id,
                    categories: Default::default(),
                    preferences: None,
                    bookmarked: false,
                    added_at: 1,
                    reading_status: Default::default(),
                });
                Ok(())
            })
            .unwrap();
    }

    /// The selection, the screen and the sync-visible directory change together.
    #[test]
    #[ignore = "isolated process for the single application directory"]
    fn profile_switching_child() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        let (images, _receiver) = mpsc::channel();
        let app = Arc::new(Controller {
            progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
                services.domain.clone(),
            )
            .unwrap(),
            artwork: crate::artwork::ArtworkLoader::new(
                services.clone(),
                ui.as_weak(),
                dir.path().join("posters"),
            ),
            services,
            chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
                dir.path().join("cache"),
            )),
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::new(View::default()),
            generation: AtomicU64::new(0),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        crate::appearance::init(&ui).unwrap();
        crate::reading_preferences::init(&ui).unwrap();
        ui.on_navigate({
            let owner = Arc::downgrade(&app);
            let weak = ui.as_weak();
            move |section| {
                if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                    app.navigate(&ui, section);
                }
            }
        });
        wire(&app, &ui);
        let bridge = ui.global::<ProfilesBridge>();
        let names = |ui: &AppWindow| {
            ui.global::<ProfilesBridge>()
                .get_profiles()
                .iter()
                .map(|row| row.name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&ui), ["Default"]);
        assert_eq!(bridge.get_active_name(), "Default");

        // Library entry in the first profile.
        add_title(&app, "alpha");
        app.render(&ui);
        assert_eq!(ui.get_items().row_count(), 1);

        bridge.invoke_create_profile("  Second ".into());
        assert_eq!(bridge.get_error(), "");
        assert_eq!(names(&ui), ["Default", "Second"]);
        let second = bridge
            .get_profiles()
            .iter()
            .find(|row| row.name == "Second")
            .unwrap()
            .id
            .to_string();
        bridge.invoke_create_profile("   ".into());
        assert_ne!(bridge.get_error(), "", "an empty name is refused");

        // Switching re-renders the Library for the new profile and resets search.
        ui.set_library_query("stale search".into());
        app.navigate(&ui, 24);
        assert_eq!(ui.get_section(), 24);
        bridge.invoke_select_profile(second.clone().into());
        assert_eq!(reader_domain::profile::selected(), second);
        assert_eq!(bridge.get_active_name(), "Second");
        assert_eq!(ui.get_section(), 0);
        assert_eq!(ui.get_items().row_count(), 0);
        assert_eq!(ui.get_library_query(), "");
        add_title(&app, "beta");
        app.render(&ui);
        assert_eq!(ui.get_items().row_count(), 1);
        bridge.invoke_select_profile("reader".into());
        assert_eq!(ui.get_items().row_count(), 1);
        assert_eq!(
            app.services.domain.snapshot().unwrap().library[0]
                .series
                .local_id,
            "alpha"
        );

        // The startup picker blocks navigation until a profile is chosen.
        bridge.set_require_selection(true);
        app.navigate(&ui, 24);
        assert_eq!(ui.get_section(), 24);
        app.navigate(&ui, 0);
        assert_eq!(ui.get_section(), 24);
        ui.invoke_go_back();
        assert_eq!(ui.get_section(), 24);
        bridge.invoke_select_profile("reader".into());
        assert!(!bridge.get_require_selection());
        assert_eq!(ui.get_section(), 0);

        // The open-last-profile preference is device-local and persists.
        bridge.invoke_remember_last_changed(false);
        assert!(!crate::profiles::device_preferences().open_last_profile_on_startup);
        assert!(!bridge.get_remember_last());
        bridge.invoke_remember_last_changed(true);
        assert!(crate::profiles::device_preferences().open_last_profile_on_startup);

        // Deleting the profile on screen first moves to a survivor.
        bridge.invoke_select_profile(second.clone().into());
        bridge.invoke_delete_profile(second.clone().into());
        assert_eq!(reader_domain::profile::selected(), "reader");
        assert_eq!(names(&ui), ["Default"]);
        assert!(crate::profiles::is_retired(&second));
        // The last profile stays.
        bridge.invoke_delete_profile("reader".into());
        assert_ne!(bridge.get_error(), "");
        assert_eq!(names(&ui), ["Default"]);

        // A peer deleting the profile on screen returns this device to the picker.
        let third = crate::profiles::create("Third").unwrap();
        bridge.invoke_select_profile(third.id.clone().into());
        let mut peer = nova_sync::Store::default();
        peer.set(
            crate::profiles::DELETIONS_DOMAIN,
            &third.id,
            Some("1".into()),
            0,
            999,
        );
        peer.set(crate::profiles::DOMAIN, &third.id, None, 0, 999);
        {
            let owner = nova_sync::local_store().unwrap();
            let mut local = owner.lock().unwrap();
            for out in peer.outbound(&local.digest()) {
                local.apply(&out.domain, &out.key, out.record);
            }
        }
        app.reconcile_profiles(&ui);
        assert_eq!(reader_domain::profile::selected(), "reader");
        assert!(bridge.get_require_selection());
        assert_eq!(ui.get_section(), 24);
        assert_ne!(bridge.get_error(), "");
    }

    #[test]
    fn profile_switching_reconciles_selection_screen_and_directory() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::profile_actions::tests::profile_switching_child",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

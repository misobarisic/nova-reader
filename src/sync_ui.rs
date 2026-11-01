//! Reader's event-loop adapter for the shared sync settings surface.
//! Endpoint setup, shutdown, projection and clipboard I/O stay on workers.
use crate::{
    AppWindow, ReaderSyncSettings, SyncInspectorRow, SyncInvite, SyncPeer, services::Services,
};
use nova_reader_ui::backend_text as text;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

const INTERVALS: &[u64] = &[30, 60, 300, 900];
static REQUEST_GENERATION: AtomicU64 = AtomicU64::new(0);
type Refresh = Arc<dyn Fn(&AppWindow) + Send + Sync>;

pub(crate) struct SyncController {
    services: Services,
    ui: slint::Weak<AppWindow>,
    refresh: Refresh,
    generation: AtomicU64,
    starting: AtomicBool,
    projecting: AtomicBool,
    pending: AtomicBool,
    closed: AtomicBool,
    addon_busy: AtomicBool,
    addon_cancel: std::sync::Mutex<Arc<AtomicBool>>,
    addon_status: std::sync::Mutex<crate::sync_addons::Status>,
    addon_check: AtomicU64,
}

impl SyncController {
    pub(crate) fn initialize(
        services: Services,
        ui: &AppWindow,
        refresh: impl Fn(&AppWindow) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, String> {
        crate::sync_settings::init(ui)?;
        let controller = Arc::new(Self {
            services,
            ui: ui.as_weak(),
            refresh: Arc::new(refresh),
            generation: AtomicU64::new(0),
            starting: AtomicBool::new(false),
            projecting: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            addon_busy: AtomicBool::new(false),
            addon_cancel: std::sync::Mutex::new(Arc::new(AtomicBool::new(false))),
            addon_status: std::sync::Mutex::default(),
            addon_check: AtomicU64::new(0),
        });
        let settings = ui.global::<ReaderSyncSettings>();
        settings.set_sync_background_available(cfg!(target_os = "android"));
        settings.set_sync_qr_scan_available(cfg!(target_os = "android"));
        settings.set_invite_qr_available(true);

        // Preference editors may hold their cached settings lock while saving.
        // Never re-read their cache synchronously from the publisher callback.
        let weak = Arc::downgrade(&controller);
        crate::sync_settings::install_publisher(move || {
            nova_sync::request_local_sync();
            if let Some(controller) = weak.upgrade() {
                let next = controller.clone();
                let _ = controller
                    .ui
                    .upgrade_in_event_loop(move |ui| next.refresh_status(&ui));
            }
        });

        macro_rules! action {
            ($callback:ident, |$this:ident, $window:ident| $body:expr) => {{
                let weak = Arc::downgrade(&controller);
                settings.$callback(move || {
                    if let Some($this) = weak.upgrade()
                        && let Some($window) = $this.ui.upgrade()
                    {
                        $body
                    }
                });
            }};
            ($callback:ident, |$this:ident, $window:ident, $value:ident| $body:expr) => {{
                let weak = Arc::downgrade(&controller);
                settings.$callback(move |$value| {
                    if let Some($this) = weak.upgrade()
                        && let Some($window) = $this.ui.upgrade()
                    {
                        $body
                    }
                });
            }};
        }
        action!(on_sync_changed, |this, window| {
            let enabled = window.global::<ReaderSyncSettings>().get_sync_enabled();
            this.change_settings(&window, |s| s.enabled = enabled, true);
        });
        action!(on_sync_device_name_edited, |this, window, name| {
            this.change_settings(&window, |s| s.device_name = name.trim().into(), false);
        });
        action!(on_sync_pair_confirm_changed, |this, window, value| {
            this.change_settings(&window, |s| s.require_confirmation = value, false);
        });
        action!(on_sync_local_discovery_changed, |this, window, value| {
            this.change_settings(&window, |s| s.enable_local_discovery = value, true);
        });
        action!(on_sync_interval_picked, |this, window, index| {
            this.change_settings(&window, |s| s.interval_secs = interval(index), false);
        });
        action!(on_sync_background_changed, |this, window, value| {
            this.change_settings(&window, |s| s.background_enabled = value, false);
        });
        action!(on_sync_add_peer, |this, window, id| {
            if let Some(engine) = nova_sync::engine() {
                match engine.add_peer(id.trim()) {
                    Ok(()) => {
                        window
                            .global::<ReaderSyncSettings>()
                            .set_sync_peer_input("".into());
                        engine.sync_now();
                    }
                    Err(error) => this.notice(&window, "Could not add device", &error.to_string()),
                }
            }
            this.refresh_status(&window);
        });
        action!(on_sync_remove_peer, |this, window, id| {
            if let Some(engine) = nova_sync::engine() {
                engine.remove_peer(&id);
            }
            this.refresh_status(&window);
        });
        action!(on_sync_now, |this, window| {
            if let Some(engine) = nova_sync::engine() {
                engine.sync_now();
            }
            this.project();
            this.reconcile_addons();
            this.refresh_status(&window);
        });
        action!(on_sync_copy_identity, |this, window| {
            let identity = window.global::<ReaderSyncSettings>().get_sync_identity();
            this.copy(&window, identity.as_str());
        });
        action!(on_sync_copy_ticket, |this, window, ticket| this
            .copy(&window, &ticket));
        action!(on_sync_create_invite, |this, window| {
            if let Some(engine) = nova_sync::engine() {
                match engine.create_invite() {
                    Ok(ticket) => {
                        let global = window.global::<ReaderSyncSettings>();
                        global.set_sync_invite_qr(invite_qr(&ticket).unwrap_or_default());
                        global.set_sync_invite_ticket(ticket.into());
                        global.set_sync_link_notice(
                            text::tr("Invite ready; it expires in 15 minutes.").into(),
                        );
                    }
                    Err(error) => {
                        this.notice(&window, "Could not create invite", &error.to_string())
                    }
                }
            }
            this.refresh_status(&window);
        });
        action!(on_sync_cancel_invite, |this, window, id| {
            if let Some(engine) = nova_sync::engine() {
                engine.cancel_invite(&id);
            }
            let global = window.global::<ReaderSyncSettings>();
            global.set_sync_invite_ticket("".into());
            global.set_sync_invite_qr(slint::Image::default());
            this.refresh_status(&window);
        });
        action!(on_sync_join_invite, |this, window, ticket| this
            .join(&window, &ticket));
        action!(on_sync_scan_qr, |this, window| {
            #[cfg(target_os = "android")]
            {
                let weak = Arc::downgrade(&this);
                crate::android_sync::scan_invite(move |result| {
                    if let Some(controller) = weak.upgrade() {
                        let next = controller.clone();
                        let _ = controller.ui.upgrade_in_event_loop(move |ui| match result {
                            Ok(ticket) => next.join(&ui, &ticket),
                            Err(error) => next.notice(&ui, "Could not scan invite", &error),
                        });
                    }
                });
            }
            #[cfg(not(target_os = "android"))]
            this.notice(&window, "Camera scanning is available on Android only.", "");
        });
        let weak = Arc::downgrade(&controller);
        settings.on_sync_respond_pair(move |id, accept| {
            if let Some(controller) = weak.upgrade() {
                if let Some(engine) = nova_sync::engine() {
                    engine.respond_pair(&id, accept);
                }
                if let Some(ui) = controller.ui.upgrade() {
                    ui.global::<ReaderSyncSettings>()
                        .set_sync_pair_prompt_visible(false);
                }
            }
        });
        action!(on_sync_inspector_refresh, |this, window| this
            .inspector(&window));
        action!(on_sync_inspector_query_changed, |this, window, _query| this
            .inspector(&window));
        controller.refresh_status(ui);
        if nova_sync::read_settings().enabled {
            controller.start(false);
        }
        Ok(controller)
    }

    fn notice(&self, ui: &AppWindow, message: &str, detail: &str) {
        let rendered = if detail.is_empty() {
            text::tr(message)
        } else {
            format!("{}: {detail}", text::tr(message))
        };
        ui.global::<ReaderSyncSettings>()
            .set_sync_link_notice(rendered.into());
    }

    fn change_settings(
        self: &Arc<Self>,
        ui: &AppWindow,
        change: impl FnOnce(&mut nova_sync::SyncSettings),
        restart: bool,
    ) {
        let mut settings = nova_sync::read_settings();
        let previous = settings.clone();
        change(&mut settings);
        if let Err(error) = serde_json::to_string(&settings)
            .map_err(|e| e.to_string())
            .and_then(|raw| {
                nova_storage::try_set_str("sync:settings", &raw).map_err(|e| e.to_string())
            })
        {
            self.notice(ui, "Could not save sync settings", &error);
            self.refresh_status(ui);
            return;
        }
        // The engine's setter also wakes cadence and pending edit schedules.
        nova_sync::write_settings(&settings);
        if !settings.enabled {
            self.stop();
        } else if restart && settings != previous {
            self.start(true);
        } else if nova_sync::engine().is_none() {
            self.start(false);
        }
        self.platform_settings(&settings);
        self.refresh_status(ui);
    }

    fn platform_settings(&self, settings: &nova_sync::SyncSettings) {
        #[cfg(target_os = "android")]
        {
            crate::android_sync::set_network_observer(settings.enabled);
            crate::android_sync::schedule_background(
                settings.enabled && settings.background_enabled,
            );
        }
        #[cfg(not(target_os = "android"))]
        let _ = settings;
    }

    fn start(self: &Arc<Self>, restart: bool) {
        let generation = REQUEST_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
        self.generation.store(generation, Ordering::Release);
        self.starting.store(true, Ordering::Release);
        let controller = self.clone();
        std::thread::spawn(move || {
            let weak = Arc::downgrade(&controller);
            let valid = || valid_start(&weak, generation);
            let result = nova_sync::foreground_engine_guarded(valid, restart);
            let next = controller.clone();
            let _ = controller.ui.upgrade_in_event_loop(move |ui| {
                if !valid_start(&Arc::downgrade(&next), generation) {
                    return;
                }
                next.starting.store(false, Ordering::Release);
                match result {
                    Ok(Some(engine)) => next.attach(engine),
                    Ok(None) => {}
                    Err(error) => next.notice(&ui, "Could not start sync", &format!("{error:#}")),
                }
                next.refresh_status(&ui);
            });
        });
    }

    fn attach(self: &Arc<Self>, engine: Arc<nova_sync::SyncEngine>) {
        let generation = self.generation.load(Ordering::Acquire);
        let weak = Arc::downgrade(self);
        engine.set_on_remote(Arc::new(move |_| {
            if valid_start(&weak, generation)
                && let Some(controller) = weak.upgrade()
            {
                controller.project();
            }
        }));
        let weak = Arc::downgrade(self);
        engine.set_pair_callback(Arc::new(move |event| {
            if let Some(controller) = weak.upgrade() {
                let next = controller.clone();
                let _ = controller.ui.upgrade_in_event_loop(move |ui| {
                    next.pair_event(&ui, event, generation);
                });
            }
        }));
        self.platform_settings(&nova_sync::read_settings());
        self.project();
        self.reconcile_addons();
        engine.sync_now();
    }

    fn stop(self: &Arc<Self>) {
        let previous = self.generation.load(Ordering::Acquire);
        let generation = previous.wrapping_add(1);
        // A retiring Android activity cannot invalidate a newer activity's
        // engine request or release its foreground lease.
        if REQUEST_GENERATION
            .compare_exchange(previous, generation, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.generation.store(generation, Ordering::Release);
        self.starting.store(false, Ordering::Release);
        self.addon_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .store(true, Ordering::Release);
        if let Some(ui) = self.ui.upgrade() {
            let global = ui.global::<ReaderSyncSettings>();
            global.set_sync_pair_prompt_visible(false);
            global.set_sync_invite_ticket("".into());
            global.set_sync_invite_qr(slint::Image::default());
        }
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            nova_sync::uninstall_guarded(|| {
                REQUEST_GENERATION.load(Ordering::Acquire) == generation
                    && weak
                        .upgrade()
                        .is_none_or(|c| c.generation.load(Ordering::Acquire) == generation)
            });
        });
    }

    pub(crate) fn shutdown(self: &Arc<Self>) {
        self.closed.store(true, Ordering::Release);
        self.stop();
    }

    pub(crate) fn tick(self: &Arc<Self>, ui: &AppWindow) {
        if !ui.global::<ReaderSyncSettings>().get_sync_enabled()
            && !self.starting.load(Ordering::Acquire)
        {
            return;
        }
        // Hidden settings must not rebuild their models on every reading tick.
        if ui.get_section() == 20 || ui.global::<crate::ReaderSettingsSync>().get_open() {
            self.refresh_status(ui);
        }
        let pending = nova_sync::local_store().ok().is_some_and(|owner| {
            owner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_domains()
                .iter()
                .any(|domain| crate::sync::handles_projection_domain(domain))
        });
        if pending {
            self.project();
        }
        let now = crate::services::now().max(0) as u64;
        if now.saturating_sub(self.addon_check.load(Ordering::Relaxed)) >= 60 {
            self.addon_check.store(now, Ordering::Relaxed);
            self.reconcile_addons();
        }
    }

    fn project(self: &Arc<Self>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.pending.store(true, Ordering::Release);
        if self.projecting.swap(true, Ordering::AcqRel) {
            return;
        }
        let controller = self.clone();
        let generation = self.generation.load(Ordering::Acquire);
        std::thread::spawn(move || {
            let result = loop {
                controller.pending.store(false, Ordering::Release);
                let result = nova_sync::flush_persistence()
                    .and_then(|_| crate::sync::apply_remote(&controller.services.domain));
                if result.is_err() || !controller.pending.load(Ordering::Acquire) {
                    break result;
                }
            };
            let next = controller.clone();
            let published = controller.ui.upgrade_in_event_loop(move |ui| {
                next.projecting.store(false, Ordering::Release);
                if !current_generation(&next, generation) {
                    if next.pending.load(Ordering::Acquire) && !next.closed.load(Ordering::Acquire)
                    {
                        next.project();
                    }
                    return;
                }
                match result {
                    Ok(()) => {
                        if let Err(error) = crate::sync_settings::refresh_ui(&ui) {
                            next.notice(&ui, "Could not apply synced settings", &error);
                        }
                        (next.refresh)(&ui);
                        next.reconcile_addons();
                    }
                    Err(error) => {
                        next.notice(&ui, "Could not apply synced changes", &format!("{error:#}"))
                    }
                }
                next.refresh_status(&ui);
                if next.pending.load(Ordering::Acquire) {
                    next.project();
                }
            });
            if published.is_err() {
                controller.projecting.store(false, Ordering::Release);
            }
        });
    }

    fn reconcile_addons(self: &Arc<Self>) {
        if self.closed.load(Ordering::Acquire)
            || !nova_sync::read_settings().enabled
            || nova_sync::engine().is_none()
            || self.addon_busy.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *self.addon_cancel.lock().unwrap_or_else(|e| e.into_inner()) = cancel.clone();
        let controller = self.clone();
        let generation = self.generation.load(Ordering::Acquire);
        std::thread::spawn(move || {
            let result = crate::sync_addons::reconcile(&controller.services, cancel);
            let next = controller.clone();
            let published = controller.ui.upgrade_in_event_loop(move |ui| {
                next.addon_busy.store(false, Ordering::Release);
                if !current_generation(&next, generation) {
                    if !next.closed.load(Ordering::Acquire) && nova_sync::read_settings().enabled {
                        next.reconcile_addons();
                    }
                    return;
                }
                match result {
                    Ok(status) => {
                        *next.addon_status.lock().unwrap_or_else(|e| e.into_inner()) = status;
                        (next.refresh)(&ui);
                    }
                    Err(error) => next.notice(&ui, "Could not sync addons", &error),
                }
                next.refresh_status(&ui);
            });
            if published.is_err() {
                controller.addon_busy.store(false, Ordering::Release);
            }
        });
    }

    fn pair_event(self: &Arc<Self>, ui: &AppWindow, event: nova_sync::PairEvent, generation: u64) {
        if !valid_start(&Arc::downgrade(self), generation) {
            return;
        }
        let global = ui.global::<ReaderSyncSettings>();
        match event {
            nova_sync::PairEvent::Incoming(pair) => {
                global.set_sync_pair_prompt_id(pair.id.into());
                global.set_sync_pair_prompt_name(pair.name.into());
                global.set_sync_pair_prompt_code(pair.code.into());
                global.set_sync_pair_prompt_visible(true);
            }
            nova_sync::PairEvent::Paired { name } => {
                global.set_sync_pair_prompt_visible(false);
                global.set_sync_join_input("".into());
                global.set_sync_invite_ticket("".into());
                global.set_sync_invite_qr(slint::Image::default());
                self.notice(ui, "Paired with", &name);
                if let Some(engine) = nova_sync::engine() {
                    engine.sync_now();
                }
            }
        }
        self.refresh_status(ui);
    }

    fn join(&self, ui: &AppWindow, ticket: &str) {
        if let Some(engine) = nova_sync::engine() {
            match engine.join_invite(ticket.trim()) {
                Ok(()) => {
                    ui.global::<ReaderSyncSettings>()
                        .set_sync_join_input("".into());
                    self.notice(ui, "Connecting to the other device…", "");
                }
                Err(error) => self.notice(ui, "Invalid invite", &error.to_string()),
            }
        }
    }

    fn refresh_status(&self, ui: &AppWindow) {
        let settings = nova_sync::read_settings();
        let global = ui.global::<ReaderSyncSettings>();
        global.set_sync_enabled(settings.enabled);
        global.set_sync_device_name(settings.device_name.clone().into());
        global.set_sync_pair_confirm(settings.require_confirmation);
        global.set_sync_local_discovery(settings.enable_local_discovery);
        global.set_sync_interval_index(interval_index(settings.interval_secs));
        global.set_sync_background(settings.background_enabled);
        if let Some(engine) = nova_sync::engine().filter(|_| settings.enabled) {
            let names = engine.peer_names();
            let status = engine.status();
            let peers = engine
                .peers()
                .into_iter()
                .map(|id| {
                    let mut last_seen = engine
                        .peer_last_seen(&id)
                        .map_or_else(|| text::tr("Never connected"), ago);
                    if let Some(attempt) = status.peer_attempts.get(&id) {
                        if attempt.in_flight {
                            last_seen = text::tr("Syncing…");
                        } else if attempt.last_error.is_some() {
                            last_seen = text::tr("Waiting to retry");
                        }
                    }
                    SyncPeer {
                        name: names.get(&id).cloned().unwrap_or_default().into(),
                        id: id.into(),
                        last_seen: last_seen.into(),
                    }
                })
                .collect::<Vec<_>>();
            global.set_sync_identity(engine.identity().into());
            global.set_sync_peers(ModelRc::new(VecModel::from(peers)));
            global.set_sync_invites(ModelRc::new(VecModel::from(
                engine
                    .invites()
                    .into_iter()
                    .map(|i| SyncInvite { id: i.id.into() })
                    .collect::<Vec<_>>(),
            )));
            let mut rendered = status_text(&status);
            let addons = self.addon_status.lock().unwrap_or_else(|e| e.into_inner());
            if self.addon_busy.load(Ordering::Acquire) || addons.pending > 0 {
                rendered.push_str(&format!(
                    "\n{}: {}",
                    text::tr("Addon transfers pending"),
                    addons.pending
                ));
            }
            if addons.failed > 0 {
                rendered.push_str(&format!(
                    "\n{}: {}",
                    text::tr("Addon transfers failed"),
                    addons.failed
                ));
            }
            if addons.confirmation_required > 0 {
                rendered.push_str(&format!(
                    "\n{}: {}",
                    text::tr("Addons awaiting permission approval in Browse"),
                    addons.confirmation_required
                ));
            }
            global.set_sync_status(rendered.into());
            if let Some(peer) = engine.take_removed_notice() {
                self.notice(ui, "A device removed this connection", &peer);
            }
        } else {
            global.set_sync_identity("".into());
            global.set_sync_peers(ModelRc::default());
            global.set_sync_invites(ModelRc::default());
            global.set_sync_status(if self.starting.load(Ordering::Acquire) {
                text::tr("Starting sync…").into()
            } else {
                "".into()
            });
        }
        if let Err(error) = crate::sync_settings::refresh_metadata(ui) {
            self.notice(ui, "Could not read sync status", &error);
        }
        if global.get_sync_inspector_active() {
            self.inspector(ui);
        }
    }

    fn inspector(&self, ui: &AppWindow) {
        let Ok(owner) = nova_sync::local_store() else {
            return;
        };
        let store = owner.lock().unwrap_or_else(|e| e.into_inner());
        let pending = store.pending_domains();
        let digest = store.digest();
        let rows = store
            .domains()
            .into_iter()
            .map(|domain| {
                let total = digest.get(&domain).map_or(0, |rows| rows.len());
                let live = store.records(&domain).len();
                SyncInspectorRow {
                    title: domain.clone().into(),
                    detail: format!(
                        "{live} {} · {} {}",
                        text::tr("records"),
                        total.saturating_sub(live),
                        text::tr("retained deletions")
                    )
                    .into(),
                    status: text::tr(if pending.contains(&domain) {
                        "Pending local application"
                    } else {
                        "Applied locally; peer delivery is unknown"
                    })
                    .into(),
                }
            })
            .collect::<Vec<_>>();
        let global = ui.global::<ReaderSyncSettings>();
        global.set_sync_inspector_rows(ModelRc::new(VecModel::from(rows)));
        let query = global.get_sync_inspector_query().to_lowercase();
        let chapters = store
            .records(&crate::sync::domain("resume"))
            .into_iter()
            .filter(|(key, value)| {
                key.to_lowercase().contains(&query) || value.to_lowercase().contains(&query)
            })
            .take(80)
            .filter_map(|(key, raw)| {
                let progress: reader_domain::ReadingProgress = serde_json::from_str(&raw).ok()?;
                Some(SyncInspectorRow {
                    title: format!(
                        "{} · {}",
                        progress.chapter.series.source_id, progress.chapter.local_id
                    )
                    .into(),
                    detail: format!(
                        "{} {} · {} {:.0}%",
                        text::tr("Page"),
                        progress.anchor.page_index + 1,
                        text::tr("offset"),
                        progress.anchor.offset() * 100.
                    )
                    .into(),
                    status: format!(
                        "{} · {key}",
                        text::tr(if progress.completed {
                            "Read"
                        } else {
                            "Reading"
                        })
                    )
                    .into(),
                })
            })
            .collect::<Vec<_>>();
        global.set_sync_inspector_episodes(ModelRc::new(VecModel::from(chapters)));
    }

    fn copy(self: &Arc<Self>, _ui: &AppWindow, value: &str) {
        if value.is_empty() {
            return;
        }
        let value = value.to_string();
        let weak = Arc::downgrade(self);
        copy_text(value, move |result| {
            if let Some(controller) = weak.upgrade() {
                let next = controller.clone();
                let _ = controller.ui.upgrade_in_event_loop(move |ui| match result {
                    Ok(()) => next.notice(&ui, "Copied", ""),
                    Err(error) => next.notice(&ui, "Could not copy", &error),
                });
            }
        });
    }
}

fn valid_start(weak: &Weak<SyncController>, generation: u64) -> bool {
    weak.upgrade()
        .is_some_and(|c| current_generation(&c, generation) && nova_sync::read_settings().enabled)
}
fn current_generation(controller: &SyncController, generation: u64) -> bool {
    !controller.closed.load(Ordering::Acquire)
        && REQUEST_GENERATION.load(Ordering::Acquire) == generation
        && controller.generation.load(Ordering::Acquire) == generation
}
fn interval(index: i32) -> u64 {
    INTERVALS[index.clamp(0, INTERVALS.len() as i32 - 1) as usize]
}
fn interval_index(secs: u64) -> i32 {
    INTERVALS
        .iter()
        .enumerate()
        .min_by_key(|(_, value)| (value.abs_diff(secs), std::cmp::Reverse(**value)))
        .map_or(1, |(i, _)| i as i32)
}
fn ago(stamp: u64) -> String {
    let seconds = crate::services::now().max(0) as u64;
    let elapsed = seconds.saturating_sub(stamp);
    if elapsed < 60 {
        text::tr("Just now")
    } else if elapsed < 3600 {
        format!("{} {}", elapsed / 60, text::tr("minutes ago"))
    } else {
        format!("{} {}", elapsed / 3600, text::tr("hours ago"))
    }
}
fn status_text(status: &nova_sync::SyncStatus) -> String {
    if status.syncing {
        text::tr("Syncing…")
    } else if let Some(error) = &status.last_error {
        format!("{}: {error}", text::tr("Last sync failed"))
    } else if status.last_sync_secs > 0 {
        format!(
            "{}: {}",
            text::tr("Last synced"),
            ago(status.last_sync_secs)
        )
    } else {
        text::tr(if status.peer_count == 0 {
            "Add a peer to start syncing"
        } else {
            "Not synced yet"
        })
    }
}

fn invite_qr(ticket: &str) -> Option<slint::Image> {
    let code = qrcode::QrCode::new(ticket.as_bytes()).ok()?;
    let modules = code.width();
    let colors = code.to_colors();
    let side = (modules + 8) * 6;
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(side as u32, side as u32);
    let bytes = pixels.make_mut_bytes();
    bytes.fill(255);
    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] == qrcode::types::Color::Dark {
                for dy in 0..6 {
                    for dx in 0..6 {
                        let offset = (((y + 4) * 6 + dy) * side + (x + 4) * 6 + dx) * 3;
                        bytes[offset..offset + 3].fill(0);
                    }
                }
            }
        }
    }
    Some(slint::Image::from_rgb8(pixels))
}

#[cfg(not(target_os = "android"))]
fn copy_text(value: String, callback: impl FnOnce(Result<(), String>) + Send + 'static) {
    type Job = (String, Box<dyn FnOnce(Result<(), String>) + Send>);
    static WORKER: std::sync::OnceLock<std::sync::mpsc::Sender<Job>> = std::sync::OnceLock::new();
    let sender = WORKER.get_or_init(|| {
        let (sender, jobs) = std::sync::mpsc::channel::<Job>();
        std::thread::spawn(move || {
            let mut clipboard = None;
            for (value, callback) in jobs {
                let result = (|| {
                    if clipboard.is_none() {
                        clipboard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
                    }
                    clipboard
                        .as_mut()
                        .unwrap()
                        .set_text(value)
                        .map_err(|e| e.to_string())
                })();
                callback(result);
            }
        });
        sender
    });
    if let Err(error) = sender.send((value, Box::new(callback))) {
        (error.0.1)(Err(text::tr("Clipboard worker stopped")));
    }
}
#[cfg(target_os = "android")]
fn copy_text(value: String, callback: impl FnOnce(Result<(), String>) + Send + 'static) {
    std::thread::spawn(move || callback(crate::android_sync::copy_text(&value)));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_requests_ignore_pairing_and_unhandled_pending_domains() {
        const MODE: &str = "READER_SYNC_UI_GUARDS_CHILD";
        if let Ok(path) = std::env::var(MODE) {
            i_slint_backend_testing::init_no_event_loop();
            let services = Services::open(std::path::Path::new(&path)).unwrap();
            let ui = AppWindow::new().unwrap();
            let controller = SyncController::initialize(services, &ui, |_| {}).unwrap();
            let mut settings = nova_sync::read_settings();
            settings.enabled = true;
            nova_sync::write_settings(&settings);
            REQUEST_GENERATION.store(1, Ordering::Release);
            controller.generation.store(1, Ordering::Release);
            let incoming = || {
                nova_sync::PairEvent::Incoming(nova_sync::IncomingPair {
                    id: "request".into(),
                    name: "Peer".into(),
                    code: "123456".into(),
                })
            };
            controller.pair_event(&ui, incoming(), 1);
            assert!(
                ui.global::<ReaderSyncSettings>()
                    .get_sync_pair_prompt_visible()
            );
            controller.stop();
            controller.pair_event(&ui, incoming(), 1);
            assert!(
                !ui.global::<ReaderSyncSettings>()
                    .get_sync_pair_prompt_visible()
            );
            REQUEST_GENERATION.store(3, Ordering::Release);
            controller.stop();
            assert_eq!(
                REQUEST_GENERATION.load(Ordering::Acquire),
                3,
                "retiring activity must not invalidate a newer owner"
            );
            controller.generation.store(3, Ordering::Release);
            let owner = nova_sync::local_store().unwrap();
            {
                let mut store = owner.lock().unwrap();
                for domain in ["peers", "unknown_future", &crate::sync_addons::domain()] {
                    store.mark_pending(domain);
                }
            }
            ui.global::<ReaderSyncSettings>().set_sync_enabled(true);
            controller.tick(&ui);
            assert!(!controller.projecting.load(Ordering::Acquire));
            assert!(!owner.lock().unwrap().pending_domains().is_empty());
            assert!(
                nova_sync::engine().is_none(),
                "guard tests must not bind an endpoint"
            );
            controller.shutdown();
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sync_ui::tests::stopped_requests_ignore_pairing_and_unhandled_pending_domains",
                "--nocapture",
            ])
            .env(MODE, dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    fn interval_presets_and_status_keep_real_engine_state() {
        for (i, value) in INTERVALS.iter().enumerate() {
            assert_eq!(interval(i as i32), *value);
            assert_eq!(interval_index(*value), i as i32);
        }
        assert_eq!(interval_index(180), 2);
        assert_eq!(interval(-1), 30);
        assert_eq!(interval(99), 900);
        let mut status = nova_sync::SyncStatus::default();
        assert_eq!(status_text(&status), "Add a peer to start syncing");
        status.peer_count = 1;
        assert_eq!(status_text(&status), "Not synced yet");
        status.syncing = true;
        assert_eq!(status_text(&status), "Syncing…");
    }
}

//! Device appearance preferences, independent of portable library metadata.
use crate::{Anim, AppWindow, Appearance};
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

const KEY: &str = "reader:appearance";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    accent_color: i32,
    background_style: i32,
    card_radius: i32,
    button_radius: i32,
    card_spacing: i32,
    transparent_navbar: bool,
    animations: bool,
    transitions: bool,
    hover: bool,
    navigation_feedback: bool,
    library_list_view: bool,
    library_min_cols: i32,
    browse_min_cols: i32,
    library_sort: i32,
    /// Reverses the chosen sort's natural order (newest, A→Z or most first),
    /// so every sort keeps its existing order by default.
    library_sort_reversed: bool,
    /// Updates: 0 by day and series, 1 by day only. Device-only, like the
    /// two History choices below (see `sync_settings::portable`).
    updates_grouping: i32,
    /// History: 0 session cards, 1 timeline.
    history_view: i32,
    history_shelf_hidden: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            accent_color: 0,
            background_style: 0,
            card_radius: 10,
            button_radius: 8,
            card_spacing: 8,
            transparent_navbar: true,
            animations: true,
            transitions: true,
            hover: true,
            navigation_feedback: true,
            library_list_view: false,
            library_min_cols: 2,
            browse_min_cols: 3,
            library_sort: 0,
            library_sort_reversed: false,
            updates_grouping: 0,
            history_view: 0,
            history_shelf_hidden: false,
        }
    }
}

impl Preferences {
    pub(crate) fn valid(&self) -> bool {
        let mut trial = self.clone();
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .is_some_and(|fields| {
                fields.into_iter().all(|(key, value)| {
                    let value = value
                        .as_bool()
                        .map(i32::from)
                        .or_else(|| value.as_i64().and_then(|v| i32::try_from(v).ok()));
                    value.is_some_and(|value| trial.change(&key, value))
                })
            })
    }

    fn apply(&self, ui: &AppWindow) {
        let appearance = ui.global::<Appearance>();
        appearance.set_accent_color(self.accent_color.clamp(0, 7));
        appearance.set_background_style(self.background_style.clamp(0, 2));
        appearance.set_card_radius(self.card_radius.clamp(0, 24));
        appearance.set_button_radius(self.button_radius.clamp(0, 24));
        appearance.set_card_spacing(self.card_spacing.clamp(0, 32));
        appearance.set_transparent_navbar(self.transparent_navbar);
        appearance.set_animations(self.animations);
        appearance.set_transitions(self.transitions);
        appearance.set_hover(self.hover);
        appearance.set_navigation_feedback(self.navigation_feedback);
        appearance.set_library_list_view(self.library_list_view);
        appearance.set_library_min_cols(self.library_min_cols.clamp(2, 6));
        appearance.set_browse_min_cols(self.browse_min_cols.clamp(2, 6));
        appearance.set_library_sort(self.library_sort.clamp(0, 4));
        appearance.set_library_sort_reversed(self.library_sort_reversed);
        appearance.set_updates_grouping(self.updates_grouping.clamp(0, 1));
        appearance.set_history_view(self.history_view.clamp(0, 1));
        appearance.set_history_shelf_hidden(self.history_shelf_hidden);
        let anim = ui.global::<Anim>();
        anim.set_enabled(self.animations);
        anim.set_transitions(self.transitions);
        anim.set_hover(self.hover);
        anim.set_nav_slide(self.navigation_feedback);
        nova_reader_ui::appearance::apply_theme(ui);
    }

    fn change(&mut self, key: &str, value: i32) -> bool {
        match key {
            "accent_color" if (0..8).contains(&value) => self.accent_color = value,
            "background_style" if (0..3).contains(&value) => self.background_style = value,
            "card_radius" if [0, 4, 10, 16, 24].contains(&value) => self.card_radius = value,
            "button_radius" if [0, 4, 8, 12, 24].contains(&value) => self.button_radius = value,
            "card_spacing" if [0, 4, 8, 12, 16].contains(&value) => self.card_spacing = value,
            "transparent_navbar" if value == 0 || value == 1 => {
                self.transparent_navbar = value == 1
            }
            "animations" if value == 0 || value == 1 => self.animations = value == 1,
            "transitions" if value == 0 || value == 1 => self.transitions = value == 1,
            "hover" if value == 0 || value == 1 => self.hover = value == 1,
            "navigation_feedback" if value == 0 || value == 1 => {
                self.navigation_feedback = value == 1
            }
            "library_list_view" if value == 0 || value == 1 => self.library_list_view = value == 1,
            "library_min_cols" if (2..=6).contains(&value) => self.library_min_cols = value,
            "browse_min_cols" if (2..=6).contains(&value) => self.browse_min_cols = value,
            "library_sort" if (0..=4).contains(&value) => self.library_sort = value,
            "library_sort_reversed" if value == 0 || value == 1 => {
                self.library_sort_reversed = value == 1
            }
            "updates_grouping" if value == 0 || value == 1 => self.updates_grouping = value,
            "history_view" if value == 0 || value == 1 => self.history_view = value,
            "history_shelf_hidden" if value == 0 || value == 1 => {
                self.history_shelf_hidden = value == 1
            }
            _ => return false,
        }
        true
    }
}

fn save(ui: &AppWindow, state: &RefCell<Preferences>, next: Preferences) {
    let result = crate::sync_settings::persist_preferences(KEY, &*state.borrow(), &next);
    match result {
        Ok(next) => {
            next.apply(ui);
            *state.borrow_mut() = next;
            ui.set_status("".into());
        }
        Err(e) => {
            // Kit toggles edit a two-way UI draft; failed persistence must
            // restore both the draft control and the effective globals.
            state.borrow().apply(ui);
            ui.set_status(
                format!(
                    "{}: {e}",
                    nova_reader_ui::backend_text::tr("Could not save appearance")
                )
                .into(),
            );
        }
    }
}

thread_local! {
    // The UI-thread draft that the callbacks edit; a profile switch replaces its value.
    static DRAFT: RefCell<Option<Rc<RefCell<Preferences>>>> = const { RefCell::new(None) };
}

fn load(ui: &AppWindow) -> Result<Preferences, String> {
    Ok(
        match reader_domain::profile::kv::try_get_str(KEY).map_err(|e| e.to_string())? {
            Some(json) => match serde_json::from_str::<Preferences>(&json) {
                Ok(prefs) => prefs,
                Err(e) => {
                    // A cosmetic preference must not prevent opening the reader.
                    // Keep the raw value until a deliberate successful user edit.
                    eprintln!("Nova Reader: invalid appearance preferences: {e}");
                    ui.set_status(
                        nova_reader_ui::backend_text::tr(
                            "Could not read appearance preferences; using defaults.",
                        )
                        .into(),
                    );
                    Preferences::default()
                }
            },
            None => Preferences::default(),
        },
    )
}

/// Re-read the active profile's appearance after a profile switch.
pub fn reload(ui: &AppWindow) -> Result<(), String> {
    let prefs = load(ui)?;
    prefs.apply(ui);
    DRAFT.with(|draft| {
        if let Some(state) = draft.borrow().as_ref() {
            *state.borrow_mut() = prefs;
        }
    });
    Ok(())
}

/// Call after Services initializes Reader's storage and before showing the UI.
pub fn init(ui: &AppWindow) -> Result<(), String> {
    let prefs = load(ui)?;
    prefs.apply(ui);
    let state = Rc::new(RefCell::new(prefs));
    DRAFT.with(|draft| *draft.borrow_mut() = Some(state.clone()));
    let weak = ui.as_weak();
    let current = state.clone();
    ui.global::<Appearance>()
        .on_setting_changed(move |key, value| {
            if let Some(ui) = weak.upgrade() {
                let mut next = current.borrow().clone();
                if next.change(&key, value) {
                    save(&ui, &current, next);
                }
            }
        });
    let weak = ui.as_weak();
    ui.global::<Appearance>().on_restore_theme(move || {
        if let Some(ui) = weak.upgrade() {
            // Nova's theme reset does not reset the separate motion choices.
            let current = state.borrow();
            let next = Preferences {
                animations: current.animations,
                transitions: current.transitions,
                hover: current.hover,
                navigation_feedback: current.navigation_feedback,
                library_list_view: current.library_list_view,
                library_min_cols: current.library_min_cols,
                browse_min_cols: current.browse_min_cols,
                library_sort: current.library_sort,
                library_sort_reversed: current.library_sort_reversed,
                updates_grouping: current.updates_grouping,
                history_view: current.history_view,
                history_shelf_hidden: current.history_shelf_hidden,
                ..Preferences::default()
            };
            drop(current);
            save(&ui, &state, next);
        }
    });
    Ok(())
}

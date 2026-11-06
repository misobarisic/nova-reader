//! Reader preferences use Nova's existing fallible KV transactions. They are
//! application-local metadata, separate from portable library backups.
use crate::AppWindow;
use nova_reader_ui::backend_text as text;
use reader_domain::SeriesId;
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc};

const ADULT_KEY: &str = "reader:show-adult";
const ORDER_PREFIX: &str = "reader:chapter-order:";

fn read_bool(key: &str, default: bool) -> Result<bool, String> {
    match reader_domain::profile::kv::try_get_str(key).map_err(|error| error.to_string())? {
        Some(raw) => serde_json::from_str(&raw).map_err(|error| error.to_string()),
        None => Ok(default),
    }
}

fn write_bool(key: &str, value: bool) -> Result<(), String> {
    crate::sync_settings::persist_preferences(key, &!value, &value).map(|_| ())
}

fn order_key(series: &SeriesId) -> String {
    format!(
        "{ORDER_PREFIX}{}",
        serde_json::to_string(series).expect("SeriesId serializes")
    )
}

/// Invalid/unavailable settings fall back without overwriting their raw value.
pub fn show_adult() -> bool {
    read_bool(ADULT_KEY, false).unwrap_or(false)
}

pub fn set_show_adult(value: bool) -> Result<(), String> {
    write_bool(ADULT_KEY, value)
}

/// The full source-local identity avoids collisions between different sources.
pub fn chapter_descending(series: &SeriesId) -> bool {
    read_bool(&order_key(series), true).unwrap_or(true)
}

pub fn set_chapter_descending(series: &SeriesId, descending: bool) -> Result<(), String> {
    write_bool(&order_key(series), descending)
}

pub fn save_chapter_order(
    ui: &AppWindow,
    series: &SeriesId,
    descending: bool,
) -> Result<(), String> {
    change_chapter_order(ui, descending, || {
        set_chapter_descending(series, descending)
    })
}

fn change_chapter_order(
    ui: &AppWindow,
    descending: bool,
    write: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    write()?;
    ui.set_chapters_descending(descending);
    Ok(())
}

fn change_adult(
    ui: &AppWindow,
    saved: &Cell<bool>,
    next: bool,
    write: impl FnOnce(bool) -> Result<(), String>,
) {
    match write(next) {
        Ok(()) => {
            saved.set(next);
            ui.set_show_adult(next);
            ui.invoke_navigate(3);
        }
        Err(error) => {
            ui.set_show_adult(saved.get());
            ui.set_status(
                format!("{}: {error}", text::tr("Could not save source visibility")).into(),
            );
        }
    }
}

thread_local! {
    // Last persisted toggle value, shared by the callback and profile reloads.
    static SAVED: std::cell::RefCell<Option<Rc<Cell<bool>>>> = const { std::cell::RefCell::new(None) };
}

fn load_adult(ui: &AppWindow) -> bool {
    match read_bool(ADULT_KEY, false) {
        Ok(value) => value,
        Err(_) => {
            ui.set_status(text::tr("Could not read source visibility; using defaults.").into());
            false
        }
    }
}

/// Re-read the active profile's source visibility after a profile switch.
pub fn reload(ui: &AppWindow) {
    let adult = load_adult(ui);
    ui.set_show_adult(adult);
    SAVED.with(|saved| {
        if let Some(saved) = saved.borrow().as_ref() {
            saved.set(adult);
        }
    });
}

/// Initialize after the same storage backend used by Nova has opened.
pub fn init(ui: &AppWindow) -> Result<(), String> {
    let adult = load_adult(ui);
    ui.set_show_adult(adult);
    let saved = Rc::new(Cell::new(adult));
    SAVED.with(|cell| *cell.borrow_mut() = Some(saved.clone()));
    let weak = ui.as_weak();
    ui.on_adult_visibility_changed(move |next| {
        if let Some(ui) = weak.upgrade() {
            change_adult(&ui, &saved, next, set_show_adult);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_visibility_write_restores_the_toggle_without_refiltering_sources() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let saved = Cell::new(true);
        let navigations = Rc::new(Cell::new(0));
        ui.on_navigate({
            let navigations = navigations.clone();
            move |_| navigations.set(navigations.get() + 1)
        });
        ui.set_show_adult(false);
        change_adult(&ui, &saved, false, |_| Err("write failed".into()));
        assert!(ui.get_show_adult() && saved.get());
        assert_eq!(navigations.get(), 0);
        assert!(ui.get_status().contains("write failed"));
        change_adult(&ui, &saved, false, |_| Ok(()));
        assert!(!ui.get_show_adult() && !saved.get());
        assert_eq!(navigations.get(), 1);
    }

    #[test]
    fn chapter_order_is_published_only_after_the_write_succeeds() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        assert!(ui.get_chapters_descending());
        assert!(change_chapter_order(&ui, false, || Err("write failed".into())).is_err());
        assert!(ui.get_chapters_descending());
        change_chapter_order(&ui, false, || Ok(())).unwrap();
        assert!(!ui.get_chapters_descending());
    }
}

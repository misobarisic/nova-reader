//! Nova Reader's platform entry points. Reader services live in leaf crates.
pub use nova_reader_ui::*;
mod about;
mod addon_icons;
#[cfg(target_os = "android")]
pub mod android_documents;
#[cfg(target_os = "android")]
pub mod android_sync;
mod app;
pub mod appearance;
mod artwork;
mod browser;
pub mod capture_settings;
pub mod chapter_cache;
mod chapter_capture;
mod cli;
#[cfg(not(target_os = "android"))]
mod desktop_dialogs;
pub mod export_names;
pub mod image_storage;
mod metadata_refresh;
mod paths;
mod progress_checkpoint;
#[cfg(not(target_os = "android"))]
mod reader_desktop;
mod reader_export;
pub mod reader_modes;
mod reader_navigation;
pub mod reader_settings;
mod reader_settings_ui;
pub mod reading_preferences;
pub mod services;
mod source_browser;
mod source_catalogs;
mod source_filters;
pub mod sync;
pub mod sync_addons;
pub mod sync_settings;
mod sync_ui;
mod timeline;
mod updates;

pub fn main_entry() -> Result<(), Box<dyn std::error::Error>> {
    if cli::run()? {
        return Ok(());
    }
    let services = services::Services::open(&paths::data_dir()?)?;
    app::run(services).map_err(Into::into)
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    if let Some(path) = app.internal_data_path() {
        let _ = paths::ANDROID_DATA.set(path.join("nova-reader"));
    }
    crate::android_sync::initialize_foreground_context(app.vm_as_ptr(), app.activity_as_ptr());
    slint::android::init_with_event_listener(app, |event| {
        use slint::android::android_activity::{MainEvent, PollEvent};
        match event {
            PollEvent::Main(MainEvent::Resume { .. }) => {
                metadata_refresh::set_foreground(true);
                nova_sync::set_foreground(true);
                nova_sync::notify_network_change();
            }
            PollEvent::Main(MainEvent::Pause | MainEvent::Destroy) => {
                metadata_refresh::set_foreground(false);
                nova_sync::set_foreground(false);
                app::flush_lifecycle();
            }
            _ => {}
        }
    })
    .expect("initialize Android UI");
    if let Err(error) = main_entry() {
        eprintln!("Nova Reader: {error}");
    }
}

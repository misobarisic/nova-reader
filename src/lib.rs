//! Nova Reader's platform entry points. Reader services live in leaf crates.
pub use nova_reader_ui::*;
mod about;
mod addon_icons;
#[cfg(target_os = "android")]
pub mod android_documents;
mod app;
pub mod appearance;
mod artwork;
mod browser;
pub mod capture_settings;
pub mod chapter_cache;
mod chapter_capture;
mod cli;
pub mod export_names;
pub mod image_storage;
mod paths;
mod reader_export;
pub mod reader_modes;
pub mod reading_preferences;
pub mod services;
mod source_browser;
mod source_filters;
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
    slint::android::init_with_event_listener(app, |event| {
        use slint::android::android_activity::{MainEvent, PollEvent};
        if matches!(
            event,
            PollEvent::Main(MainEvent::Pause | MainEvent::Destroy)
        ) {
            app::flush_lifecycle();
        }
    })
    .expect("initialize Android UI");
    if let Err(error) = main_entry() {
        eprintln!("Nova Reader: {error}");
    }
}

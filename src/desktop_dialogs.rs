//! Desktop document selection, using Nova's working addon ZIP picker.

use std::path::PathBuf;

/// Runs on the application's task worker, as in Nova's addon_picker::launch.
/// Cancellation keeps the existing rfd semantics and returns no selected file.
pub(crate) fn choose_addon() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(nova_reader_ui::backend_text::tr(
            "Choose an addon ZIP file…",
        ))
        .add_filter("ZIP", &["zip"])
        .pick_file()
}

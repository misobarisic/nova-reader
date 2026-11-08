//! The compiled Slint UI is isolated so domain and source work stays inexpensive.
mod generated {
    slint::include_modules!();
}
// Generated names keep their public paths; `AppWindow` is the wrapper below,
// which installs the page factories for production and headless callers alike.
pub use generated::*;
// Slint 1.18 does not re-export imported library types; keep their existing
// paths while sharing the compiled library's exact types.
pub use nova_reader_ui_shared::{
    Appearance, BackupBridge, BackupMapping, BackupOption, BrowseBridge, CaptureSettings,
    DownloadCleanupSettings, EntryCategoryRow, ExtensionRow, FilterGroup, FilterPill,
    ImageLimitSettings, ImageStorageSettings, ItemRow, LibraryBridge, PageMenuItem,
    ReaderSyncSettings, SearchSection, SettingRow, SettingSyncInfo, SettingsPages, SheetItem,
    ShelfEntry, SourceRow, SyncContent, SyncInspectorRow, SyncInvite, SyncPeer, ThemePalette,
    TimelineBridge, TimelineEntry,
};
// One sync-badge global for the window and every page; Rust keeps the name
// Reader used before the split. (Renaming a library global inside `.slint`
// would make the generator create a second copy.)
pub use nova_reader_ui_shared::SettingsSync as ReaderSettingsSync;
pub mod backend_text;

pub mod appearance;
mod page_factories;
mod window;
pub use window::AppWindow;

// Page callbacks reach the window's callbacks without keeping it alive.
macro_rules! forward_app_callbacks {
    ($app:expr, $bridge:expr; $($callback:ident => $invoke:ident($($arg:ident),*)),* $(,)?) => {{
        let bridge = $bridge;
        $(let weak = $app.as_weak();
        bridge.$callback(move |$($arg),*| {
            weak.upgrade().map(|app| app.$invoke($($arg),*)).unwrap_or_default()
        });)*
    }};
}
pub(crate) use forward_app_callbacks;

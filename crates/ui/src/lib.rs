//! The compiled Slint UI is isolated so domain and source work stays inexpensive.
slint::include_modules!();
// Slint 1.18 does not re-export imported library types; keep their existing
// paths while sharing the compiled library's exact types.
pub use nova_reader_ui_shared::{SettingSyncInfo, SheetItem, ThemePalette};
pub mod backend_text;

pub mod appearance;

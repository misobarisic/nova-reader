//! The library page, compiled apart from the window so edits to it rebuild
//! only this crate. The window embeds it through `LibraryBridge.factory` and
//! shares its globals.
slint::include_modules!();

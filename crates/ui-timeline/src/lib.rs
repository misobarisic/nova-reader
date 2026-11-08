//! Updates and History, compiled apart from the window so edits to them
//! rebuild only this crate. The window embeds the page through
//! `TimelineBridge.factory` and shares its globals.
slint::include_modules!();

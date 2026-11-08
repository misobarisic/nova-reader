//! Browse (sources, extensions, catalogues and search across sources),
//! compiled apart from the window so edits to it rebuild only this crate.
//! The window embeds it through `BrowseBridge.factory` and shares its globals.
slint::include_modules!();

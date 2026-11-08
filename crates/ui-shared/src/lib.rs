//! Reader's compiled shared UI library: the theme and host globals, shared
//! settings globals and row types, with no page visual tree. The window and
//! every page crate import it as `@NovaUiShared`, so they all use the same
//! global objects.
slint::include_modules!();

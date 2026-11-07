//! The page boundary is enforced by the compiler (the strip types are private
//! to `src/page_store/strips.rs`, with a `compile_fail` doctest on the module)
//! and watched here: no other source file may mention the container, and the
//! raw image decoders may be called on stored pages only through `PageImage`.
use std::{
    fs,
    path::{Path, PathBuf},
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn files(directory: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | "vendor" | ".git" | "node_modules") {
                files(&path, extensions, out);
            }
        } else if path
            .extension()
            .is_some_and(|ext| extensions.contains(&ext.to_string_lossy().as_ref()))
        {
            out.push(path);
        }
    }
}
/// Every Rust and Slint source of the workspace except this file.
fn sources() -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for directory in ["src", "tests", "crates", "examples"] {
        files(&root().join(directory), &["rs", "slint"], &mut paths);
    }
    let this = Path::new(file!()).file_name().unwrap().to_owned();
    paths
        .into_iter()
        .filter(|path| path.file_name() != Some(&this))
        .map(|path| {
            (
                path.strip_prefix(root())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                fs::read_to_string(&path).unwrap(),
            )
        })
        .collect()
}
fn mentioning<'a>(
    sources: &'a [(String, String)],
    needle: &'a str,
) -> impl Iterator<Item = &'a str> {
    sources
        .iter()
        .filter(move |(_, text)| text.contains(needle))
        .map(|(path, _)| path.as_str())
}

#[test]
fn only_the_page_store_knows_the_container_layout() {
    let sources = sources();
    let allowed = ["src/page_store.rs", "src/page_store/strips.rs"];
    for needle in [
        "NRSTRIP",
        "strips::",
        "Stored::",
        "Container::parse",
        "Container::build",
        "strip_table",
    ] {
        let leaks = mentioning(&sources, needle)
            .filter(|path| !allowed.contains(path))
            .collect::<Vec<_>>();
        assert!(leaks.is_empty(), "{needle:?} leaks into {leaks:?}");
    }
    // The module is private and re-exports nothing from it.
    let store = &sources
        .iter()
        .find(|(path, _)| path == "src/page_store.rs")
        .unwrap()
        .1;
    assert!(store.lines().any(|line| line == "mod strips;"));
    for forbidden in [
        "pub mod strips",
        "pub(crate) mod strips",
        "pub use strips",
        "pub(crate) use strips",
        "pub use self::strips",
    ] {
        assert!(!store.contains(forbidden), "{forbidden}");
    }
    // Inside strips.rs nothing is more visible than the page store itself.
    let strips = &sources
        .iter()
        .find(|(path, _)| path == "src/page_store/strips.rs")
        .unwrap()
        .1;
    for line in strips.lines() {
        let line = line.trim_start();
        assert!(
            ![
                "pub fn",
                "pub struct",
                "pub enum",
                "pub const",
                "pub type",
                "pub trait",
                "pub mod",
                "pub use"
            ]
            .iter()
            .any(|public| line.starts_with(public)),
            "strips.rs exposes: {line}"
        );
    }
    // No other module may even spell the module path.
    assert!(
        mentioning(&sources, "page_store/strips").all(|path| allowed.contains(&path)),
        "only the store's own docs name the container's file"
    );
}

/// First line in `text` that is not inside a `#[cfg(test)]` module, matching
/// `needle`. Files are treated as test-only code after their first
/// `#[cfg(test)]`, which every module in this repository places last.
fn production_use(text: &str, needle: &str) -> Option<usize> {
    let production = text.find("#[cfg(test)]").unwrap_or(text.len());
    text[..production]
        .find(needle)
        .map(|at| text[..at].lines().count() + 1)
}

#[test]
fn raw_image_decoders_are_not_called_on_stored_pages_outside_the_boundary() {
    let sources = sources();
    // Each of these decodes or inspects bytes that are not a stored page: fetched
    // source bytes before they are stored, covers and icons, locally encoded
    // exports and thumbnails, or the crate that implements the codecs.
    let allowed = [
        "src/page_store.rs",
        "src/page_store/strips.rs",
        "src/reader_export.rs",
        "src/chapter_cache.rs",
        "src/page_thumbnails.rs",
        "src/artwork.rs",
        "src/addon_icons.rs",
        "src/reader_progressive.rs",
        "crates/reader-engine/",
        "crates/reader-downloads/src/lib.rs",
    ];
    for needle in [
        "decode_image(",
        "inspect_image(",
        "decode_display_image(",
        "decode_display_window(",
    ] {
        for (path, text) in &sources {
            if path.starts_with("tests/") || path.starts_with("examples/") {
                continue;
            }
            if allowed.iter().any(|allowed| path.starts_with(allowed)) {
                continue;
            }
            // Test fixtures in other modules may decode their own PNGs.
            assert!(
                production_use(text, needle).is_none(),
                "{path}:{} decodes pages without going through PageImage",
                production_use(text, needle).unwrap_or(0)
            );
        }
    }
    // Reader code that has a stored page asks the page, never the codec.
    for path in [
        "src/app.rs",
        "src/page_inventory.rs",
        "src/reader_capture_sources.rs",
        "src/reader_capture_editor.rs",
        "src/chapter_capture.rs",
    ] {
        let text = &sources.iter().find(|(p, _)| p == path).unwrap().1;
        for needle in ["decode_image(", "inspect_image(", "decode_display_image("] {
            assert!(production_use(text, needle).is_none(), "{path} {needle}");
        }
    }
    assert!(
        mentioning(&sources, "decode_image(").any(|path| path == "crates/reader-engine/src/lib.rs"),
        "the scan sees the engine"
    );
}

#[test]
fn stored_bytes_and_stored_page_constructors_stay_with_the_stores() {
    let sources = sources();
    // `stored_bytes` is the file as written: only the cache writer, the
    // thumbnail preview feed and the page store itself may read it.
    let stored_bytes = [
        "src/page_store.rs",
        "src/chapter_cache.rs",
        "src/page_thumbnails.rs",
    ];
    for (path, text) in &sources {
        if path.starts_with("tests/") || path.starts_with("examples/") {
            continue;
        }
        if stored_bytes.contains(&path.as_str()) {
            continue;
        }
        assert!(
            production_use(text, ".stored_bytes(").is_none(),
            "{path} reads the stored file"
        );
        // A page read back from disk is built only where a store reads it.
        if !["src/page_inventory.rs", "src/app.rs"].contains(&path.as_str()) {
            assert!(
                production_use(text, "PageImage::from_stored(").is_none(),
                "{path} reads a stored page"
            );
        }
    }
}

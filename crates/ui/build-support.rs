// Shared by the window and every compiled UI library so they agree on style,
// Kit source paths, element metadata and translation context.

fn enable_experimental_components() {
    // Compiled libraries and the component factory/container are experimental
    // in Slint 1.18. SAFETY: build scripts configure Slint before any threads.
    unsafe { std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1") };
}

fn compiler_configuration() -> slint_build::CompilerConfiguration {
    let kit = std::path::PathBuf::from(nova_ui_base::SLINT_SOURCE_DIR);
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=../ui/build-support.rs");
    slint_build::CompilerConfiguration::new()
        .with_include_paths(vec![kit.clone()])
        .with_library_paths(std::collections::HashMap::from([
            ("NovaUiBase".into(), kit),
            ("NovaSyncHost".into(), manifest.join("../ui/sync-host.slint")),
        ]))
        // Layout regressions inspect element bounds in development builds.
        .with_debug_info(std::env::var("PROFILE").as_deref() == Ok("debug"))
        .with_style("fluent-dark".into())
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
        .with_default_translation_context(slint_build::DefaultTranslationContext::None)
}

/// Compile a `.slint` file as a named library other UI crates import.
#[allow(dead_code)] // The window crate compiles an application, not a library.
fn compile_library(source: &str, library: &str) {
    enable_experimental_components();
    slint_build::compile_with_config(source, compiler_configuration().as_library(library))
        .expect("compile UI library");
}

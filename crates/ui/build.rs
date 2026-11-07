fn main() {
    let shared = std::path::PathBuf::from(nova_ui_base::SLINT_SOURCE_DIR);
    slint_build::compile_with_config(
        "app.slint",
        slint_build::CompilerConfiguration::new()
            .with_include_paths(vec![shared.clone()])
            .with_library_paths(std::collections::HashMap::from([
                ("NovaUiBase".into(), shared.clone()),
                (
                    "NovaUiShared".into(),
                    std::path::PathBuf::from("host-globals.slint"),
                ),
                (
                    "NovaSyncHost".into(),
                    std::path::PathBuf::from("sync-host.slint"),
                ),
            ]))
            // Layout regressions inspect element bounds in development builds.
            .with_debug_info(std::env::var("PROFILE").as_deref() == Ok("debug"))
            .with_style("fluent-dark".into())
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("compile Nova Reader UI");
}

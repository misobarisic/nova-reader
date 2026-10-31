fn main() {
    let shared = std::path::PathBuf::from(nova_ui_base::SLINT_SOURCE_DIR);
    slint_build::compile_with_config(
        "app.slint",
        slint_build::CompilerConfiguration::new()
            .with_include_paths(vec![shared])
            .with_style("fluent-dark".into())
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("compile Nova Reader UI");
}

fn main() {
    let shared = std::path::PathBuf::from(nova_ui_base::SLINT_SOURCE_DIR);
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let dropdown = reader_dropdown(&shared, &out);
    let segmented = reader_segmented(&shared, &out);
    slint_build::compile_with_config(
        "app.slint",
        slint_build::CompilerConfiguration::new()
            .with_include_paths(vec![shared.clone()])
            .with_library_paths(std::collections::HashMap::from([
                ("NovaUiBase".into(), shared.clone()),
                ("ReaderDropdown".into(), dropdown),
                ("ReaderSegmented".into(), segmented),
                ("NovaUiShared".into(), shared.join("host-globals.slint")),
            ]))
            // Layout regressions inspect element bounds in development builds.
            .with_debug_info(std::env::var("PROFILE").as_deref() == Ok("debug"))
            .with_style("fluent-dark".into())
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("compile Nova Reader UI");
}

// Preserve the pinned Kit's complete segmented control. Only its repeated
// option's missing assistive-technology action is adapted for this host.
fn reader_segmented(shared: &std::path::Path, out: &std::path::Path) -> std::path::PathBuf {
    let source = shared.join("settings-controls.slint");
    println!("cargo:rerun-if-changed={}", source.display());
    let contents = std::fs::read_to_string(source).expect("read Kit settings controls");
    let start = contents
        .find("export component Segmented inherits HorizontalLayout {")
        .expect("Kit Segmented start");
    let end = contents[start..]
        .find("// Section title:")
        .expect("Kit Segmented end")
        + start;
    let old = "        accessible-checked: root.current_index == i;";
    let mut component = contents[start..end].to_owned();
    assert_eq!(
        component.matches(old).count(),
        1,
        "Kit Segmented contract changed"
    );
    component = component.replace(old, &format!("{old}\n        accessible-action-default => {{ if root.enabled && i != root.disabled_index {{ root.picked(i); }} }}"));
    let adapted = format!(
        "import {{ Theme, Anim, SettingsStyle, InterfaceStyle }} from \"@NovaUiShared\";\n{component}"
    );
    let destination = out.join("reader-segmented.slint");
    if std::fs::read_to_string(&destination).ok().as_deref() != Some(&adapted) {
        std::fs::write(&destination, adapted).expect("write adapted Kit Segmented");
    }
    destination
}

// Keep the pinned Kit implementation and its visuals. This revision limits
// keyboard input to TV and keeps popup opening private, so adapt only those
// gates plus accessible option activation until Kit exposes that host API.
fn reader_dropdown(shared: &std::path::Path, out: &std::path::Path) -> std::path::PathBuf {
    let source = shared.join("dropdown.slint");
    println!("cargo:rerun-if-changed={}", source.display());
    let mut contents = std::fs::read_to_string(&source).expect("read Kit dropdown");
    for (old, new) in [
        (
            "    function open_popup()",
            "    public function open_popup()",
        ),
        (
            "enabled: root.enabled && InterfaceStyle.tv;",
            "enabled: root.enabled;",
        ),
        (
            "border-width: InterfaceStyle.tv && index == root.popup-index",
            "border-width: SettingsStyle.keyboard-mode && index == root.popup-index",
        ),
        ("InterfaceStyle.tv && ", ""),
        (
            "if InterfaceStyle.tv { closed_scope.focus(); }",
            "closed_scope.focus();",
        ),
        (
            "if InterfaceStyle.tv { popup_scroll.content-y =",
            "if true { popup_scroll.content-y =",
        ),
        (
            "                            height: root.item-h;",
            r#"                            height: root.item-h;
                            accessible-role: button;
                            accessible-label: root.text_at(index);
                            accessible-action-default => {
                                root.current-index = index;
                                root.selected(value);
                                popup.close();
                                closed_scope.focus();
                            }"#,
        ),
    ] {
        assert!(
            contents.contains(old),
            "Kit dropdown contract changed: {old}"
        );
        contents = contents.replace(old, new);
    }
    let destination = out.join("reader-dropdown.slint");
    if std::fs::read_to_string(&destination).ok().as_deref() != Some(&contents) {
        std::fs::write(&destination, contents).expect("write adapted Kit dropdown");
    }
    destination
}

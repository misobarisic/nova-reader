fn main() {
    let shared = std::path::PathBuf::from(nova_ui_base::SLINT_SOURCE_DIR);
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let dropdown = reader_dropdown(&shared, &out);
    let segmented = reader_segmented(&shared, &out);
    let menu_sheet = reader_menu_sheet(&shared, &out);
    slint_build::compile_with_config(
        "app.slint",
        slint_build::CompilerConfiguration::new()
            .with_include_paths(vec![shared.clone()])
            .with_library_paths(std::collections::HashMap::from([
                ("NovaUiBase".into(), shared.clone()),
                ("ReaderDropdown".into(), dropdown),
                ("ReaderSegmented".into(), segmented),
                ("ReaderMenuSheet".into(), menu_sheet),
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

// Retain Nova's exact sheet visuals while wiring its semantic action targets.
fn reader_menu_sheet(shared: &std::path::Path, out: &std::path::Path) -> std::path::PathBuf {
    let source = shared.join("menusheet.slint");
    println!("cargo:rerun-if-changed={}", source.display());
    let mut contents = std::fs::read_to_string(source).expect("read Kit MenuSheet");
    contents = contents.replace(
        "from \"scrollview.slint\"",
        "from \"@NovaUiBase/scrollview.slint\"",
    );
    assert_eq!(
        contents.matches("accessible-enabled: it.enabled;").count(),
        2,
        "Kit MenuSheet semantic rows changed"
    );
    contents=contents.replace("accessible-enabled: it.enabled;", "accessible-enabled: it.enabled;\n                                    accessible-action-default => { if root.open && it.enabled { root.picked(i); } }");
    let cancel = "                    Rectangle {\n                        height: InterfaceStyle.tv ? 64px : 48px;";
    assert!(
        contents.contains(cancel),
        "Kit MenuSheet Cancel contract changed"
    );
    contents=contents.replace(cancel,&format!("{cancel}\n                        accessible-role: button; accessible-label: @tr(\"Cancel\");\n                        accessible-action-default => {{ if root.open {{ root.closed(); }} }}"));
    let destination = out.join("reader-menu-sheet.slint");
    if std::fs::read_to_string(&destination).ok().as_deref() != Some(&contents) {
        std::fs::write(&destination, contents).expect("write adapted Kit MenuSheet");
    }
    destination
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
    let action = "accessible-action-default => { if root.enabled && i != root.disabled_index { root.picked(i); } }";
    if !component.contains(action) {
        component = component.replace(old, &format!("{old}\n        {action}"));
    }
    assert_eq!(
        component.matches(action).count(),
        1,
        "Kit Segmented action drift"
    );
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
    // New Kit sources expose the same entry points additively. Older pins
    // still use the literal Nova adaptation below until their consumer updates.
    if contents.contains("in property <bool> keyboard-input-enabled: InterfaceStyle.tv;") {
        assert_eq!(contents.matches("public function open_popup()").count(), 1);
        assert_eq!(
            contents
                .matches("accessible-label: root.text_at(index);")
                .count(),
            1
        );
        assert_eq!(contents.matches("root.selected(value);").count(), 2);
        contents = contents.replace(
            "in property <bool> keyboard-input-enabled: InterfaceStyle.tv;",
            "in property <bool> keyboard-input-enabled: true;",
        );
        contents = contents.replace(
            "border-width: root.keyboard-input-enabled && index == root.popup-index",
            "border-width: SettingsStyle.keyboard-mode && index == root.popup-index",
        );
        let destination = out.join("reader-dropdown.slint");
        if std::fs::read_to_string(&destination).ok().as_deref() != Some(&contents) {
            std::fs::write(&destination, contents).expect("write adapted Kit dropdown");
        }
        return destination;
    }
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

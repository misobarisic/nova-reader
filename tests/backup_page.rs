//! The Data page: backup choices dispatch to the host, rows fit phone and
//! desktop widths, and the shared confirmation sheet carries the host's wording.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
use nova_reader::{Anim, AppWindow, BackupBridge, BackupMapping, BackupOption};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn option(id: &str, label: &str, checked: bool) -> BackupOption {
    BackupOption {
        id: id.into(),
        label: label.into(),
        detail: "".into(),
        checked,
    }
}

#[test]
fn the_data_page_dispatches_export_and_import_choices_on_phone_and_desktop() {
    i_slint_backend_testing::init_no_event_loop();
    for width in [390., 1120.] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.window().set_size(slint::LogicalSize::new(width, 6000.));
        ui.set_section(12);
        let bridge = ui.global::<BackupBridge>();
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));
        macro_rules! plain {
            ($on:ident, $text:expr) => {{
                let calls = calls.clone();
                bridge.$on(move || calls.borrow_mut().push($text.to_string()));
            }};
        }
        macro_rules! record {
            ($on:ident, |$($arg:ident),*| $format:expr) => {{
                let calls = calls.clone();
                bridge.$on(move |$($arg),*| calls.borrow_mut().push($format));
            }};
        }
        plain!(on_opened, "opened");
        record!(on_export_preset, |n| format!("preset {n}"));
        record!(on_export_toggle, |kind, id| format!("export {kind} {id}"));
        plain!(on_export_file, "export");
        plain!(on_import_file, "choose");
        record!(on_import_toggle, |kind, id| format!("import {kind} {id}"));
        record!(on_cycle_target, |id| format!("target {id}"));
        record!(on_mode_changed, |replace| format!("replace {replace}"));
        plain!(on_build_preview, "preview");
        plain!(on_apply_import, "apply");
        plain!(on_undo, "undo");
        plain!(on_delete_recovery, "delete");
        record!(on_confirm_choice, |choice| format!("confirm {choice}"));
        bridge.set_export_profiles(ModelRc::new(VecModel::from(vec![option(
            "reader", "Default", true,
        )])));
        bridge.set_export_components(ModelRc::new(VecModel::from(vec![
            option("library", "Library", true),
            option("addon_secrets", "Addon credentials", true),
        ])));
        bridge.set_export_device(ModelRc::new(VecModel::from(vec![option(
            "packages",
            "Addon packages",
            false,
        )])));
        bridge.set_can_undo(true);
        ui.show().unwrap();
        mock_elapsed_time(Duration::from_millis(100));
        let tap = |label: &str, role: AccessibleRole| {
            ElementHandle::find_by_accessible_label(&ui, label)
                .find(|e| e.accessible_role() == Some(role))
                .unwrap_or_else(|| panic!("{label} at {width}px"))
                .invoke_accessible_default_action();
            mock_elapsed_time(Duration::from_millis(50));
        };
        let click = |label: &str| tap(label, AccessibleRole::Button);
        let toggle = |label: &str| tap(label, AccessibleRole::Checkbox);
        assert_eq!(*calls.borrow(), ["opened"], "the page announces itself");

        // The plain-text warning is on the page that offers the export.
        assert!(
            ElementHandle::find_by_accessible_label(
                &ui,
                "Save your profiles and this device's data as one file. Addon credentials are saved in plain text, so keep the file private. Sync identity, paired devices and caches are never included."
            )
            .next()
            .is_some()
        );
        toggle("Library");
        toggle("Addon packages");
        toggle("Addon credentials");
        click("Export backup");
        click("Choose backup file");
        click("Undo last import");
        click("Delete recovery copies");
        assert_eq!(
            calls.borrow()[1..],
            [
                "export component library",
                "export device packages",
                "export component addon_secrets",
                "export",
                "choose",
                "undo",
                "delete",
            ]
        );
        // Every action stays inside the window on a phone.
        for label in ["Export backup", "Choose backup file"] {
            let button = ElementHandle::find_by_accessible_label(&ui, label)
                .next()
                .unwrap();
            let (x, size) = (button.absolute_position().x, button.size().width);
            assert!(
                x >= 0. && x + size <= width + 0.5,
                "{label}: {x} + {size} in {width}"
            );
        }

        // An open file shows its mapping, mode and review, and gates the import.
        calls.borrow_mut().clear();
        bridge.set_importing(true);
        bridge.set_file_label("nova-reader.nova-reader-backup".into());
        bridge.set_profiles(ModelRc::new(VecModel::from(vec![option(
            "p0", "Default", true,
        )])));
        bridge.set_mappings(ModelRc::new(VecModel::from(vec![BackupMapping {
            id: "p0".into(),
            label: "Default →".into(),
            target: "Into profile: Default".into(),
        }])));
        bridge.set_components(ModelRc::new(VecModel::from(vec![option(
            "progress",
            "Reading progress",
            true,
        )])));
        bridge.set_conflicts(ModelRc::new(VecModel::from(vec![option(
            "progress",
            "Reading progress",
            false,
        )])));
        mock_elapsed_time(Duration::from_millis(100));
        click("Change");
        toggle("Replace selected data");
        click("Review changes");
        assert_eq!(*calls.borrow(), ["target p0", "replace true", "preview"]);
        bridge.set_preview_ready(true);
        bridge.set_preview_rows(ModelRc::new(VecModel::from(vec![
            "Default · Reading progress: 1 in backup, 1 new".into(),
        ])));
        mock_elapsed_time(Duration::from_millis(100));
        calls.borrow_mut().clear();
        click("Import reviewed changes");
        assert_eq!(*calls.borrow(), ["apply"]);

        // The host asks through the shared sheet; its confirm row answers 0.
        bridge.set_confirm_message(
            "Import these changes? They sync to your paired devices, and any removals do too."
                .into(),
        );
        bridge.set_confirm_label("Import".into());
        bridge.set_confirm_open(true);
        mock_elapsed_time(Duration::from_millis(100));
        assert!(
            ElementHandle::find_by_accessible_label(
                &ui,
                "Import these changes? They sync to your paired devices, and any removals do too."
            )
            .next()
            .is_some()
        );
    }
}

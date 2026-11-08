use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{
    AppWindow,
    reader_modes::{self, ModeChoice},
};
use reader_domain::{ReadingMode, SeriesId};
use slint::ComponentHandle;
use std::{process::Command, time::Duration};

#[test]
fn per_series_manual_modes_migrate_and_survive_restart_without_progress_override() {
    const PHASE: &str = "READER_MODE_PHASE";
    const DIRECTORY: &str = "READER_MODE_DIRECTORY";
    if let Ok(phase) = std::env::var(PHASE) {
        let a = SeriesId::new("source-a", "same-local");
        let b = SeriesId::new("source-b", "same-local");
        let migrated = SeriesId::new("legacy", "same-local");
        if phase == "unavailable" {
            assert!(reader_modes::save(&a, ModeChoice::Manual(ReadingMode::Vertical)).is_err());
            return;
        }
        nova_storage::init_at(std::path::Path::new(&std::env::var(DIRECTORY).unwrap()));
        match phase.as_str() {
            "write" => {
                assert_eq!(reader_modes::load(&a, None).unwrap(), ModeChoice::Default);
                reader_modes::save(&a, ModeChoice::Manual(ReadingMode::RightToLeft)).unwrap();
                reader_modes::save(&b, ModeChoice::Manual(ReadingMode::Webtoon)).unwrap();
                assert_eq!(
                    reader_modes::load(&migrated, Some(ReadingMode::Vertical)).unwrap(),
                    ModeChoice::Manual(ReadingMode::Vertical)
                );
            }
            "reload" => {
                assert_eq!(
                    reader_modes::load(&a, Some(ReadingMode::LeftToRight)).unwrap(),
                    ModeChoice::Manual(ReadingMode::RightToLeft)
                );
                assert_eq!(
                    reader_modes::load(&b, None).unwrap(),
                    ModeChoice::Manual(ReadingMode::Webtoon)
                );
                assert_eq!(
                    reader_modes::load(&migrated, None).unwrap(),
                    ModeChoice::Manual(ReadingMode::Vertical)
                );
                // A completed async chapter-open must reread, not use this old
                // Default snapshot after a newer manual choice was saved.
                let stale = reader_modes::load(&SeriesId::new("late", "same-local"), None).unwrap();
                let late = SeriesId::new("late", "same-local");
                reader_modes::save(&late, ModeChoice::Manual(ReadingMode::LeftToRight)).unwrap();
                assert_eq!(stale, ModeChoice::Default);
                assert_eq!(
                    reader_modes::load(&late, None)
                        .unwrap()
                        .resolve(ReadingMode::Webtoon),
                    ReadingMode::LeftToRight
                );
                reader_modes::save(&migrated, ModeChoice::Automatic).unwrap();
            }
            "automatic" => {
                assert_eq!(
                    reader_modes::load(&migrated, Some(ReadingMode::Vertical)).unwrap(),
                    ModeChoice::Automatic
                );
                assert_eq!(
                    reader_modes::load(&a, None).unwrap(),
                    ModeChoice::Manual(ReadingMode::RightToLeft)
                );
                assert_eq!(
                    reader_modes::load(&b, None).unwrap(),
                    ModeChoice::Manual(ReadingMode::Webtoon)
                );
                let invalid = SeriesId::new("invalid", "same-local");
                let key = format!(
                    "reader:mode-choice:{}",
                    serde_json::to_string(&invalid).unwrap()
                );
                for raw in [
                    "broken json",
                    r#"{"schema_version":999,"choice":"Automatic"}"#,
                ] {
                    nova_storage::try_set_str(&key, raw).unwrap();
                    assert_eq!(
                        reader_modes::load(&invalid, Some(ReadingMode::Vertical)).unwrap(),
                        ModeChoice::Default
                    );
                    assert_eq!(
                        nova_storage::try_get_str(&key).unwrap().as_deref(),
                        Some(raw)
                    );
                }
                reader_modes::save(&invalid, ModeChoice::Manual(ReadingMode::LeftToRight)).unwrap();
                assert_eq!(
                    reader_modes::load(&invalid, None).unwrap(),
                    ModeChoice::Manual(ReadingMode::LeftToRight)
                );
            }
            _ => panic!("unknown phase"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for phase in ["unavailable", "write", "reload", "automatic"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "per_series_manual_modes_migrate_and_survive_restart_without_progress_override",
                "--nocapture",
            ])
            .env(PHASE, phase)
            .env(DIRECTORY, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{phase}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
fn settle() {
    for _ in 0..12 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing {label}"))
}
/// A mode tile, scrolling the settings form to it first: on a small window
/// the half-height sheet shows only part of the grid, and clipped elements are
/// not searchable.
fn tile(ui: &AppWindow, label: &str) -> ElementHandle {
    let find = || {
        ElementHandle::find_by_accessible_label(ui, label)
            .find(|e| e.accessible_role() == Some(AccessibleRole::RadioButton))
    };
    for direction in [-60., 60.] {
        for _ in 0..12 {
            if let Some(tile) = find() {
                return tile;
            }
            if let Some(scroll) =
                ElementHandle::find_by_element_id(ui, "ReaderSettingsForm::settings-scroll").next()
            {
                scroll.scroll(0., direction);
            }
            settle();
        }
    }
    panic!("missing tile {label}")
}
#[test]
fn mode_tiles_send_the_dropdown_values_and_show_the_saved_choice() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let picked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    ui.on_mode_changed({
        let picked = picked.clone();
        move |mode| picked.borrow_mut().push(mode)
    });
    ui.set_section(6);
    ui.set_reader_osd(true);
    ui.show().unwrap();
    for (width, height) in [(320., 480.), (640., 240.)] {
        ui.window().set_size(slint::LogicalSize::new(width, height));
        ui.set_reader_mode_choice(4);
        ui.set_reading_mode(0);
        settle();
        ui.set_reader_osd(true);
        settle();
        button(&ui, "Settings").invoke_accessible_default_action();
        settle();
        // Each tile sends the mode code the dropdown used to send.
        picked.borrow_mut().clear();
        for label in [
            "Automatic",
            "Left to right",
            "Right to left",
            "Vertical",
            "Webtoon",
            "With gaps",
        ] {
            tile(&ui, label).invoke_accessible_default_action();
            settle();
        }
        tile(&ui, "Use default").invoke_accessible_default_action();
        settle();
        assert_eq!(*picked.borrow(), vec![4, 1, 2, 3, 0, 5, 6]);
        // The selection reflects the saved series choice independently of page
        // progress, and keeps inheritance separate from explicit Automatic.
        // (Every tile was reached above; a taller window shows them all.)
        ui.window().set_size(slint::LogicalSize::new(width, 1200.));
        ui.set_reader_mode_choice(2);
        ui.set_reading_mode(2);
        settle();
        assert_eq!(tile(&ui, "Right to left").accessible_checked(), Some(true));
        assert_eq!(tile(&ui, "Automatic").accessible_checked(), Some(false));
        assert_eq!(tile(&ui, "Use default").accessible_checked(), Some(false));
        ui.set_reader_mode_choice(6);
        settle();
        assert_eq!(tile(&ui, "Use default").accessible_checked(), Some(true));
        assert_eq!(tile(&ui, "Automatic").accessible_checked(), Some(false));
        button(&ui, "Close reader settings").invoke_accessible_default_action();
        settle();
    }
}

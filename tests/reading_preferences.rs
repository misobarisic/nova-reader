use nova_reader::{AppWindow, reading_preferences};
use reader_domain::SeriesId;
use slint::ComponentHandle;
use std::{cell::Cell, process::Command, rc::Rc};

fn toggle_adult(ui: &AppWindow) {
    ui.set_section(3);
    ui.show().unwrap();
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
    // The switch lives in Browse's ⋯ menu.
    i_slint_backend_testing::ElementHandle::find_by_accessible_label(ui, "Browse options")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
    i_slint_backend_testing::ElementHandle::find_by_accessible_label(ui, "Show adult sources")
        .find(|element| element.accessible_checkable() == Some(true))
        .unwrap()
        .invoke_accessible_default_action();
    ui.hide().unwrap();
}

#[test]
fn visibility_and_full_series_chapter_order_survive_process_restarts() {
    const MODE: &str = "READER_READING_PREFERENCES_MODE";
    const DIRECTORY: &str = "READER_READING_PREFERENCES_DIRECTORY";
    if let Ok(mode) = std::env::var(MODE) {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let first = SeriesId::new("source-a", "same-local-id");
        let second = SeriesId::new("source-b", "same-local-id");
        if mode == "unavailable" {
            reading_preferences::init(&ui).unwrap();
            assert!(!ui.get_show_adult());
            assert!(reading_preferences::chapter_descending(&first));
            assert!(reading_preferences::set_chapter_descending(&first, false).is_err());
            toggle_adult(&ui);
            assert!(!ui.get_show_adult());
            assert!(!ui.get_status().is_empty());
            return;
        }
        nova_storage::init_at(std::path::Path::new(&std::env::var(DIRECTORY).unwrap()));
        reading_preferences::init(&ui).unwrap();
        let routes = Rc::new(Cell::new(0));
        ui.on_navigate({
            let routes = routes.clone();
            move |section| {
                assert_eq!(section, 3);
                routes.set(routes.get() + 1);
            }
        });
        match mode.as_str() {
            "write-true" => {
                assert!(!ui.get_show_adult());
                assert!(reading_preferences::chapter_descending(&first));
                toggle_adult(&ui);
                assert!(ui.get_show_adult());
                assert_eq!(routes.get(), 1);
                reading_preferences::set_chapter_descending(&first, false).unwrap();
                reading_preferences::set_chapter_descending(&second, true).unwrap();
            }
            "reload-true" => {
                assert!(ui.get_show_adult());
                assert!(!reading_preferences::chapter_descending(&first));
                assert!(reading_preferences::chapter_descending(&second));
            }
            "write-false" => {
                toggle_adult(&ui);
                assert!(!ui.get_show_adult());
                reading_preferences::set_chapter_descending(&first, true).unwrap();
                reading_preferences::set_chapter_descending(&second, false).unwrap();
            }
            "reload-false" => {
                assert!(!ui.get_show_adult());
                assert!(reading_preferences::chapter_descending(&first));
                assert!(!reading_preferences::chapter_descending(&second));
            }
            "corrupt" => {
                let order_key = format!(
                    "reader:chapter-order:{}",
                    serde_json::to_string(&first).unwrap()
                );
                for raw in ["broken JSON", "null", "1", "\"true\"", "{}", "[]"] {
                    nova_storage::try_set_str("reader:show-adult", raw).unwrap();
                    nova_storage::try_set_str(&order_key, raw).unwrap();
                    let recovered = AppWindow::new().unwrap();
                    reading_preferences::init(&recovered).unwrap();
                    assert!(!recovered.get_show_adult());
                    assert!(reading_preferences::chapter_descending(&first));
                    assert_eq!(
                        nova_storage::try_get_str("reader:show-adult")
                            .unwrap()
                            .as_deref(),
                        Some(raw)
                    );
                    assert_eq!(
                        nova_storage::try_get_str(&order_key).unwrap().as_deref(),
                        Some(raw)
                    );
                    recovered.invoke_adult_visibility_changed(true);
                    reading_preferences::set_chapter_descending(&first, false).unwrap();
                    assert!(reading_preferences::show_adult());
                    assert!(!reading_preferences::chapter_descending(&first));
                    assert!(!reading_preferences::chapter_descending(&second));
                }
            }
            _ => panic!("unexpected mode {mode}"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for mode in [
        "write-true",
        "reload-true",
        "write-false",
        "reload-false",
        "corrupt",
        "unavailable",
    ] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "visibility_and_full_series_chapter_order_survive_process_restarts",
                "--nocapture",
            ])
            .env(MODE, mode)
            .env(DIRECTORY, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

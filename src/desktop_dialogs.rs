//! Nova's native addon picker worker; only bounded bytes reach the installer.

use crate::AppWindow;
use nova_reader_ui::backend_text as text;
use slint::ComponentHandle;

const MAX_ZIP_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn pick_zip(ui: &AppWindow, install: impl FnOnce(&AppWindow, Vec<u8>) + Send + 'static) {
    // Nova uses a picker-specific guard, not the application's loading state.
    // Browsing/loading a source must not silently suppress this action.
    if ui.get_addon_zip_import_busy() {
        return;
    }
    ui.set_addon_zip_import_busy(true);
    ui.set_status(text::tr("Choose an addon ZIP file…").into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        use std::io::Read;
        let Some(path) = rfd::FileDialog::new()
            .set_title(text::tr("Choose an addon ZIP file…"))
            .add_filter("ZIP", &["zip"])
            .pick_file()
        else {
            deliver(weak, Ok(None), install);
            return;
        };
        let status_window = weak.clone();
        let _ = status_window.upgrade_in_event_loop(move |ui| {
            ui.set_status(text::tr("Reading addon ZIP…").into());
        });
        let result = (|| {
            let file = std::fs::File::open(path).map_err(|_| "The addon ZIP could not be read")?;
            let mut bytes = Vec::new();
            file.take((MAX_ZIP_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| "The addon ZIP could not be read")?;
            if bytes.len() > MAX_ZIP_BYTES {
                return Err("The addon ZIP exceeds the 8 MiB size limit");
            }
            Ok(Some(bytes))
        })();
        deliver(weak, result, install);
    });
}

fn deliver(
    weak: slint::Weak<AppWindow>,
    result: Result<Option<Vec<u8>>, &'static str>,
    install: impl FnOnce(&AppWindow, Vec<u8>) + Send + 'static,
) {
    // The weak window is Reader's equivalent of Nova's current-root check:
    // an old dialog cannot install into a replacement application window.
    let _ = weak.upgrade_in_event_loop(move |ui| {
        ui.set_addon_zip_import_busy(false);
        match result {
            Ok(Some(bytes)) => install(&ui, bytes),
            Ok(None) => ui.set_status("".into()),
            Err(message) => ui.set_status(text::tr(message).into()),
        }
    });
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    #[test]
    fn native_picker_runs_while_loading_and_delivers_selection_cancel_and_errors() {
        const CASE: &str = "READER_PICKER_TEST_CASE";
        let Ok(case) = std::env::var(CASE) else {
            for case in ["selected", "cancelled", "oversized", "unreadable"] {
                let fixture = tempfile::tempdir().unwrap();
                let selected = fixture.path().join("chosen addon.zip");
                let calls = fixture.path().join("picker-calls");
                let script = fixture.path().join("zenity");
                std::fs::write(
                    &script,
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$READER_PICKER_CALLS\"\nif [ \"$READER_PICKER_TEST_CASE\" = cancelled ]; then exit 1; fi\nprintf '%s\\n' \"$READER_PICKER_SELECTED\"\n",
                )
                .unwrap();
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
                match case {
                    "selected" => std::fs::write(&selected, b"chosen addon bytes").unwrap(),
                    "oversized" => std::fs::File::create(&selected)
                        .unwrap()
                        .set_len(MAX_ZIP_BYTES as u64 + 1)
                        .unwrap(),
                    _ => {}
                }
                // Exercise rfd itself, including its ZIP filter and native
                // fallback. Each child has its own bus/environment/event loop.
                assert!(
                    std::process::Command::new(std::env::current_exe().unwrap())
                        .args([
                            "--exact",
                            "desktop_dialogs::tests::native_picker_runs_while_loading_and_delivers_selection_cancel_and_errors",
                            "--nocapture",
                        ])
                        .env(CASE, case)
                        .env("PATH", fixture.path())
                        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-reader-test-bus")
                        .env("READER_PICKER_SELECTED", &selected)
                        .env("READER_PICKER_CALLS", &calls)
                        .status()
                        .unwrap()
                        .success(),
                    "picker case {case} failed"
                );
                let calls = std::fs::read_to_string(calls).unwrap();
                assert_eq!(calls.matches("--file-selection").count(), 1);
                assert!(calls.contains("ZIP | *.zip"));
            }
            return;
        };

        i_slint_backend_testing::init_integration_test_with_system_time();
        let ui = AppWindow::new().unwrap();
        ui.set_busy(true); // General application loading cannot swallow a click.
        let installed = Arc::new(AtomicBool::new(false));
        let selected = installed.clone();
        pick_zip(&ui, move |ui, bytes| {
            assert_eq!(bytes, b"chosen addon bytes");
            assert!(ui.get_busy());
            assert!(!ui.get_addon_zip_import_busy());
            selected.store(true, Ordering::Release);
            ui.set_status("picked".into());
        });
        assert!(ui.get_addon_zip_import_busy());
        pick_zip(&ui, |_, _| panic!("duplicate picker must be suppressed"));
        ui.set_section(4); // Ordinary navigation does not cancel the picker.
        let weak = ui.as_weak();
        let completion = slint::Timer::default();
        completion.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(10),
            move || {
                if !weak.unwrap().get_addon_zip_import_busy() {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        slint::Timer::single_shot(Duration::from_secs(5), || {
            panic!("native addon picker did not complete")
        });
        slint::run_event_loop().unwrap();
        assert!(ui.get_busy(), "picker must not own general loading state");
        assert_eq!(installed.load(Ordering::Acquire), case == "selected");
        assert_eq!(
            ui.get_status().as_str(),
            match case.as_str() {
                "selected" => "picked",
                "cancelled" => "",
                "oversized" => "The addon ZIP exceeds the 8 MiB size limit",
                "unreadable" => "The addon ZIP could not be read",
                _ => unreachable!(),
            }
        );
    }
}

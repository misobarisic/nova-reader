//! Native About/ListView regression, using Nova's headless measurement recipe.
//! The settle helper is copied from Nova tests/settings_navigation/about.rs at
//! b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda (GPL-3.0-or-later).
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use nova_reader::{AppWindow, LicenseSource};
use slint::{ComponentHandle, LogicalPosition, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};

fn settle(app: &AppWindow) {
    // The mock backend has no paint pass to resolve virtual ListView height
    // estimates. Measure its exposed delegates before advancing queued work.
    for _ in 0..16 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
    }
}

fn wheel(app: &AppWindow, pane: &str, delta_y: f32) {
    let pane = ElementHandle::find_by_element_id(app, pane).next().unwrap();
    let position = pane.absolute_position();
    let size = pane.size();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: LogicalPosition::new(
                position.x + size.width / 2.,
                position.y + size.height / 2.,
            ),
            delta_x: 0.,
            delta_y,
        });
    // Nova's wheel physics completes independently of the animation setting.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(400));
    settle(app);
}

fn reveal(app: &AppWindow, pane_id: &str, id: &str) -> ElementHandle {
    for attempt in 0..64 {
        let pane = ElementHandle::find_by_element_id(app, pane_id)
            .next()
            .unwrap();
        let mut direction = if attempt < 24 { -40. } else { 40. };
        if let Some(button) = ElementHandle::find_by_element_id(app, id).next() {
            let y = button.absolute_position().y;
            let top = pane.absolute_position().y;
            if y >= top - 0.5 && y + button.size().height <= top + pane.size().height + 0.5 {
                return button;
            }
            direction = if y < top { 40. } else { -40. };
        }
        wheel(app, pane_id, direction);
    }
    panic!("could not reveal {id}");
}

fn back_key(app: &AppWindow) {
    for event in [
        slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        app.window().dispatch_event(event);
    }
    settle(app);
}

#[test]
fn copied_about_exposes_full_licenses_sources_and_independent_back_navigation() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().unwrap();
    app.on_navigate({
        let weak = app.as_weak();
        move |section| weak.unwrap().set_section(section)
    });
    let opened = Rc::new(RefCell::new(Vec::new()));
    app.on_license_source_requested({
        let opened = opened.clone();
        move |url| opened.borrow_mut().push(url.to_string())
    });
    app.set_app_version("0.1.16-test".into());
    app.set_section(13);
    app.window().set_size(slint::LogicalSize::new(320., 640.));
    app.show().unwrap();
    settle(&app);
    assert_eq!(app.get_license_text_chunks().row_count(), 0);
    reveal(
        &app,
        "AboutPage::about_info_scroll",
        "AboutPage::about_licenses_button",
    )
    .invoke_accessible_default_action();
    settle(&app);
    assert!(app.get_about_licenses_open());
    assert_eq!(app.get_section(), 13);

    // The copied startup worker can deliver after the page is already open.
    // Publishing models must preserve the pane, its native list, and all text.
    let mut chunks = (0..16)
        .map(|index| {
            format!(
                "LICENSE SECTION {index}\n{}",
                "Permission and copyright notice.\n".repeat(8)
            )
        })
        .collect::<Vec<_>>();
    chunks.push("FINAL LEGAL NOTICE — ž漢🙂 — complete notice remains available.".into());
    let original = chunks.concat();
    app.set_license_text_chunks(ModelRc::new(VecModel::from(
        chunks
            .into_iter()
            .map(Into::into)
            .collect::<Vec<slint::SharedString>>(),
    )));
    app.set_license_sources(ModelRc::new(VecModel::from(vec![
        LicenseSource {
            label: "Nova Reader".into(),
            license: "GPL-3.0-or-later".into(),
            url: "https://github.com/misobarisic/nova-reader".into(),
        },
        LicenseSource {
            label: "Slint 1.18.0 and contributors".into(),
            license:
                "GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-commercial"
                    .into(),
            url: "https://crates.io/crates/slint/1.18.0".into(),
        },
    ])));
    settle(&app);
    assert!(app.get_about_licenses_open());
    assert_eq!(
        app.get_license_text_chunks()
            .iter()
            .map(|chunk| chunk.to_string())
            .collect::<String>(),
        original
    );

    for (width, height) in [(844., 240.), (320., 640.), (1120., 800.)] {
        app.invoke_go_back();
        settle(&app);
        assert!(!app.get_about_licenses_open());
        assert_eq!(app.get_section(), 13);
        app.set_about_licenses_scroll_y(0.);
        app.window()
            .set_size(slint::LogicalSize::new(width, height));
        settle(&app);
        reveal(
            &app,
            "AboutPage::about_info_scroll",
            "AboutPage::about_licenses_button",
        )
        .invoke_accessible_default_action();
        settle(&app);
        let source = reveal(
            &app,
            "AboutPage::about_scroll",
            "AboutPage::source_view_button",
        );
        let position = source.absolute_position();
        assert!(source.size().height >= 44.);
        assert!(position.x >= 0. && position.x + source.size().width <= width + 0.5);
        let before = opened.borrow().len();
        source.invoke_accessible_default_action();
        assert_eq!(opened.borrow().len(), before + 1);
        assert!(opened.borrow().last().unwrap().starts_with("https://"));
        if width > 1000. {
            assert_eq!(
                opened.borrow().last().unwrap(),
                "https://github.com/misobarisic/nova-reader"
            );
            let second = ElementHandle::find_by_element_id(&app, "AboutPage::source_view_button")
                .nth(1)
                .unwrap();
            second.mock_single_click(slint::platform::PointerEventButton::Left);
            assert_eq!(
                opened.borrow().last().unwrap(),
                "https://crates.io/crates/slint/1.18.0"
            );
        }
        for row in ElementHandle::find_by_element_id(&app, "AboutPage::source_card") {
            assert!(row.absolute_position().x >= -0.5);
            assert!(row.absolute_position().x + row.size().width <= width + 0.5);
        }
        for _ in 0..24 {
            wheel(&app, "AboutPage::about_scroll", -100_000_000.);
            if ElementHandle::find_by_accessible_label(
                &app,
                "FINAL LEGAL NOTICE — ž漢🙂 — complete notice remains available.",
            )
            .next()
            .is_some()
            {
                break;
            }
        }
        let notice = ElementHandle::find_by_accessible_label(
            &app,
            "FINAL LEGAL NOTICE — ž漢🙂 — complete notice remains available.",
        )
        .next()
        .expect("full license tail is accessible");
        assert!(notice.absolute_position().y >= 0.);
        assert!(notice.absolute_position().y + notice.size().height <= height + 0.5);
        let legal_scroll = app.get_about_licenses_scroll_y();
        let about_scroll = app.get_about_scroll_y();
        back_key(&app);
        assert_eq!(app.get_section(), 13);
        assert!(!app.get_about_licenses_open());
        assert!((app.get_about_scroll_y() - about_scroll).abs() < 1.);
        reveal(
            &app,
            "AboutPage::about_info_scroll",
            "AboutPage::about_licenses_button",
        )
        .invoke_accessible_default_action();
        settle(&app);
        assert!(
            (app.get_about_licenses_scroll_y() - legal_scroll).abs() < 1.,
            "legal scroll restores independently"
        );
    }
    back_key(&app);
    assert_eq!(app.get_section(), 13);
    back_key(&app);
    assert_eq!(app.get_section(), 4);
}

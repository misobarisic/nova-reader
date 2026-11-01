use std::{cell::RefCell, rc::Rc, time::Duration};

use i_slint_backend_testing::ElementHandle;
use nova_reader::{Anim, AppWindow, ReaderSyncSettings, SyncPeer};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel, platform::PointerEventButton};

fn settle() {
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn click(ui: &AppWindow, label: &str) {
    for _ in 0..60 {
        if let Some(button) = ElementHandle::find_by_accessible_label(ui, label).next() {
            let y = button.absolute_position().y + button.size().height / 2.;
            let height = ui.window().size().height as f32 / ui.window().scale_factor();
            if y > 0. && y < height - 70. {
                button.mock_single_click(PointerEventButton::Left);
                settle();
                return;
            }
        }
        ElementHandle::find_by_element_id(ui, "MorePage::detail-scroll")
            .next()
            .expect("sync settings document")
            .scroll(0., -160.);
        settle();
    }
    panic!("sync action not reachable: {label}");
}

#[test]
fn shared_sync_page_fits_phone_and_desktop_and_dispatches_pairing_actions() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    let sync = ui.global::<ReaderSyncSettings>();
    sync.set_sync_enabled(true);
    sync.set_sync_device_name("Reader device".into());
    sync.set_sync_identity("d".repeat(64).into());
    sync.set_sync_status("Waiting for paired device".into());
    sync.set_sync_pair_prompt_visible(true);
    sync.set_sync_pair_prompt_id("request".into());
    sync.set_sync_pair_prompt_name("A".repeat(200).into());
    sync.set_sync_pair_prompt_code("123456".into());
    sync.set_sync_peers(ModelRc::new(VecModel::from(vec![SyncPeer {
        id: "e".repeat(64).into(),
        name: "Other Reader".into(),
        last_seen: "Never connected".into(),
    }])));
    let events = Rc::new(RefCell::new(Vec::new()));
    sync.on_sync_respond_pair({
        let events = events.clone();
        move |id, accepted| events.borrow_mut().push((id.to_string(), accepted))
    });
    sync.on_sync_create_invite({
        let events = events.clone();
        move || events.borrow_mut().push(("invite".into(), true))
    });
    ui.set_section(20);
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1280., 900.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        settle();
        for kind in ["PillButton", "SearchField", "SettingsRow"] {
            for element in ElementHandle::find_by_element_type_name(&ui, kind) {
                let position = element.absolute_position();
                let size = element.size();
                assert!(
                    position.x >= -0.5 && position.x + size.width <= width + 0.5,
                    "{kind} overflows {width}px: {position:?} {size:?}"
                );
            }
        }
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Allow remote control")
                .next()
                .is_none()
        );
        click(&ui, "Accept");
        click(&ui, "Deny");
    }
    assert_eq!(
        &*events.borrow(),
        &[
            ("request".into(), true),
            ("request".into(), false),
            ("request".into(), true),
            ("request".into(), false)
        ]
    );
    click(&ui, "Create invite");
    assert_eq!(events.borrow().last().unwrap().0, "invite");
}

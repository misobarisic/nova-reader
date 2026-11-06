//! The Profiles page in More: opening, editing, confirmation, the startup
//! picker and Back handling, driven through the real accessibility tree.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, ProfileRow, ProfilesBridge};
use slint::platform::{Key, WindowEvent, WindowEventDispatchResult};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..40 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn row(id: &str, name: &str, active: bool) -> ProfileRow {
    ProfileRow {
        id: id.into(),
        name: name.into(),
        active,
        initial: name.chars().next().unwrap().to_string().into(),
        avatar_background: slint::Color::from_rgb_u8(30, 60, 120),
        avatar_foreground: slint::Color::from_rgb_u8(255, 255, 255),
    }
}
fn press(ui: &AppWindow, key: Key) -> WindowEventDispatchResult {
    let text: slint::SharedString = key.into();
    let result = ui
        .window()
        .dispatch_event_with_result(WindowEvent::KeyPressed { text: text.clone() })
        .unwrap();
    ui.window()
        .dispatch_event_with_result(WindowEvent::KeyReleased { text })
        .unwrap();
    settle();
    result
}
#[track_caller]
fn find(ui: &AppWindow, label: &str) -> ElementHandle {
    settle();
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .unwrap_or_else(|| panic!("missing element {label}"))
}
#[track_caller]
fn tap(ui: &AppWindow, label: &str) {
    find(ui, label).invoke_accessible_default_action();
    settle();
}
/// Buttons carrying `label`, once each (a composed control can expose its
/// label on more than one node).
fn buttons(ui: &AppWindow, label: &str) -> Vec<ElementHandle> {
    settle();
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .collect()
}
fn exists(ui: &AppWindow, label: &str) -> bool {
    settle();
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .is_some()
}

struct Page {
    ui: AppWindow,
    routes: Rc<RefCell<Vec<i32>>>,
    events: Rc<RefCell<Vec<String>>>,
}
fn page(required: bool) -> Page {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let routes = Rc::new(RefCell::new(Vec::new()));
    let events = Rc::new(RefCell::new(Vec::new()));
    ui.on_navigate({
        let weak = ui.as_weak();
        let routes = routes.clone();
        move |section| {
            routes.borrow_mut().push(section);
            // The host refuses to leave the startup picker before a choice.
            let ui = weak.unwrap();
            if !(ui.global::<ProfilesBridge>().get_require_selection() && section != 24) {
                ui.set_section(section);
            }
        }
    });
    let bridge = ui.global::<ProfilesBridge>();
    bridge.set_profiles(ModelRc::new(VecModel::from(vec![
        row("reader", "Mina", true),
        row("p1_1", "Kid", false),
    ])));
    bridge.set_active_name("Mina".into());
    bridge.set_require_selection(required);
    bridge.on_create_profile({
        let events = events.clone();
        move |name| events.borrow_mut().push(format!("create:{name}"))
    });
    bridge.on_rename_profile({
        let events = events.clone();
        move |id, name| events.borrow_mut().push(format!("rename:{id}:{name}"))
    });
    bridge.on_select_profile({
        let events = events.clone();
        move |id| events.borrow_mut().push(format!("select:{id}"))
    });
    bridge.on_delete_profile({
        let events = events.clone();
        move |id| events.borrow_mut().push(format!("delete:{id}"))
    });
    bridge.on_remember_last_changed({
        let events = events.clone();
        move |enabled| events.borrow_mut().push(format!("remember:{enabled}"))
    });
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    ui.set_section(4);
    settle();
    Page { ui, routes, events }
}

#[test]
fn more_opens_profiles_and_back_returns_to_more() {
    let Page { ui, routes, .. } = page(false);
    tap(&ui, "Profiles");
    assert_eq!(routes.borrow().last(), Some(&24));
    assert_eq!(ui.get_section(), 24);
    assert!(exists(&ui, "Current profile: Mina"));
    assert!(exists(&ui, "Use profile: Kid"));
    // Android's system Back (and the on-screen header) leave for More.
    assert_eq!(press(&ui, Key::Back), WindowEventDispatchResult::Accepted);
    assert_eq!(ui.get_section(), 4);
}

#[test]
fn choosing_a_profile_asks_the_host_to_switch() {
    let Page { ui, events, .. } = page(false);
    ui.set_section(24);
    tap(&ui, "Use profile: Kid");
    assert_eq!(events.borrow().as_slice(), ["select:p1_1"]);
}

#[test]
fn creating_and_renaming_validate_nothing_locally_and_report_the_name() {
    let Page { ui, events, .. } = page(false);
    ui.set_section(24);
    tap(&ui, "Add profile");
    find(&ui, "Profile name").set_accessible_value("  Sam ");
    tap(&ui, "Save");
    assert_eq!(events.borrow().as_slice(), ["create:  Sam "]);
    assert!(!exists(&ui, "Profile name"), "the editor closes after Save");
    // Rename starts from the current name.
    let renames = buttons(&ui, "Rename");
    assert_eq!(renames.len(), 2);
    renames[1].invoke_accessible_default_action();
    settle();
    assert_eq!(
        find(&ui, "Profile name")
            .accessible_value()
            .unwrap()
            .as_str(),
        "Kid"
    );
    find(&ui, "Profile name").set_accessible_value("Kiddo");
    tap(&ui, "Save");
    assert_eq!(events.borrow().last().unwrap(), "rename:p1_1:Kiddo");
    // Cancel discards.
    tap(&ui, "Add profile");
    tap(&ui, "Cancel");
    assert!(!exists(&ui, "Profile name"));
}

#[test]
fn deleting_a_profile_waits_for_confirmation() {
    let Page { ui, events, .. } = page(false);
    ui.set_section(24);
    let deletes = buttons(&ui, "Delete");
    assert_eq!(deletes.len(), 2);
    deletes[1].invoke_accessible_default_action();
    settle();
    assert!(
        events.borrow().is_empty(),
        "nothing is deleted before confirming"
    );
    // Back dismisses the sheet without deleting.
    ui.invoke_go_back();
    settle();
    assert!(events.borrow().is_empty());
    let deletes = buttons(&ui, "Delete");
    deletes[1].invoke_accessible_default_action();
    settle();
    tap(&ui, "Delete profile");
    assert_eq!(events.borrow().as_slice(), ["delete:p1_1"]);
}

#[test]
fn the_last_profile_cannot_be_deleted() {
    let Page { ui, events, .. } = page(false);
    ui.global::<ProfilesBridge>()
        .set_profiles(ModelRc::new(VecModel::from(vec![row(
            "reader", "Mina", true,
        )])));
    ui.set_section(24);
    settle();
    for delete in buttons(&ui, "Delete") {
        delete.invoke_accessible_default_action();
    }
    settle();
    assert!(!exists(&ui, "Delete profile"));
    assert!(events.borrow().is_empty());
}

#[test]
fn the_startup_picker_cannot_be_dismissed_without_a_choice() {
    let Page { ui, routes, events } = page(true);
    ui.set_section(24);
    settle();
    assert!(exists(&ui, "Use profile: Kid"));
    assert!(
        !exists(&ui, "Rename"),
        "management waits until a profile is chosen"
    );
    ui.invoke_go_back();
    settle();
    press(&ui, Key::Back);
    press(&ui, Key::Escape);
    assert_eq!(ui.get_section(), 24);
    assert!(routes.borrow().is_empty(), "Back never asked to navigate");
    // The host's guard also refuses other destinations.
    ui.invoke_navigate(0);
    assert_eq!(ui.get_section(), 24);
    tap(&ui, "Use profile: Kid");
    assert_eq!(events.borrow().as_slice(), ["select:p1_1"]);
    // Once the host accepts the choice, normal navigation resumes.
    ui.global::<ProfilesBridge>().set_require_selection(false);
    ui.invoke_go_back();
    settle();
    assert_eq!(ui.get_section(), 4);
}

#[test]
fn open_last_profile_toggle_reports_the_new_value() {
    let Page { ui, events, .. } = page(false);
    ui.set_section(24);
    let toggle = |ui: &AppWindow| {
        settle();
        ElementHandle::find_by_accessible_label(ui, "Open the last profile on startup")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Checkbox))
            .expect("toggle")
            .invoke_accessible_default_action();
        settle();
    };
    toggle(&ui);
    assert_eq!(events.borrow().as_slice(), ["remember:false"]);
    ui.global::<ProfilesBridge>().set_remember_last(false);
    toggle(&ui);
    assert_eq!(events.borrow().last().unwrap(), "remember:true");
}

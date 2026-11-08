//! Reader OSD and image context menu motion: both stay mounted so they can
//! animate out, take their durations from Kit's `Anim`, and never hold input
//! while hidden or hiding.
//!
//! The testing backend's mock clock is shared by every test thread in this
//! process, so the scenarios run one after another from a single test: a
//! parallel test's `settle()` would otherwise finish another's animations.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{Anim, AppWindow, PageRow, TileRow};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

const WIDTH: f32 = 390.;
const HEIGHT: f32 = 844.;

fn settle() {
    for _ in 0..24 {
        advance(16);
    }
}

fn reader() -> AppWindow {
    let ui = AppWindow::new().unwrap();
    let image = slint::Image::from_rgb8(slint::SharedPixelBuffer::new(2, 4));
    ui.set_pages(ModelRc::new(VecModel::from(
        (0..2)
            .map(|index| PageRow {
                tiles: ModelRc::new(VecModel::from(vec![TileRow {
                    image: image.clone(),
                    offset: 0.,
                    fraction: 1.,
                    ..Default::default()
                }])),
                ratio: 2.,
                position: index as f32 * 2.,
                loaded: true,
                page_id: format!("page-{index}").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_total_ratio(4.);
    ui.set_reading_mode(0);
    ui.set_section(6);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(WIDTH, HEIGHT));
    settle();
    ui
}

fn find_osd_action(ui: &AppWindow, id: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_element_type_name(ui, "AddonIconButton")
        .find(|element| element.accessible_id().as_deref() == Some(id))
}

fn osd_action(ui: &AppWindow, id: &str) -> ElementHandle {
    find_osd_action(ui, id).unwrap_or_else(|| panic!("missing OSD action {id}"))
}

fn back(ui: &AppWindow) -> ElementHandle {
    osd_action(ui, "reader-osd-action:back:icon")
}

fn bookmark(ui: &AppWindow) -> ElementHandle {
    osd_action(ui, "reader-osd-action:bookmark:icon")
}

fn rail(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, "ReaderOsd::rail")
        .next()
        .expect("OSD rail")
}

/// Back x, bookmark x, rail y.
fn osd_positions(ui: &AppWindow) -> (f32, f32, f32) {
    (
        back(ui).absolute_position().x,
        bookmark(ui).absolute_position().x,
        rail(ui).absolute_position().y,
    )
}

fn assert_close(actual: (f32, f32, f32), expected: (f32, f32, f32)) {
    assert!(
        (actual.0 - expected.0).abs() < 0.5
            && (actual.1 - expected.1).abs() < 0.5
            && (actual.2 - expected.2).abs() < 0.5,
        "{actual:?} != {expected:?}"
    );
}

/// A fully hidden OSD is invisible: its controls are not in the searchable
/// (hit-testable, focusable, accessible) tree at all.
fn assert_hidden(ui: &AppWindow) {
    assert!(find_osd_action(ui, "reader-osd-action:back:icon").is_none());
    assert!(find_osd_action(ui, "reader-osd-action:bookmark:icon").is_none());
    assert!(
        ElementHandle::find_by_element_id(ui, "ReaderOsd::rail")
            .next()
            .is_none()
    );
}

/// Advance mock time in frame-sized steps, as a running window would.
fn advance(ms: u64) {
    for _ in 0..ms / 10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms % 10));
}

fn click(ui: &AppWindow, position: LogicalPosition) {
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

fn reader_osd_slides_to_its_edges_and_lets_go_of_input_while_hiding() {
    let ui = reader();
    let taps = Rc::new(Cell::new(0));
    ui.on_reader_tap({
        let taps = taps.clone();
        move |_, _| taps.set(taps.get() + 1)
    });
    let went_back = Rc::new(Cell::new(false));
    ui.on_reader_action({
        let went_back = went_back.clone();
        move |action| {
            if action == "back" {
                went_back.set(true);
            }
        }
    });

    assert!(!ui.get_reader_osd());
    assert_hidden(&ui);

    ui.set_reader_osd(true);
    settle();
    let shown = osd_positions(&ui);
    // Geometry when shown is unchanged: gutter + safe area from each edge.
    assert!((shown.0 - 12.).abs() < 0.5);
    assert!((shown.1 + 44. + 12. - WIDTH).abs() < 0.5);
    assert!((shown.2 + 190. - HEIGHT).abs() < 0.5);
    assert_eq!(back(&ui).accessible_enabled(), Some(true));
    assert!(
        (back(&ui).computed_opacity() - 1.).abs() < 0.01,
        "controls slide, they do not fade"
    );

    // Mid-hide: on the way out, already without input or accessibility.
    ui.set_reader_osd(false);
    advance(60);
    let partway = osd_positions(&ui);
    assert!(partway.0 < shown.0 && partway.0 > -(44. + 12.));
    assert!(partway.1 > shown.1 && partway.1 < WIDTH);
    assert!(partway.2 > shown.2 && partway.2 < HEIGHT);
    assert_eq!(back(&ui).accessible_enabled(), Some(false));
    assert!((back(&ui).computed_opacity() - 1.).abs() < 0.01);
    // A tap where Back was reaches the page underneath instead.
    let center = back(&ui).absolute_position();
    click(&ui, LogicalPosition::new(center.x + 30., center.y + 22.));
    settle();
    assert!(!went_back.get(), "a hiding OSD must not take taps");
    assert_eq!(taps.get(), 1, "the page receives the tap");
    assert_hidden(&ui);

    // Hide, then show again before the hide finishes: ends fully shown.
    ui.set_reader_osd(true);
    settle();
    ui.set_reader_osd(false);
    advance(40);
    let reversing = osd_positions(&ui);
    ui.set_reader_osd(true);
    advance(1);
    let after = osd_positions(&ui);
    assert!(
        (after.0 - reversing.0).abs() < 8.,
        "reversal continues from where the controls are"
    );
    settle();
    assert_close(osd_positions(&ui), shown);

    // Opening the context menu forces the OSD off.
    ui.invoke_open_reader_context(0, 100., 300.);
    settle();
    assert!(!ui.get_reader_osd());
    assert_hidden(&ui);
    ui.invoke_close_reader_context();
    settle();

    // Animations off (master or player): toggling is instant.
    for master in [false, true] {
        ui.global::<Anim>().set_enabled(master);
        ui.global::<Anim>().set_player(!master);
        ui.set_reader_osd(true);
        advance(1);
        assert_close(osd_positions(&ui), shown);
        ui.set_reader_osd(false);
        advance(1);
        assert_hidden(&ui);
    }
    ui.global::<Anim>().set_enabled(true);
    ui.global::<Anim>().set_player(true);

    // Turning animations off mid-reveal snaps to the end state.
    ui.set_reader_osd(true);
    advance(40);
    ui.global::<Anim>().set_player(false);
    advance(1);
    assert_close(osd_positions(&ui), shown);
    ui.global::<Anim>().set_player(true);

    // While the reader is opening the OSD stays hidden.
    ui.set_reader_opening(true);
    settle();
    assert!(ui.get_reader_osd());
    assert_hidden(&ui);
    ui.set_reader_opening(false);
    settle();
    assert_close(osd_positions(&ui), shown);
}

fn reader_osd_keeps_its_wide_layout_and_slides_the_same_way() {
    let ui = reader();
    ui.window().set_size(LogicalSize::new(1120., 800.));
    ui.set_reader_osd(true);
    settle();
    let shown = osd_positions(&ui);
    assert!((shown.0 - 18.).abs() < 0.5);
    ui.set_reader_osd(false);
    advance(60);
    // Partway out, each group is heading for its own edge.
    let partway = osd_positions(&ui);
    assert!(partway.0 < shown.0 && partway.1 > shown.1 && partway.2 > shown.2);
    settle();
    assert_hidden(&ui);
}

fn menu_panel(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, "ReaderContextMenu::menu-panel")
        .next()
        .expect("context menu panel")
}

fn menu_button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing menu action {label}"))
}

fn context_menu_grows_from_the_press_and_takes_no_input_while_closing() {
    let ui = reader();
    let actions = Rc::new(Cell::new(0));
    ui.on_reader_context_action({
        let actions = actions.clone();
        move |_, _| actions.set(actions.get() + 1)
    });
    let taps = Rc::new(Cell::new(0));
    ui.on_reader_tap({
        let taps = taps.clone();
        move |_, _| taps.set(taps.get() + 1)
    });

    // The closed menu is not in the tree. Querying it also evaluates its
    // state, as each rendered frame does; without a renderer nothing would,
    // and the first read after opening would find the animation already over.
    assert!(
        ElementHandle::find_by_element_id(&ui, "ReaderContextMenu::menu-panel")
            .next()
            .is_none()
    );
    // Read every frame, as the renderer does: the fade advances with time.
    ui.invoke_open_reader_context(0, 20., 200.);
    advance(1);
    let first = menu_panel(&ui).computed_opacity();
    advance(20);
    let opening = menu_panel(&ui).computed_opacity();
    assert!(
        first < opening && opening < 1.,
        "fades in: {first} then {opening}"
    );
    settle();
    let panel = menu_panel(&ui);
    assert!((panel.computed_opacity() - 1.).abs() < 0.01);
    let at = panel.absolute_position();
    // Geometry is unchanged: the panel sits at the anchor (a 340px panel on a
    // 390px screen is clamped to x ≤ 38, so these anchors stay unclamped).
    assert!((at.x - 20.).abs() < 0.5 && (at.y - 200.).abs() < 0.5);

    // Close: input stops at once, the panel fades away, the page gets taps.
    let target = menu_button(&ui, "Copy image to clipboard");
    let target_at = target.absolute_position();
    ui.invoke_close_reader_context();
    advance(1);
    let first = menu_panel(&ui).computed_opacity();
    advance(20);
    let closing = menu_panel(&ui).computed_opacity();
    assert!(
        0. < closing && closing < first,
        "still animating out: {first} then {closing}"
    );
    assert_eq!(target.accessible_enabled(), Some(false));
    target.invoke_accessible_default_action();
    click(
        &ui,
        LogicalPosition::new(target_at.x + 60., target_at.y + 27.),
    );
    settle();
    assert_eq!(actions.get(), 0, "a closing menu cannot be picked");
    assert_eq!(taps.get(), 1, "the reader receives the tap");
    assert!(!ui.get_reader_context_visible());

    // Re-opening during the close ends fully open at the new anchor.
    ui.invoke_open_reader_context(0, 36., 260.);
    settle();
    ui.invoke_close_reader_context();
    advance(40);
    ui.invoke_open_reader_context(0, 28., 320.);
    settle();
    let panel = menu_panel(&ui);
    assert!((panel.computed_opacity() - 1.).abs() < 0.01);
    let at = panel.absolute_position();
    assert!((at.x - 28.).abs() < 0.5 && (at.y - 320.).abs() < 0.5);
    ui.invoke_close_reader_context();
    settle();

    // Near the bottom-right edge the panel is pushed inward and still opens.
    ui.invoke_open_reader_context(0, WIDTH - 1., HEIGHT - 1.);
    settle();
    let panel = menu_panel(&ui);
    let at = panel.absolute_position();
    let size = panel.size();
    assert!(at.x + size.width <= WIDTH + 0.5 && at.y + size.height <= HEIGHT + 0.5);
    assert!((panel.computed_opacity() - 1.).abs() < 0.01);
    ui.invoke_close_reader_context();
    settle();

    // Animations off: open and close are instant.
    ui.global::<Anim>().set_player(false);
    ui.invoke_open_reader_context(0, 20., 200.);
    advance(1);
    assert!((menu_panel(&ui).computed_opacity() - 1.).abs() < 0.01);
    ui.invoke_close_reader_context();
    advance(1);
    assert!(
        ElementHandle::find_by_element_id(&ui, "ReaderContextMenu::menu-panel")
            .next()
            .is_none(),
        "a closed menu is not in the tree"
    );
    ui.global::<Anim>().set_player(true);
}

#[test]
fn reader_osd_and_context_menu_motion() {
    i_slint_backend_testing::init_no_event_loop();
    reader_osd_slides_to_its_edges_and_lets_go_of_input_while_hiding();
    reader_osd_keeps_its_wide_layout_and_slides_the_same_way();
    context_menu_grows_from_the_press_and_takes_no_input_while_closing();
}

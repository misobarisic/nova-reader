//! Installs the separately compiled pages into the window. Factories keep only
//! a weak window and reuse its shared-globals bundle, so a page sees the same
//! `Theme`, `Appearance` and bridge objects as the window.
use crate::{BrowseBridge, LibraryBridge, TimelineBridge};
use slint::ComponentHandle;

pub(crate) fn install(app: &crate::generated::AppWindow) {
    let bridge = app.global::<LibraryBridge>();
    crate::forward_app_callbacks!(app, &bridge;
        on_category_selected => invoke_category_selected(index),
        on_sort_changed => invoke_library_sort_changed(),
        on_search => invoke_search(query),
        on_close_search => invoke_close_library_search(),
        on_toggle_search => invoke_toggle_library_search(),
        on_submitted => invoke_library_search_submitted(),
        on_picked => invoke_open_item(index),
        on_context_requested => invoke_open_entry_menu(key, title, in_library, x, y),
        on_request_artwork => invoke_request_artwork(first, last),
        on_viewport_changed => invoke_library_viewport_changed(key, fraction, y),
        on_checkpoint => invoke_library_checkpoint(),
    );
    let weak = app.as_weak();
    bridge.set_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        match nova_reader_ui_library::LibraryModule::new_embedded_with_shared_globals(
            context,
            app.shared_globals_novauishared(),
        ) {
            Ok(page) => {
                let page_weak = page.as_weak();
                app.global::<LibraryBridge>().on_remember_position(move || {
                    if let Some(page) = page_weak.upgrade() {
                        page.invoke_remember_position();
                    }
                });
                Some(page)
            }
            Err(error) => {
                eprintln!("nova-reader-ui: could not attach the library page: {error}");
                None
            }
        }
    }));
    // Updates and History: the app wires the bridge's callbacks itself.
    let weak = app.as_weak();
    app.global::<TimelineBridge>()
        .set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            nova_reader_ui_timeline::TimelineModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            )
            .inspect_err(|error| {
                eprintln!("nova-reader-ui: could not attach Updates/History: {error}")
            })
            .ok()
        }));
    install_browse(app);
}

/// Browse: the window's callbacks keep their behaviour; the app wires the
/// bridge's own callbacks (sections, filters, search across sources).
fn install_browse(app: &crate::generated::AppWindow) {
    let bridge = app.global::<BrowseBridge>();
    crate::forward_app_callbacks!(app, &bridge;
        on_source_selected => invoke_source_selected(index),
        on_source_latest_selected => invoke_source_latest_selected(index),
        on_source_pin_picked => invoke_source_pin_picked(id),
        on_adult_visibility_changed => invoke_adult_visibility_changed(shown),
        on_extension_action => invoke_extension_action(id, action),
        on_install_addon => invoke_install_addon(),
        on_browse => invoke_browse(kind),
        on_search => invoke_search(query),
        on_load_more => invoke_load_more(),
        on_source_filters_requested => invoke_source_filters_requested(),
        on_source_filter_changed => invoke_source_filter_changed(key, value),
        on_source_filter_selected => invoke_source_filter_selected(key, index),
        on_source_filters_applied => invoke_source_filters_applied(),
        on_source_action => invoke_source_action(action),
        on_picked => invoke_open_item(index),
        on_request_artwork => invoke_request_artwork(first, last),
        on_back => invoke_go_back(),
        on_restore_focus => invoke_focus_window(),
    );
    // Removing an addon keeps the window's confirmation sheet.
    let weak = app.as_weak();
    bridge.on_extension_remove(move |id| {
        if let Some(app) = weak.upgrade() {
            app.invoke_ask_confirm(
                "extension".into(),
                id,
                "remove".into(),
                0,
                crate::backend_text::tr(
                    "Remove this addon? Its sources will no longer be available.",
                )
                .into(),
                crate::backend_text::tr("Remove addon").into(),
            );
        }
    });
    let weak = app.as_weak();
    bridge.on_source_remove(move || {
        if let Some(app) = weak.upgrade() {
            app.invoke_ask_confirm(
                "source".into(),
                "remove".into(),
                "".into(),
                0,
                crate::backend_text::tr(
                    "Remove this addon? Its sources will no longer be available.",
                )
                .into(),
                crate::backend_text::tr("Remove addon").into(),
            );
        }
    });
    let weak = app.as_weak();
    bridge.set_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        nova_reader_ui_browse::BrowseModule::new_embedded_with_shared_globals(
            context,
            app.shared_globals_novauishared(),
        )
        .inspect_err(|error| eprintln!("nova-reader-ui: could not attach Browse: {error}"))
        .ok()
    }));
}

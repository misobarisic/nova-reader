//! Installs the separately compiled pages into the window. Factories keep only
//! a weak window and reuse its shared-globals bundle, so a page sees the same
//! `Theme`, `Appearance` and bridge objects as the window.
use crate::LibraryBridge;
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
}

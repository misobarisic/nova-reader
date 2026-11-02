//! Thumbnail-only maintenance within the shared chapter cache namespace.
use super::*;

impl Controller {
    pub(super) fn thumbnail_cache_maintenance(self: &Arc<Self>, ui: &AppWindow, clear: bool) {
        if self
            .chapter_cache
            .maintenance_busy
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        if clear {
            // Drop UI references and invalidate work before deleting variants.
            self.clear_overview_images(ui);
            if let Some(service) = self.thumbnail_service.get() {
                service.invalidate();
            }
            self.chapter_cache.invalidate_writes();
        }
        let settings = ui.global::<crate::ImageStorageSettings>();
        settings.set_thumbnail_cache_busy(true);
        settings.set_thumbnail_cache_status("".into());
        let cache = self.chapter_cache.clone();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let removed = clear.then(|| cache.clear_thumbnails());
            let (bytes, files) = cache.thumbnail_usage();
            let (chapter_bytes, chapter_files) = cache.usage();
            let failed = cache.clone();
            if weak
                .upgrade_in_event_loop(move |ui| {
                    cache.maintenance_busy.store(false, Ordering::Release);
                    let settings = ui.global::<crate::ImageStorageSettings>();
                    settings.set_thumbnail_cache_busy(false);
                    settings.set_thumbnail_cache_info(text::image_cache_usage(bytes, files).into());
                    settings.set_chapter_cache_info(
                        text::image_cache_usage(chapter_bytes, chapter_files).into(),
                    );
                    if let Some((_, count)) = removed {
                        settings.set_thumbnail_cache_status(
                            format!("{}: {count}", text::tr("Thumbnails removed")).into(),
                        );
                    }
                })
                .is_err()
            {
                failed.maintenance_busy.store(false, Ordering::Release);
            }
        });
    }
}

pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) {
    let owner = Arc::downgrade(app);
    let weak = ui.as_weak();
    ui.global::<crate::ImageStorageSettings>()
        .on_thumbnail_cache_refresh(move || {
            if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                app.thumbnail_cache_maintenance(&ui, false);
            }
        });
    let owner = Arc::downgrade(app);
    let weak = ui.as_weak();
    ui.global::<crate::ImageStorageSettings>()
        .on_thumbnail_cache_clear(move || {
            if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                app.thumbnail_cache_maintenance(&ui, true);
            }
        });
}

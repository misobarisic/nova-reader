//! Settings host for device-local image admission limits.
use crate::{AppWindow, ImageLimitSettings, image_limits::Settings};
use slint::ComponentHandle;

pub(crate) fn refresh(ui: &AppWindow) {
    let settings = Settings::load();
    let global = ui.global::<ImageLimitSettings>();
    global.set_encoded_mib(settings.encoded_mib.to_string().into());
    global.set_width_pixels(settings.width.to_string().into());
    global.set_height_pixels(settings.height.to_string().into());
    global.set_megapixels(settings.megapixels.to_string().into());
    global.set_decoded_mib(settings.decoded_mib.to_string().into());
    global.set_message("".into());
    global.set_error(false);
}

fn message(ui: &AppWindow, result: Result<(), String>) {
    let global = ui.global::<ImageLimitSettings>();
    match result {
        Ok(()) => {
            refresh(ui);
            global.set_message(
                nova_reader_ui::backend_text::tr(
                    "Image limits saved. New image operations use these limits.",
                )
                .into(),
            );
        }
        Err(error) => {
            global.set_error(true);
            global.set_message(
                format!(
                    "{}: {error}",
                    nova_reader_ui::backend_text::tr("Could not save image limits")
                )
                .into(),
            );
        }
    }
}

pub(crate) fn init(ui: &AppWindow, changed: impl Fn() + 'static) {
    let changed = std::rc::Rc::new(changed);
    let save_changed = changed.clone();
    refresh(ui);
    let weak = ui.as_weak();
    ui.global::<ImageLimitSettings>()
        .on_save(move |encoded, width, height, pixels, decoded| {
            let Some(ui) = weak.upgrade() else { return };
            let previous = reader_engine::limits_revision();
            let result = (|| {
                let parse = |text: &str| {
                    text.trim().parse::<u32>().map_err(|_| {
                        nova_reader_ui::backend_text::tr(
                            "Enter a positive whole number in every field",
                        )
                        .to_string()
                    })
                };
                Settings {
                    encoded_mib: parse(&encoded)?,
                    width: parse(&width)?,
                    height: parse(&height)?,
                    megapixels: parse(&pixels)?,
                    decoded_mib: parse(&decoded)?,
                }
                .save()
            })();
            if result.is_ok() && previous != reader_engine::limits_revision() {
                save_changed();
            }
            message(&ui, result);
        });
    let weak = ui.as_weak();
    ui.global::<ImageLimitSettings>().on_reset(move || {
        if let Some(ui) = weak.upgrade() {
            let previous = reader_engine::limits_revision();
            let result = Settings::reset();
            if result.is_ok() && previous != reader_engine::limits_revision() {
                changed();
            }
            message(&ui, result);
        }
    });
}

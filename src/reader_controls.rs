//! Reader input and preference projection; shared by desktop and Android.
use super::*;
use crate::reader_settings::{self, Background, ReaderSettings, Rotation};
use std::cell::Cell;

thread_local! { static PRE_READER_FULLSCREEN: Cell<Option<bool>> = const { Cell::new(None) }; }
const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 8.;

/// Keep the same source point beneath the cursor/fingers, including contained
/// image margins. A moving pinch midpoint also translates that point before
/// the bounds are clamped. All dimensions come from the mounted reader frame.
fn apply_zoom_transform(ui: &AppWindow, zoom: f32, from: (f32, f32), to: (f32, f32)) -> bool {
    if ![zoom, from.0, from.1, to.0, to.1]
        .into_iter()
        .all(f32::is_finite)
    {
        return false;
    }
    let viewport_width = ui.get_reader_viewport_width().max(1.);
    let viewport_height = ui.get_reader_viewport_height().max(1.);
    let from = (
        from.0.clamp(0., 1.) * viewport_width,
        from.1.clamp(0., 1.) * viewport_height,
    );
    let to = (
        to.0.clamp(0., 1.) * viewport_width,
        to.1.clamp(0., 1.) * viewport_height,
    );
    let paged = ui.get_reading_mode() != 0;
    let width = if paged {
        ui.get_reader_paged_width()
    } else {
        ui.get_reader_width()
    }
    .max(1.);
    let height = if paged {
        ui.get_reader_paged_height()
    } else {
        width
    }
    .max(1.);
    let vertical_margin = |height: f32| {
        if !paged || ui.get_reader_split_page() {
            0.
        } else {
            (viewport_height - height).max(0.) / 2.
        }
    };
    let source = (
        (from.0 - ui.get_scroll_x() - (viewport_width - width).max(0.) / 2.) / width,
        (from.1 - ui.get_scroll_y() - vertical_margin(height)) / height,
    );
    ui.set_zoom(zoom.clamp(MIN_ZOOM, MAX_ZOOM));
    let width = if paged {
        ui.get_reader_paged_width()
    } else {
        ui.get_reader_width()
    }
    .max(1.);
    let height = if paged {
        ui.get_reader_paged_height()
    } else {
        width
    }
    .max(1.);
    ui.set_scroll_x(
        -((viewport_width - width).max(0.) / 2. + source.0 * width - to.0)
            .clamp(0., (width - viewport_width).max(0.)),
    );
    let content_height = if paged {
        height
    } else {
        ui.get_total_ratio() * width
    };
    ui.set_scroll_y(
        -(vertical_margin(height) + source.1 * height - to.1)
            .clamp(0., (content_height - viewport_height).max(0.)),
    );
    true
}

pub(super) fn mode_label(mode: ReadingMode, gaps: bool) -> &'static str {
    match mode {
        ReadingMode::Webtoon if gaps => "Long strip with gaps",
        ReadingMode::Webtoon => "Webtoon",
        ReadingMode::LeftToRight => "Paged (left to right)",
        ReadingMode::RightToLeft => "Paged (right to left)",
        ReadingMode::Vertical => "Paged (vertical)",
    }
}

fn display_options(prefs: &ReaderSettings, mode: ReadingMode) -> reader_engine::DisplayOptions {
    let layout = if mode == ReadingMode::Webtoon {
        prefs.webtoon
    } else {
        prefs.paged
    };
    reader_engine::DisplayOptions {
        crop_borders: layout.crop_borders,
        split_wide: layout.split_wide,
        rotate_wide: layout.rotate_wide,
        flip_wide: layout.flip_wide,
        rtl: mode == ReadingMode::RightToLeft,
        reverse_page: !cfg!(target_os = "android") && prefs.rotation == Rotation::ReversePortrait,
        color_filter: prefs.color_filter,
        filter_color: prefs.filter_color,
        filter_intensity: prefs.filter_intensity,
        grayscale: prefs.grayscale,
        invert_colors: prefs.invert_colors,
    }
}

pub(super) fn platform(ui: &AppWindow) {
    let prefs = reader_settings::snapshot();
    let reading = ui.get_section() == 6;
    if !reading {
        crate::reader_settings_ui::apply(ui, &prefs);
    }
    let immersive = reading && prefs.fullscreen && !ui.get_reader_osd();
    #[cfg(target_os = "android")]
    if let Err(error) = crate::android_documents::set_reader_presentation(
        reading,
        immersive,
        ui.get_reader_orientation(),
        prefs.keep_awake,
        prefs.cutout,
    ) {
        eprintln!("Nova Reader: {error}");
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = immersive;
        crate::reader_desktop::keep_awake(reading && prefs.keep_awake);
        PRE_READER_FULLSCREEN.with(|previous| {
            if reading {
                if previous.get().is_none() {
                    previous.set(Some(ui.window().is_fullscreen()));
                }
                ui.window().set_fullscreen(prefs.fullscreen);
            } else if let Some(fullscreen) = previous.take() {
                ui.window().set_fullscreen(fullscreen);
            }
        });
        ui.set_reader_window_fullscreen(ui.window().is_fullscreen());
    }
}

pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) {
    let prefs = reader_settings::snapshot();
    crate::reader_settings_ui::apply(ui, &prefs);
    ui.set_reader_platform_android(cfg!(target_os = "android"));
    ui.global::<crate::ReaderSettings>().on_settings_changed({
        let app = app.clone();
        let weak = ui.as_weak();
        move |key, value| {
            if let Some(ui) = weak.upgrade() {
                let series = if ui.get_section() == 6 {
                    app.view().reader.as_ref().map(|r| r.chapter.series.clone())
                } else {
                    None
                };
                let result = if let Some(series) = series.as_ref().filter(|_| {
                    key.starts_with("paged_")
                        || key.starts_with("webtoon_")
                        || key == "reset_series"
                }) {
                    if key == "reset_series" {
                        reader_settings::reset_series(series).map(|_| ())
                    } else {
                        reader_settings::update_series(series, &key, value).map(|_| ())
                    }
                } else {
                    reader_settings::update(&key, value).map(|_| ())
                };
                match result {
                    Ok(()) => {
                        ui.set_status("".into());
                        app.apply_reader_preferences(&ui);
                    }
                    Err(message) => {
                        ui.set_status(
                            format!("{}: {message}", text::tr("Could not save reader settings"))
                                .into(),
                        );
                    }
                }
                let values = series
                    .as_ref()
                    .map(reader_settings::effective)
                    .unwrap_or_else(reader_settings::snapshot);
                crate::reader_settings_ui::apply(&ui, &values);
            }
        }
    });
    ui.on_reader_rotation_changed({
        let app = app.clone();
        let weak = ui.as_weak();
        move |value| {
            if let Some(ui) = weak.upgrade() {
                let series = app.view().reader.as_ref().map(|r| r.chapter.series.clone());
                if let Some(series) = series {
                    if let Err(error) = reader_settings::update_series(&series, "rotation", value) {
                        ui.set_status(
                            format!("{}: {error}", text::tr("Could not save reader settings"))
                                .into(),
                        );
                    }
                    app.apply_reader_preferences(&ui);
                }
            }
        }
    });
    ui.on_reader_action({
        let app = app.clone();
        let weak = ui.as_weak();
        move |action| {
            if let Some(ui) = weak.upgrade() {
                app.reader_action(&ui, &action);
            }
        }
    });
    ui.on_reader_seek({
        let app = app.clone();
        let weak = ui.as_weak();
        move |progress| {
            if let Some(ui) = weak.upgrade() {
                app.reader_seek(&ui, progress);
            }
        }
    });
    ui.on_reader_tap({
        let app = app.clone();
        let weak = ui.as_weak();
        move |x, y| {
            if let Some(ui) = weak.upgrade() {
                app.reader_tap(&ui, x, y);
            }
        }
    });
    ui.on_reader_double_tap({
        let app = app.clone();
        let weak = ui.as_weak();
        move |x, y| {
            if let Some(ui) = weak.upgrade() {
                app.reader_zoom(&ui, if ui.get_zoom() > 1.01 { 1. } else { 2. }, x, y);
            }
        }
    });
    ui.on_reader_zoom_by({
        let app = app.clone();
        let weak = ui.as_weak();
        move |factor, x, y| {
            if let Some(ui) = weak.upgrade() {
                app.reader_transform(&ui, factor, (x, y), (x, y));
            }
        }
    });
    ui.on_reader_transform({
        let app = app.clone();
        let weak = ui.as_weak();
        move |factor, from_x, from_y, to_x, to_y| {
            if let Some(ui) = weak.upgrade() {
                app.reader_transform(&ui, factor, (from_x, from_y), (to_x, to_y));
            }
        }
    });
    ui.on_immersive_changed({
        let weak = ui.as_weak();
        move |_| {
            if let Some(ui) = weak.upgrade() {
                platform(&ui);
            }
        }
    });
    platform(ui);
}

impl Controller {
    pub(super) fn apply_reader_preferences(&self, ui: &AppWindow) {
        crate::reader_settings_ui::apply(ui, &reader_settings::snapshot());
        if ui.get_section() != 6 {
            platform(ui);
            return;
        }
        let mut view = self.view();
        let Some(reader) = view.reader.as_mut() else {
            drop(view);
            platform(ui);
            return;
        };
        let prefs = reader_settings::effective(&reader.chapter.series);
        let previous_mode = reader.mode;
        reader.mode = reader.mode_choice.resolve(reader.automatic_mode);
        let gaps = if reader.mode == ReadingMode::Webtoon && reader.mode_choice.is_gapped() {
            16.
        } else {
            0.
        };
        let layout = if reader.mode == ReadingMode::Webtoon {
            prefs.webtoon
        } else {
            prefs.paged
        };
        let options = display_options(&prefs, reader.mode);
        let options_changed = options != reader.display_options;
        if options_changed {
            reader.display_options = options;
            // Keep visible buffers until their replacements arrive. Empty
            // quality markers demand a new display recipe without a refetch.
            reader.decoded_widths.clear();
            reader.decoded_responses.clear();
            reader.presentations.clear();
        }
        let geometry_changed = reader.gap_pixels != gaps
            || previous_mode != reader.mode
            || reader.chapter_boundaries != prefs.chapter_transition;
        reader.chapter_boundaries = prefs.chapter_transition;
        reader.gap_pixels = gaps;
        reader.mode_switching = true;
        reader.budget_blocked.clear();
        let (mode, choice, index) = (reader.mode, reader.mode_choice, reader.anchor.page_index);
        let rotation =
            reader_settings::series_snapshot(&reader.chapter.series).rotation_selection();
        let background = match prefs.background {
            Background::Black => slint::Color::from_rgb_u8(0, 0, 0),
            Background::Grey => slint::Color::from_rgb_u8(64, 64, 64),
            Background::White => slint::Color::from_rgb_u8(255, 255, 255),
            Background::Auto => reader
                .presentations
                .get(&index)
                .map(|p| {
                    slint::Color::from_argb_u8(
                        p.background_color[3],
                        p.background_color[0],
                        p.background_color[1],
                        p.background_color[2],
                    )
                })
                .unwrap_or(slint::Color::from_rgb_u8(0, 0, 0)),
        };
        drop(view);
        if options_changed {
            let rows = ui.get_pages();
            for index in 0..rows.row_count() {
                if let Some(mut row) = rows
                    .row_data(index)
                    .filter(|row| row.preview && !row.loaded)
                {
                    super::reader_progressive::clear_preview(&mut row);
                    rows.set_row_data(index, row);
                }
            }
        }
        crate::reader_settings_ui::apply(ui, &prefs);
        ui.set_reader_mode_choice(choice.selection());
        ui.set_reading_mode(mode_index(mode));
        if previous_mode != mode && mode != ReadingMode::Webtoon {
            ui.set_scroll_x(0.);
            ui.set_scroll_y(0.);
        }
        ui.set_reader_mode_label(text::tr(mode_label(mode, gaps > 0.)).into());
        ui.set_reader_rotation_choice(rotation);
        ui.set_reader_orientation(prefs.rotation.into());
        ui.set_reader_gap(gaps);
        ui.set_reader_side_padding(i32::from(layout.side_padding));
        ui.set_reader_double_tap_enabled(layout.double_tap_zoom);
        ui.set_reader_background(background);
        if geometry_changed {
            self.update_reader_geometry(ui);
        }
        self.restore_position(ui);
        if let Some(reader) = self.view().reader.as_mut() {
            reader.mode_switching = false;
        }
        self.publish_reader_controls(ui);
        self.request_images(ui, index);
        platform(ui);
    }

    pub(super) fn publish_reader_controls(&self, ui: &AppWindow) {
        let view = self.view();
        let Some(reader) = &view.reader else {
            return;
        };
        let index = reader
            .anchor
            .page_index
            .min(reader.pages.len().saturating_sub(1));
        let split = reader.presentations.get(&index).is_some_and(|p| p.split);
        let segment = reader.chapter_segment(index);
        let first = segment.map(|s| s.start).unwrap_or(0);
        let end = segment.map(|s| s.end).unwrap_or(reader.pages.len());
        let start_position = reader.page_position(first);
        let total = reader.page_position(end - 1) + reader.ratios[end - 1] - start_position;
        let offset = reader.page_position(index) - start_position
            + reader.ratios[index] * reader.anchor.offset() as f32;
        ui.set_reader_progress(if total > 0. {
            (offset / total).clamp(0., 1.)
        } else {
            0.
        });
        ui.set_reader_local_page(reader.local_index(index) as i32 + 1);
        ui.set_reader_page_count(reader.active_count() as i32);
        ui.set_reader_bookmarked(reader.bookmarked_chapters.contains(reader.owner(index)));
        let boundaries = reader
            .segments
            .windows(2)
            .map(|pair| crate::ReaderBoundaryRow {
                position: reader.page_position(pair[1].start)
                    - reader.gap_after(pair[1].start - 1) / reader.viewport_width.max(1.),
                previous: pair[0].title.clone().into(),
                next: pair[1].title.clone().into(),
            })
            .collect::<Vec<_>>();
        let current = ui.get_reader_boundaries();
        if current.row_count() != boundaries.len()
            || boundaries
                .iter()
                .enumerate()
                .any(|(i, row)| current.row_data(i).as_ref() != Some(row))
        {
            ui.set_reader_boundaries(ModelRc::new(VecModel::from(boundaries)));
        }
        ui.set_reader_split_page(split && reader.mode != ReadingMode::Webtoon);
        ui.set_reader_split_half(i32::from(reader.anchor.offset() >= 0.5));
        if ui.global::<crate::ReaderSettings>().get_background() == 3
            && let Some(p) = reader.presentations.get(&index)
        {
            let c = p.background_color;
            ui.set_reader_background(slint::Color::from_argb_u8(c[3], c[0], c[1], c[2]));
        }
        let title = reader
            .chapter_segment(index)
            .map(|s| s.title.as_str())
            .or_else(|| {
                view.chapters
                    .iter()
                    .find(|c| c.id == reader.chapter)
                    .map(|c| c.title.as_str())
            })
            .unwrap_or("Chapter");
        if ui.get_reader_chapter_title().as_str() != title {
            if !ui.get_reader_chapter_title().is_empty() {
                ui.invoke_show_reader_transition(ui.get_reader_chapter_title(), title.into());
            }
            ui.set_reader_chapter_title(title.into());
        }
    }

    pub(super) fn update_reader_geometry(&self, ui: &AppWindow) {
        let view = self.view();
        let Some(reader) = &view.reader else {
            return;
        };
        let model = ui.get_pages();
        for i in 0..reader.pages.len() {
            if let Some(mut row) = model.row_data(i) {
                row.ratio = reader.ratios[i];
                row.position = reader.page_position(i);
                model.set_row_data(i, row);
            }
        }
        ui.set_total_ratio(reader.total_extent());
    }

    pub(super) fn reader_seek(self: &Arc<Self>, ui: &AppWindow, progress: f32) {
        if !progress.is_finite() {
            return;
        }
        let mut view = self.view();
        let Some(reader) = view.reader.as_mut() else {
            return;
        };
        let start = reader
            .chapter_segment(reader.anchor.page_index)
            .map(|s| s.start)
            .unwrap_or(0);
        let end = reader
            .chapter_segment(reader.anchor.page_index)
            .map(|s| s.end)
            .unwrap_or(reader.pages.len());
        let extent =
            reader.page_position(end - 1) + reader.ratios[end - 1] - reader.page_position(start);
        let mut remaining = extent * progress.clamp(0., 1.);
        let mut index = start;
        while index + 1 < end
            && remaining
                >= reader.ratios[index] + reader.gap_after(index) / reader.viewport_width.max(1.)
        {
            remaining -=
                reader.ratios[index] + reader.gap_after(index) / reader.viewport_width.max(1.);
            index += 1;
        }
        let fraction = (remaining / reader.ratios[index]).clamp(0., 1.);
        reader.anchor = ResumeAnchor::new(reader.page_id(index), index, f64::from(fraction))
            .unwrap_or_default();
        view.pending = Some(reader.progress_record());
        drop(view);
        ui.set_current_page(index as i32);
        if ui.get_reading_mode() != 0 {
            ui.set_scroll_x(0.);
            ui.set_scroll_y(0.);
        }
        self.restore_position(ui);
        self.publish_reader_controls(ui);
        self.request_images(ui, index);
        self.maybe_preload_next(ui);
    }

    pub(super) fn reader_tap(self: &Arc<Self>, ui: &AppWindow, x: f32, y: f32) {
        let view = self.view();
        let Some(reader) = &view.reader else {
            return;
        };
        if ui.get_zoom() > 1.01 {
            drop(view);
            ui.set_reader_osd(!ui.get_reader_osd());
            return;
        }
        let prefs = reader_settings::effective(&reader.chapter.series);
        let layout = if reader.mode == ReadingMode::Webtoon {
            prefs.webtoon
        } else {
            prefs.paged
        };
        let action = crate::reader_navigation::tap_action(
            x,
            y,
            layout.tap_zones,
            layout.invert_taps,
            reader.mode,
        );
        drop(view);
        match action {
            crate::reader_navigation::TapAction::ToggleControls => {
                ui.set_reader_osd(!ui.get_reader_osd())
            }
            crate::reader_navigation::TapAction::Previous => self.reader_action(ui, "backward"),
            crate::reader_navigation::TapAction::Next => self.reader_action(ui, "forward"),
        }
    }

    pub(super) fn reader_zoom(&self, ui: &AppWindow, zoom: f32, x: f32, y: f32) {
        if apply_zoom_transform(ui, zoom, (x, y), (x, y)) && ui.get_reading_mode() != 0 {
            self.reflow(ui, ui.get_reader_width(), ui.get_reader_viewport_height());
        }
    }

    fn reader_transform(&self, ui: &AppWindow, factor: f32, from: (f32, f32), to: (f32, f32)) {
        if !factor.is_finite() || factor <= 0. {
            return;
        }
        if apply_zoom_transform(ui, ui.get_zoom() * factor, from, to) && ui.get_reading_mode() != 0
        {
            self.reflow(ui, ui.get_reader_width(), ui.get_reader_viewport_height());
        }
    }

    pub(super) fn reader_action(self: &Arc<Self>, ui: &AppWindow, action: &str) {
        match action {
            "back" | "chapters" => self.navigate(ui, 5),
            "previous-chapter" => self.step_chapter(ui, -1),
            "next-chapter" => self.step_chapter(ui, 1),
            "previous-page" => self.step_page(ui, -1),
            "next-page" => self.step_page(ui, 1),
            "fit-width" => self.reader_zoom(ui, 1., 0.5, 0.5),
            "zoom" => self.reader_zoom(
                ui,
                if ui.get_zoom() >= MAX_ZOOM - 0.1 {
                    1.
                } else {
                    ui.get_zoom() + 0.25
                },
                0.5,
                0.5,
            ),
            "zoom-in" => self.reader_zoom(ui, ui.get_zoom() + 0.25, 0.5, 0.5),
            "zoom-out" => self.reader_zoom(ui, ui.get_zoom() - 0.25, 0.5, 0.5),
            "fullscreen" => {
                if reader_settings::update(
                    "fullscreen",
                    i32::from(!reader_settings::snapshot().fullscreen),
                )
                .is_ok()
                {
                    self.apply_reader_preferences(ui);
                }
            }
            "bookmark" => {
                let point = self.view().reader.as_ref().map(|r| {
                    let p = r.progress_record();
                    (p.chapter, p.anchor)
                });
                if let Some((chapter, anchor)) = point {
                    let mut bookmarked = false;
                    match self.services.domain.update(|s| {
                        if s.bookmarks.iter().any(|b| b.chapter == chapter) {
                            s.bookmarks.retain(|b| b.chapter != chapter);
                        } else {
                            s.bookmarks.push(Bookmark {
                                chapter: chapter.clone(),
                                anchor: anchor.clone(),
                                note: String::new(),
                            });
                            bookmarked = true;
                        }
                        Ok(())
                    }) {
                        Ok(()) => {
                            if let Some(reader) = self.view().reader.as_mut() {
                                if bookmarked {
                                    reader.bookmarked_chapters.insert(chapter);
                                } else {
                                    reader.bookmarked_chapters.remove(&chapter);
                                }
                            }
                            ui.set_reader_bookmarked(bookmarked);
                        }
                        Err(e) => ui.set_status(error(e).into()),
                    }
                }
            }
            "screencap" => {
                ui.invoke_reader_context_action(ui.get_current_page(), "save-screencap".into());
            }
            "forward" | "backward" => {
                if ui.get_reading_mode() == 0 {
                    let amount = ui.get_reader_viewport_height()
                        * 0.8
                        * if action == "forward" { 1. } else { -1. };
                    let limit = (ui.get_total_ratio() * ui.get_reader_width()
                        - ui.get_reader_viewport_height())
                    .max(0.);
                    ui.set_scroll_y(-(-ui.get_scroll_y() + amount).clamp(0., limit));
                } else {
                    self.step_page(ui, if action == "forward" { 1 } else { -1 });
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
    use i_slint_core::{input::TouchPhase, lengths::LogicalPoint, platform::InternalEvent};
    use slint::platform::{Key, PointerEventButton, WindowEvent};
    use slint::{LogicalPosition, LogicalSize, VecModel};
    use std::{cell::RefCell, rc::Rc, time::Duration};

    fn advance(ms: u64) {
        for _ in 0..ms / 10 {
            mock_elapsed_time(Duration::from_millis(10));
        }
    }
    fn touch(ui: &AppWindow, id: i32, x: f32, y: f32, phase: TouchPhase) {
        ui.window()
            .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
                id,
                position: LogicalPoint::new(x, y),
                phase,
            }));
    }
    fn tap(ui: &AppWindow, touchscreen: bool, x: f32, y: f32) {
        if touchscreen {
            touch(ui, 1, x, y, TouchPhase::Started);
            advance(50);
            touch(ui, 1, x, y, TouchPhase::Ended);
        } else {
            ui.window().dispatch_event(WindowEvent::PointerPressed {
                position: LogicalPosition::new(x, y),
                button: PointerEventButton::Left,
            });
            ui.window().dispatch_event(WindowEvent::PointerReleased {
                position: LogicalPosition::new(x, y),
                button: PointerEventButton::Left,
            });
        }
        advance(300);
    }
    fn wheel(ui: &AppWindow, x: f32, y: f32, delta: f32, control: bool) {
        if control {
            ui.window().dispatch_event(WindowEvent::KeyPressed {
                text: Key::Control.into(),
            });
        }
        ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(x, y),
            delta_x: 0.,
            delta_y: delta,
        });
        if control {
            ui.window().dispatch_event(WindowEvent::KeyReleased {
                text: Key::Control.into(),
            });
        }
        advance(50);
    }
    fn source_at(ui: &AppWindow, x: f32, y: f32) -> (f32, f32) {
        let image = ElementHandle::find_by_accessible_label(ui, "Page 2 image")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        let origin = image.absolute_position();
        let size = image.size();
        ((x - origin.x) / size.width, (y - origin.y) / size.height)
    }

    #[test]
    fn native_zoom_and_zoomed_taps_use_production_controller() {
        const CHILD: &str = "READER_ZOOM_CONTROL_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_controls::tests::native_zoom_and_zoomed_taps_use_production_controller", "--nocapture"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        i_slint_backend_testing::init_no_event_loop();
        let directory = tempfile::tempdir().unwrap();
        nova_storage::init_at(directory.path());
        reader_settings::init().unwrap();
        reader_settings::update("fullscreen", 0).unwrap();
        reader_settings::update("keep_awake", 0).unwrap();
        let ui = AppWindow::new().unwrap();
        let services = Services::open(&directory.path().join("reader")).unwrap();
        let mut reader = super::super::tests::tall_reader();
        reader.pages = (0..3)
            .map(|index| PageInput::Local {
                path: PathBuf::new(),
                id: format!("page-{index}"),
            })
            .collect();
        reader.ratios = vec![1.5; 3];
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
        reader.retained = (0..3).collect();
        reader.requested = (0..3).collect();
        let series = reader.chapter.series.clone();
        let (images, _receiver) = mpsc::channel();
        let app = Arc::new(Controller {
            progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
                services.domain.clone(),
            )
            .unwrap(),
            artwork: crate::artwork::ArtworkLoader::new(
                services.clone(),
                ui.as_weak(),
                directory.path().join("posters"),
            ),
            services,
            chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
                directory.path().join("cache"),
            )),
            image_worker_live: AtomicBool::new(true),
            view: Mutex::new(View {
                section: 6,
                reader: Some(reader),
                ..View::default()
            }),
            generation: AtomicU64::new(1),
            cancel: Mutex::new(CancelToken::default()),
            images,
            download_serial: Mutex::new(()),
            active_image: Mutex::new(None),
            metadata_refresh: Arc::default(),
        });
        wire(&app, &ui);
        let steps = Rc::new(RefCell::new(Vec::new()));
        ui.on_page_step({
            let steps = steps.clone();
            move |step| steps.borrow_mut().push(step)
        });
        ui.set_pages(ModelRc::new(VecModel::from(
            (0..3)
                .map(|index| PageRow {
                    page_id: format!("page-{index}").into(),
                    position: index as f32 * 1.5,
                    ratio: 1.5,
                    loaded: true,
                    tiles: ModelRc::new(VecModel::from(vec![TileRow {
                        image: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(200, 300)),
                        fraction: 1.,
                        ..Default::default()
                    }])),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_total_ratio(4.5);
        ui.set_section(6);
        ui.set_current_page(1);
        ui.show().unwrap();
        advance(100);

        for (width, height) in [(390., 844.), (1280., 800.)] {
            ui.window().set_size(LogicalSize::new(width, height));
            advance(100);
            for (mode, native_mode) in [
                (0, ReadingMode::Webtoon),
                (1, ReadingMode::LeftToRight),
                (2, ReadingMode::RightToLeft),
                (3, ReadingMode::Vertical),
            ] {
                app.view().reader.as_mut().unwrap().mode = native_mode;
                ui.set_reading_mode(mode);
                ui.set_current_page(1);
                ui.set_reader_platform_android(false);
                if mode == 0 && width > 1100. {
                    app.reader_zoom(&ui, 1., 0.5, 0.5);
                    ui.set_scroll_x(0.);
                    ui.set_scroll_y(-1.5 * ui.get_reader_width());
                    advance(100);
                    let point = (width * 0.55, height * 0.45);
                    let before = source_at(&ui, point.0, point.1);
                    wheel(&ui, point.0, point.1, 120., true);
                    let after = source_at(&ui, point.0, point.1);
                    assert!(
                        (before.0 - after.0).abs() < 0.003 && (before.1 - after.1).abs() < 0.003,
                        "Webtoon cursor anchor accounts for the centered 1100px fit column"
                    );
                }
                // A contained image has no pan range on a still-fitted axis.
                // Begin this strict off-center check with room on both axes.
                let initial_zoom = if mode == 0 { 2. } else { 4. };
                app.reader_zoom(&ui, initial_zoom, 0.5, 0.5);
                ui.set_scroll_x(-(ui.get_reader_paged_width() - width).max(0.) / 2.);
                ui.set_scroll_y(if mode == 0 {
                    -1.5 * ui.get_reader_width()
                } else {
                    -(ui.get_reader_paged_height() - height).max(0.) / 2.
                });
                advance(100);
                let (x, y) = (width * 0.55, height * 0.45);
                let before = source_at(&ui, x, y);
                let page_steps = steps.borrow().len();
                wheel(&ui, x, y, 120., true);
                assert!(
                    (ui.get_zoom() - initial_zoom * 1.2).abs() < 0.001,
                    "Ctrl+wheel zooms mode {mode}"
                );
                let after = source_at(&ui, x, y);
                assert!(
                    (before.0 - after.0).abs() < 0.003 && (before.1 - after.1).abs() < 0.003,
                    "cursor source anchor at mode {mode}: {before:?} -> {after:?}"
                );
                assert_eq!(steps.borrow().len(), page_steps, "Ctrl wheel never pages");
                wheel(&ui, x, y, -120., true);
                assert!((ui.get_zoom() - initial_zoom).abs() < 0.001);
                for _ in 0..12 {
                    wheel(&ui, x, y, 120., true);
                }
                assert_eq!(ui.get_zoom(), MAX_ZOOM);
                assert!(
                    ui.get_reader_width() > 1100.,
                    "desktop magnification exceeds fit-width cap"
                );
                for _ in 0..24 {
                    wheel(&ui, x, y, -120., true);
                }
                assert_eq!(ui.get_zoom(), MIN_ZOOM);

                // Every zone/inversion is bypassed while magnified. These are
                // native presses reaching the real reader_tap handler above.
                for zone in [0, 1, 2, 3, 4] {
                    for inversion in [0, 1, 2, 3] {
                        let prefix = if mode == 0 { "webtoon" } else { "paged" };
                        reader_settings::update_series(
                            &series,
                            &format!("{prefix}_tap_zones"),
                            zone,
                        )
                        .unwrap();
                        reader_settings::update_series(
                            &series,
                            &format!("{prefix}_invert_taps"),
                            inversion,
                        )
                        .unwrap();
                        for zoom in [2., 4.] {
                            app.reader_zoom(&ui, zoom, 0.5, 0.5);
                            ui.set_reader_double_tap_enabled(false);
                            for touchscreen in [false, true] {
                                for fraction in [0.1, 0.5, 0.9] {
                                    ui.set_reader_osd(false);
                                    tap(&ui, touchscreen, width * fraction, height * 0.4);
                                    assert!(
                                        ui.get_reader_osd(),
                                        "zoomed tap toggles controls: mode {mode}, zone {zone}, inversion {inversion}"
                                    );
                                    assert_eq!(
                                        app.view().reader.as_ref().unwrap().anchor.page_index,
                                        1
                                    );
                                    assert_eq!(ui.get_current_page(), 1);
                                }
                            }
                        }
                    }
                }
                ui.set_reader_osd(false);
                ui.set_reader_double_tap_enabled(true);
                app.reader_zoom(&ui, 1., 0.5, 0.5);
                ui.set_scroll_x(0.);
                ui.set_scroll_y(if mode == 0 {
                    -ui.get_reader_width() * 1.5
                } else {
                    0.
                });
                ui.set_reader_platform_android(true);
                advance(100);
                let (cx, cy) = (width / 2., height * 0.4);
                for expected in [2., 1.] {
                    ui.set_reader_osd(false);
                    touch(&ui, 1, cx, cy, TouchPhase::Started);
                    advance(30);
                    touch(&ui, 1, cx, cy, TouchPhase::Ended);
                    advance(70);
                    touch(&ui, 1, cx + 2., cy, TouchPhase::Started);
                    advance(30);
                    touch(&ui, 1, cx + 2., cy, TouchPhase::Ended);
                    advance(300);
                    assert!(
                        (ui.get_zoom() - expected).abs() < 0.001,
                        "double tap remains fit/2x in mode {mode}"
                    );
                    assert!(!ui.get_reader_osd());
                }
                let page_steps = steps.borrow().len();
                touch(&ui, 11, cx - 20., cy, TouchPhase::Started);
                advance(50);
                touch(&ui, 22, cx + 20., cy, TouchPhase::Started);
                advance(800);
                assert!(
                    !ui.get_reader_osd() && !ui.get_reader_context_visible(),
                    "second finger cancels taps/hold before pinch threshold"
                );
                touch(&ui, 11, cx - 30., cy, TouchPhase::Moved);
                advance(20);
                for (right, expected) in [
                    (45., 1.5),
                    (70., 2.),
                    (20., 1.),
                    (45., 1.5),
                    (170., 4.),
                    (-30., 0.5),
                    (20., 1.),
                    (70., 2.),
                    (20., 1.),
                ] {
                    touch(&ui, 22, cx + right, cy, TouchPhase::Moved);
                    advance(20);
                    assert!(
                        (ui.get_zoom() - expected).abs() < 0.005,
                        "raw finger distance gives exact scale {expected} in mode {mode}, got {}",
                        ui.get_zoom()
                    );
                }
                touch(&ui, 11, cx - 30., cy, TouchPhase::Ended);
                advance(800);
                touch(&ui, 22, cx + 20., cy, TouchPhase::Ended);
                advance(400);
                assert_eq!(steps.borrow().len(), page_steps);
                assert!(
                    !ui.get_reader_osd() && !ui.get_reader_context_visible(),
                    "remaining finger is consumed through release"
                );
                assert_eq!(ui.get_current_page(), 1);
                ui.set_reader_double_tap_enabled(false);
                for lifted in [11, 22] {
                    for phase in [TouchPhase::Ended, TouchPhase::Cancelled] {
                        touch(&ui, 11, cx - 20., cy, TouchPhase::Started);
                        advance(50);
                        touch(&ui, 22, cx + 20., cy, TouchPhase::Started);
                        advance(800);
                        touch(&ui, lifted, cx, cy, phase);
                        advance(800);
                        let remaining = if lifted == 11 { 22 } else { 11 };
                        touch(&ui, remaining, cx + 5., cy, TouchPhase::Moved);
                        touch(&ui, remaining, cx + 5., cy, TouchPhase::Ended);
                        advance(400);
                        assert!(
                            !ui.get_reader_osd() && !ui.get_reader_context_visible(),
                            "unrecognized two-finger input never becomes a tap/hold"
                        );
                        assert_eq!(steps.borrow().len(), page_steps);
                        tap(&ui, true, cx, cy);
                        assert!(
                            ui.get_reader_osd(),
                            "fresh independent contact works after both fingers release"
                        );
                        ui.set_reader_osd(false);
                    }
                }
            }
        }
        app.invalidate();
    }
}

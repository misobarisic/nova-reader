//! Reader input and preference projection; shared by desktop and Android.
use super::*;
use crate::reader_settings::{self, Background, ReaderSettings, Rotation};
use std::cell::Cell;

thread_local! { static PRE_READER_FULLSCREEN: Cell<Option<bool>> = const { Cell::new(None) }; }

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
        if options != reader.display_options {
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
        if !zoom.is_finite() {
            return;
        }
        if ui.get_reading_mode() != 0 {
            let viewport_width = ui.get_reader_viewport_width().max(1.);
            let viewport_height = ui.get_reader_viewport_height().max(1.);
            let local_x = x.clamp(0., 1.) * viewport_width;
            let local_y = y.clamp(0., 1.) * viewport_height;
            let old_width = ui.get_reader_paged_width().max(1.);
            let old_height = ui.get_reader_paged_height().max(1.);
            let source_x =
                (local_x - ui.get_scroll_x() - (viewport_width - old_width).max(0.) / 2.)
                    / old_width;
            let vertical_margin = |height: f32| {
                if ui.get_reader_split_page() {
                    0.
                } else {
                    (viewport_height - height).max(0.) / 2.
                }
            };
            let source_y = (local_y - ui.get_scroll_y() - vertical_margin(old_height)) / old_height;
            ui.set_zoom(zoom.clamp(0.5, 3.));
            let width = ui.get_reader_paged_width().max(1.);
            let height = ui.get_reader_paged_height().max(1.);
            ui.set_scroll_x(
                -((viewport_width - width).max(0.) / 2. + source_x * width - local_x)
                    .clamp(0., (width - viewport_width).max(0.)),
            );
            ui.set_scroll_y(
                -(vertical_margin(height) + source_y * height - local_y)
                    .clamp(0., (height - viewport_height).max(0.)),
            );
            self.reflow(ui, ui.get_reader_width(), viewport_height);
            return;
        }
        let old_width = ui.get_reader_width().max(1.);
        let local_y = y.clamp(0., 1.) * ui.get_reader_viewport_height();
        let old_y = (-ui.get_scroll_y() + local_y) / old_width;
        let old_x =
            (-ui.get_scroll_x() + x.clamp(0., 1.) * ui.get_reader_canvas_width()) / old_width;
        ui.set_zoom(zoom.clamp(0.5, 3.));
        let width = ui.get_reader_width();
        ui.set_scroll_x(
            -(old_x * width - x.clamp(0., 1.) * ui.get_reader_canvas_width())
                .clamp(0., (width - ui.get_reader_canvas_width()).max(0.)),
        );
        if ui.get_reading_mode() == 0 {
            ui.set_scroll_y(-(old_y * width - local_y).clamp(
                0.,
                (ui.get_total_ratio() * width - ui.get_reader_viewport_height()).max(0.),
            ));
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
                if ui.get_zoom() >= 2.9 {
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

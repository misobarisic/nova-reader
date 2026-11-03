//! Explicit chapter buttons share the retained reader, but have an edge intent
//! rather than the saved resume intent used by Detail and History.
use super::*;

#[derive(Clone, Debug)]
pub(super) struct ChapterNavigation {
    chapter: ChapterId,
    backwards: bool,
}

#[derive(Default)]
struct EntryLayout {
    /// Tail-first while discovering a previous chapter, head-first otherwise.
    spans: Vec<(usize, (f64, f64))>,
    anchor: Option<(usize, f64)>,
    /// Webtoon canvas coordinate, including real gaps between short images.
    position: Option<f64>,
}

fn valid_dimensions(dimensions: reader_engine::PageDimensions) -> bool {
    dimensions.width > 0 && dimensions.height > 0
}

impl Reader {
    pub(super) fn navigation_target_chapter(&self) -> Option<&ChapterId> {
        self.chapter_navigation
            .as_ref()
            .map(|intent| &intent.chapter)
    }

    pub(super) fn navigation_metadata_target(&self) -> Option<(&ChapterId, bool)> {
        let intent = self.chapter_navigation.as_ref()?;
        (!self.segments.iter().any(|s| s.chapter == intent.chapter))
            .then_some((&intent.chapter, intent.backwards))
    }

    fn navigation_layout(
        &self,
        probed: Option<(usize, reader_engine::PageDimensions)>,
    ) -> Option<EntryLayout> {
        let intent = self.chapter_navigation.as_ref()?;
        let segment = self.segments.iter().find(|s| s.chapter == intent.chapter)?;
        if segment.start >= segment.end || segment.end > self.pages.len() {
            return None;
        }
        let dimensions = |index| {
            probed
                .filter(|(i, _)| *i == index)
                .map(|(_, d)| d)
                .or_else(|| self.presentations.get(&index).map(|p| p.effective))
                .filter(|d| valid_dimensions(*d))
        };
        let edge = if intent.backwards {
            segment.end - 1
        } else {
            segment.start
        };
        if self.mode != ReadingMode::Webtoon {
            // A probe can plan the pixels before its PagePresentation reaches
            // the UI thread. Splitting is an effective-geometry operation;
            // commit waits for the actual presentation, never a width guess.
            let split = self.presentations.get(&edge).map(|p| p.split);
            let span = match (intent.backwards, split) {
                (true, Some(true)) => (0.5, 1.),
                (true, None) => (1., 1.),
                _ => (0., 1.),
            };
            return Some(EntryLayout {
                spans: vec![(edge, span)],
                anchor: split.map(|split| (edge, if intent.backwards && split { 0.5 } else { 0. })),
                position: None,
            });
        }
        if self.viewport_width <= 0. || self.viewport_height <= 0. {
            return Some(EntryLayout {
                spans: vec![(edge, if intent.backwards { (1., 1.) } else { (0., 0.) })],
                ..Default::default()
            });
        }
        let mut remaining = f64::from(self.viewport_height / self.viewport_width);
        let mut layout = EntryLayout::default();
        if intent.backwards {
            for index in (segment.start..segment.end).rev() {
                let Some(d) = dimensions(index) else {
                    layout.spans.push((index, (1., 1.)));
                    return Some(layout);
                };
                let ratio = f64::from(d.height) / f64::from(d.width);
                let start = (1. - remaining / ratio).clamp(0., 1.);
                layout.spans.push((index, (start, 1.)));
                if remaining <= ratio || index == segment.start {
                    layout.anchor = Some((index, start));
                    layout.position = Some(f64::from(self.page_position(index)) + ratio * start);
                    return Some(layout);
                }
                remaining -= ratio;
                let gap = f64::from(self.gap_after(index - 1) / self.viewport_width);
                if remaining <= gap {
                    // ResumeAnchor cannot encode a point inside a gap. Keep
                    // the nearest real image anchor and restore the exact
                    // canvas coordinate separately, under callback suppression.
                    layout.anchor = Some((index, 0.));
                    layout.position = Some(f64::from(self.page_position(index)) - remaining);
                    return Some(layout);
                }
                remaining -= gap;
            }
        } else {
            for index in segment.start..segment.end {
                let Some(d) = dimensions(index) else {
                    layout.spans.push((index, (0., 0.)));
                    return Some(layout);
                };
                let ratio = f64::from(d.height) / f64::from(d.width);
                layout
                    .spans
                    .push((index, (0., (remaining / ratio).clamp(0., 1.))));
                if remaining <= ratio || index + 1 == segment.end {
                    layout.anchor = Some((segment.start, 0.));
                    layout.position = Some(f64::from(self.page_position(segment.start)));
                    return Some(layout);
                }
                remaining -= ratio;
                remaining -= f64::from(self.gap_after(index) / self.viewport_width);
                if remaining <= 0. {
                    layout.anchor = Some((segment.start, 0.));
                    layout.position = Some(f64::from(self.page_position(segment.start)));
                    return Some(layout);
                }
            }
        }
        Some(layout)
    }

    pub(super) fn navigation_pixel_targets(&self) -> Vec<usize> {
        self.navigation_layout(None)
            .map(|layout| layout.spans.into_iter().map(|(index, _)| index).collect())
            .unwrap_or_default()
    }

    pub(super) fn navigation_pixel_target(&self) -> Option<usize> {
        let layout = self.navigation_layout(None)?;
        layout.spans.iter().find_map(|(index, span)| {
            (!self.navigation_pixels_ready(*index, *span)).then_some(*index)
        })
    }

    fn navigation_pixels_ready(&self, index: usize, span: (f64, f64)) -> bool {
        self.presentations.contains_key(&index)
            // Recipe changes retain the old buffers but clear this marker.
            // Early presentation probing alone cannot make those pixels valid.
            && self.decoded_widths.contains_key(&index)
            && self.retained.contains(&index)
            && self
                .decoded_windows
                .get(&index)
                .is_some_and(|plan| plan.covers(span.0, span.1))
    }

    pub(super) fn navigation_span(
        &self,
        index: usize,
        dimensions: reader_engine::PageDimensions,
    ) -> Option<(f64, f64)> {
        self.navigation_layout(Some((index, dimensions)))?
            .spans
            .into_iter()
            .find_map(|(i, span)| (i == index).then_some(span))
    }
}

impl Controller {
    fn settle_chapter_navigation_zoom(self: &Arc<Self>, ui: &AppWindow) {
        if !ui.get_reader_zoom_animating() {
            return;
        }
        self.settle_reader_zoom(ui, PositionOrigin::ZoomSettlement);
    }

    pub(super) fn settle_reader_zoom(self: &Arc<Self>, ui: &AppWindow, origin: PositionOrigin) {
        let switching = self.view().reader.as_mut().map(|reader| {
            let old = reader.mode_switching;
            reader.mode_switching = true;
            old
        });
        // Cancellation reflows synchronously and can emit position callbacks.
        // Keep those programmatic writes from cancelling the chapter intent or
        // committing a nested entry before the final zoom anchor is applied.
        ui.invoke_cancel_reader_zoom();
        if let Some(switching) = switching
            && let Some(reader) = self.view().reader.as_mut()
        {
            reader.mode_switching = switching;
        }
        self.position_internal(
            ui,
            -ui.get_scroll_y(),
            ui.get_reader_width(),
            ui.get_reader_viewport_height(),
            origin,
        );
    }

    fn reset_chapter_entry_zoom(self: &Arc<Self>, ui: &AppWindow) {
        if (ui.get_zoom() - 1.).abs() <= f32::EPSILON {
            return;
        }
        let switching = self.view().reader.as_mut().map(|reader| {
            let old = reader.mode_switching;
            reader.mode_switching = true;
            old
        });
        // A fresh chapter starts at fit scale. Restore the admitted old source
        // point at that scale before any destination viewport is planned.
        ui.set_zoom(1.);
        self.reflow(ui, ui.get_reader_width(), ui.get_reader_viewport_height());
        if let Some(switching) = switching
            && let Some(reader) = self.view().reader.as_mut()
        {
            reader.mode_switching = switching;
        }
        self.position_internal(
            ui,
            -ui.get_scroll_y(),
            ui.get_reader_width(),
            ui.get_reader_viewport_height(),
            PositionOrigin::ZoomSettlement,
        );
    }

    pub(super) fn navigate_chapter(self: &Arc<Self>, ui: &AppWindow, backwards: bool) {
        let (chapter, collection_group) = {
            let mut view = self.view();
            if view.section != 6 {
                return;
            }
            let Some(reader) = view.reader.as_ref() else {
                return;
            };
            // Resolve direction against the chapter active at the button press,
            // before zoom settlement can move the live old viewport's owner.
            let next = view
                .chapters
                .iter()
                .position(|c| c.id == reader.chapter)
                .and_then(|current| {
                    if backwards {
                        current.checked_sub(1)
                    } else {
                        current.checked_add(1)
                    }
                })
                .filter(|index| *index < view.chapters.len());
            let chapter = next.map(|index| view.chapters[index].id.clone());
            let collection_group = reader
                .collection
                .then(|| next.and_then(|index| i32::try_from(index).ok()));
            self.continuation_work.cancel();
            let reader = view.reader.as_mut().unwrap();
            reader.preloading = None;
            reader.preload_nonce = reader.preload_nonce.wrapping_add(1);
            reader.chapter_navigation = None;
            reader.reverse_demand = None;
            reader.window_demand = None;
            (chapter, collection_group)
        };
        self.checkpoint_progress();
        self.settle_chapter_navigation_zoom(ui);
        if let Some(group) = collection_group {
            // Collection chapters use their existing group overview route.
            if let Some(group) = group
                && self.gallery_return(ui)
            {
                self.gallery_group(ui, group);
            }
            return;
        }
        let Some(chapter) = chapter else {
            return;
        };
        self.reset_chapter_entry_zoom(ui);
        {
            let mut view = self.view();
            if view.section != 6 {
                return;
            }
            let Some(reader) = view.reader.as_mut() else {
                return;
            };
            reader.chapter_navigation = Some(ChapterNavigation {
                chapter: chapter.clone(),
                backwards,
            });
            reader.continuity_failures.remove(&chapter);
            if reader
                .continuity_error
                .as_ref()
                .is_some_and(|(failed, _)| failed == &chapter)
            {
                reader.continuity_error = None;
            }
            reader.budget_blocked.clear();
            reader.desired_anchor = usize::MAX;
            for index in reader.navigation_pixel_targets() {
                if reader.failed.remove(&index) {
                    reader.decoded_widths.remove(&index);
                }
                reader.image_retries.remove(&index);
                reader.image_retry_due.remove(&reader.row_page_id(index));
            }
        }
        ui.set_reader_continuation_error("".into());
        self.apply_chapter_navigation(ui);
    }

    pub(super) fn apply_chapter_navigation(self: &Arc<Self>, ui: &AppWindow) {
        if !self
            .view()
            .reader
            .as_ref()
            .is_some_and(|reader| reader.chapter_navigation.is_some() && !reader.mode_switching)
        {
            return;
        }
        self.settle_chapter_navigation_zoom(ui);
        self.reset_chapter_entry_zoom(ui);
        let (anchor, position, old_switching) = {
            let mut view = self.view();
            if view.section != 6 {
                return;
            }
            let Some(reader) = view.reader.as_mut() else {
                return;
            };
            let Some(intent) = reader.chapter_navigation.as_ref() else {
                return;
            };
            let metadata_error = reader.continuity_failures.get(&intent.chapter).cloned();
            let layout = reader.navigation_layout(None);
            let source_error = reader
                .segments
                .iter()
                .find(|s| s.chapter == intent.chapter)
                .and_then(|s| s.source.as_ref())
                .is_some_and(|source| !source.availability().is_available());
            let pixel_error = layout.as_ref().and_then(|layout| {
                layout.spans.iter().find_map(|(index, span)| {
                    (reader.failed.contains(index)
                        && !reader.navigation_pixels_ready(*index, *span)
                        && !reader
                            .image_retry_due
                            .contains_key(&reader.row_page_id(*index)))
                    .then(|| {
                        ui.get_pages()
                            .row_data(*index)
                            .map(|row| row.error.to_string())
                            .filter(|message| !message.is_empty())
                            .unwrap_or_else(|| text::tr("Could not load image"))
                    })
                })
            });
            if let Some(message) = metadata_error
                .or_else(|| source_error.then(|| text::tr("Source unavailable")))
                .or(pixel_error)
            {
                reader.chapter_navigation = None;
                reader.desired_anchor = usize::MAX;
                let index = reader.anchor.page_index;
                drop(view);
                ui.set_reader_continuation_error(message.into());
                self.request_images(ui, index);
                self.publish_reader_controls(ui);
                return;
            }
            let ready = layout.as_ref().is_some_and(|layout| {
                layout.anchor.is_some()
                    && layout
                        .spans
                        .iter()
                        .all(|(index, span)| reader.navigation_pixels_ready(*index, *span))
            });
            if !ready {
                let index = reader.anchor.page_index;
                drop(view);
                self.request_images(ui, index);
                self.maybe_preload_next(ui);
                return;
            }
            let layout = layout.unwrap();
            let (index, offset) = layout.anchor.unwrap();
            if let Some(previous) = reader.activate_page(index) {
                let _ = self.progress_checkpoint.enqueue(previous);
            }
            reader.anchor =
                ResumeAnchor::new(reader.page_id(index), index, offset).unwrap_or_default();
            reader.chapter_navigation = None;
            reader.reverse_demand = None;
            reader.desired_anchor = usize::MAX;
            reader.budget_blocked.clear();
            reader.completion_eligible = true;
            reader.completed |=
                reader.chapter_finished() || reader.selected_image_finishes_chapter(index);
            let old_switching = reader.mode_switching;
            reader.mode_switching = true;
            let progress = reader.progress_record();
            view.pending = Some(progress);
            (index, layout.position, old_switching)
        };
        ui.set_scroll_x(0.);
        ui.set_current_page(anchor as i32);
        self.restore_position(ui);
        if let Some(position) = position {
            ui.set_scroll_y(-(position as f32) * ui.get_reader_width());
        } else {
            ui.set_scroll_y(0.);
        }
        if let Some(reader) = self.view().reader.as_mut() {
            reader.mode_switching = old_switching;
        }
        ui.set_reader_continuation_error("".into());
        self.publish_reader_controls(ui);
        self.request_images(ui, anchor);
        self.maybe_preload_next(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(mode: ReadingMode, backwards: bool) -> Reader {
        let mut reader = super::super::tests::tall_reader();
        reader.pages = (0..9)
            .map(|index| PageInput::Local {
                path: PathBuf::new(),
                id: format!("page-{}", index % 3),
            })
            .collect();
        reader.ratios = vec![30.; 9];
        reader.viewport_width = 400.;
        reader.viewport_height = 600.;
        reader.gap_pixels = 0.;
        reader.mode = mode;
        reader.segments = (0..3)
            .map(|index| reader_continuity::ChapterSegment {
                chapter: ChapterId::new(reader.chapter.series.clone(), format!("chapter-{index}")),
                title: format!("Chapter {index}"),
                start: index * 3,
                end: index * 3 + 3,
                source: None,
                completed: false,
            })
            .collect();
        reader.chapter = reader.segments[1].chapter.clone();
        reader.anchor = ResumeAnchor::new(reader.page_id(4), 4, 0.375).unwrap();
        reader.chapter_navigation = Some(ChapterNavigation {
            chapter: reader.segments[if backwards { 0 } else { 2 }]
                .chapter
                .clone(),
            backwards,
        });
        reader
    }

    fn measure(reader: &mut Reader, index: usize, width: u32, height: u32, split: bool) {
        let dimensions = reader_engine::PageDimensions { width, height };
        let mut presentation = reader_engine::page_presentation(
            &image::RgbaImage::new(1, 1),
            reader_engine::DisplayOptions::default(),
        )
        .unwrap();
        presentation.source = dimensions;
        presentation.cropped = dimensions;
        presentation.effective = dimensions;
        presentation.split = split;
        presentation.half_height = if split { height / 2 } else { 0 };
        reader.presentations.insert(index, presentation);
        reader.native_dimensions.insert(index, dimensions);
        reader.ratios[index] = height as f32 / width as f32;
    }

    fn retain(reader: &mut Reader, index: usize, first_row: u32, rows: u32, full_height: u32) {
        reader.retained.insert(index);
        reader.decoded_widths.insert(index, 100);
        reader.decoded_windows.insert(
            index,
            reader_engine::DisplayWindowPlan {
                width: 100,
                first_row,
                rows,
                full_height,
            },
        );
    }

    fn seed_fixture_metadata(app: &Arc<Controller>, chapters: &[Chapter]) {
        let series_id = chapters.first().unwrap().id.series.clone();
        app.services
            .domain
            .update(|state| {
                state.series = vec![Series {
                    id: series_id,
                    title: "Chapter navigation fixture".into(),
                    publication_status: Default::default(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                }];
                state.chapters = chapters.to_vec();
                Ok(())
            })
            .unwrap();
    }

    fn zoom_fixture() -> (
        Arc<Controller>,
        AppWindow,
        tempfile::TempDir,
        mpsc::Receiver<ImageTask>,
    ) {
        i_slint_backend_testing::init_no_event_loop();
        let directory = tempfile::tempdir().unwrap();
        let ui = AppWindow::new().unwrap();
        ui.set_section(6);
        let (app, receiver) = super::super::reader_handoff_tests::fixture_app(
            &ui,
            directory.path(),
            reader(ReadingMode::Webtoon, false),
            false,
        );
        crate::reader_settings::init().unwrap();
        crate::reader_settings::update("fullscreen", 0).unwrap();
        crate::reader_settings::update("keep_awake", 0).unwrap();
        reader_controls::wire(&app, &ui);
        ui.on_reader_reflow({
            let owner = Arc::downgrade(&app);
            let weak = ui.as_weak();
            move |width, height| {
                if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                    app.reflow(&ui, width, height);
                }
            }
        });
        ui.on_reader_position({
            let owner = Arc::downgrade(&app);
            let weak = ui.as_weak();
            move |offset, width, height| {
                if let (Some(app), Some(ui)) = (owner.upgrade(), weak.upgrade()) {
                    app.position(&ui, offset, width, height);
                }
            }
        });
        ui.window().set_size(slint::LogicalSize::new(400., 600.));
        ui.set_reader_side_padding(0);
        ui.global::<crate::Anim>().set_enabled(true);
        ui.global::<crate::ReaderSettings>()
            .set_page_animations(true);
        ui.show().unwrap();
        (app, ui, directory, receiver)
    }

    fn mount_zoom_fixture(app: &Arc<Controller>, ui: &AppWindow) {
        ui.invoke_cancel_reader_zoom();
        ui.set_zoom(1.);
        let mut reader = reader(ReadingMode::Webtoon, false);
        reader.chapter_navigation = None;
        reader.preload_epoch = app.generation.load(Ordering::Acquire);
        reader.pages.truncate(3);
        reader.ratios.truncate(3);
        for (index, segment) in reader.segments.iter_mut().enumerate() {
            segment.start = index;
            segment.end = index + 1;
        }
        reader.chapter = reader.segments[1].chapter.clone();
        reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.375).unwrap();
        for index in 0..3 {
            measure(&mut reader, index, 100, 1000, false);
            retain(&mut reader, index, 0, 1000, 1000);
        }
        let chapters: Vec<Chapter> = reader
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| Chapter {
                id: segment.chapter.clone(),
                title: segment.title.clone(),
                label: String::new(),
                number: Some(index.to_string()),
                volume: None,
                source_order: index as i64,
                language: None,
                scanlator: None,
                uploaded_at: None,
            })
            .collect();
        seed_fixture_metadata(app, &chapters);
        *app.view() = View {
            section: 6,
            reader: Some(reader),
            chapters,
            ..Default::default()
        };
        ui.set_section(6);
        ui.set_reading_mode(0);
        ui.set_current_page(1);
        app.mount_pages(ui);
        let rows = ui.get_pages();
        for index in 0..rows.row_count() {
            let mut row = rows.row_data(index).unwrap();
            row.loaded = true;
            rows.set_row_data(index, row);
        }
        app.reflow(ui, ui.get_reader_width(), ui.get_reader_viewport_height());
        app.restore_position(ui);
    }

    fn start_intermediate_zoom(app: &Arc<Controller>, ui: &AppWindow) -> f32 {
        let old_width = app.view().reader.as_ref().unwrap().viewport_width;
        ui.invoke_reader_double_tap(0.5, 0.4);
        // Drive the real production animation frame callback deterministically;
        // frames scale retained artwork and deliberately do not reflow Reader.
        ui.invoke_reader_zoom_frame(0.4);
        assert!(ui.get_reader_zoom_animating());
        assert!((ui.get_zoom() - 1.4).abs() < 0.001);
        assert!(ui.get_reader_width() > old_width * 1.3);
        assert_eq!(
            app.view().reader.as_ref().unwrap().viewport_width,
            old_width
        );
        ui.get_zoom()
    }

    #[test]
    fn animated_zoom_settles_before_ready_and_pending_chapter_entry_layouts() {
        const CHILD: &str = "READER_CHAPTER_ZOOM_SETTLEMENT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_chapter_navigation::tests::animated_zoom_settles_before_ready_and_pending_chapter_entry_layouts", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (app, ui, _directory, _receiver) = zoom_fixture();
        for backwards in [true, false] {
            for initially_ready in [true, false] {
                mount_zoom_fixture(&app, &ui);
                let target = if backwards { 0 } else { 2 };
                if !initially_ready {
                    {
                        let mut view = app.view();
                        let reader = view.reader.as_mut().unwrap();
                        reader.retained.remove(&target);
                        reader.decoded_windows.remove(&target);
                        reader.decoded_widths.remove(&target);
                    }
                    app.navigate_chapter(&ui, backwards);
                    assert!(
                        app.view()
                            .reader
                            .as_ref()
                            .unwrap()
                            .chapter_navigation
                            .is_some()
                    );
                }
                start_intermediate_zoom(&app, &ui);
                if initially_ready {
                    assert!(
                        app.view()
                            .reader
                            .as_ref()
                            .unwrap()
                            .retained
                            .contains(&target)
                    );
                    app.navigate_chapter(&ui, backwards);
                } else {
                    {
                        let mut view = app.view();
                        let reader = view.reader.as_mut().unwrap();
                        retain(reader, target, 0, 1000, 1000);
                        reader.requested.remove(&target);
                    }
                    app.apply_chapter_navigation(&ui);
                }
                assert!(!ui.get_reader_zoom_animating());
                assert_eq!(ui.get_zoom(), 1., "a fresh chapter enters at fit scale");
                let view = app.view();
                let reader = view.reader.as_ref().unwrap();
                assert_eq!(reader.anchor.page_index, target);
                assert!(
                    reader.chapter_navigation.is_none(),
                    "programmatic settle cannot cancel a pending destination"
                );
                assert!((reader.viewport_width - ui.get_reader_width()).abs() < 0.01);
                let expected = if backwards {
                    1. - f64::from(reader.viewport_height / reader.viewport_width) / 10.
                } else {
                    0.
                };
                assert!(
                    (reader.anchor.offset() - expected).abs() < 0.000001,
                    "entry must use actual fit-scale viewport geometry"
                );
                let canvas = (f64::from(reader.page_position(target))
                    + reader.anchor.offset() * 10.)
                    * f64::from(reader.viewport_width);
                assert!((-f64::from(ui.get_scroll_y()) - canvas).abs() < 0.01);
                let anchor = reader.anchor.clone();
                drop(view);
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
                assert_eq!(
                    ui.get_zoom(),
                    1.,
                    "late animation frames cannot move the chosen entry"
                );
                assert_eq!(app.view().reader.as_ref().unwrap().anchor, anchor);
            }
        }
    }

    #[test]
    fn pending_zoom_settlement_reconciles_actual_covered_viewport_before_shrink() {
        const CHILD: &str = "READER_PENDING_ZOOM_COVERAGE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_chapter_navigation::tests::pending_zoom_settlement_reconciles_actual_covered_viewport_before_shrink", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (app, ui, _directory, _receiver) = zoom_fixture();
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 4. });
        ui.window().set_size(slint::PhysicalSize::new(2048, 2048));
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::Resized {
                size: slint::LogicalSize::new(512., 512.),
            });
        mount_zoom_fixture(&app, &ui);
        let dimensions = reader_engine::PageDimensions {
            width: 2048,
            height: 8192,
        };
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.retained.clear();
            reader.decoded_windows.clear();
            reader.decoded_widths.clear();
            reader.decoded_bytes.clear();
            reader.requested.clear();
            for index in 0..3 {
                measure(reader, index, dimensions.width, dimensions.height, false);
            }
            let current = reader_engine::DisplayWindowPlan {
                width: 2048,
                full_height: 8192,
                first_row: 2048,
                rows: 4096,
            };
            reader.retained.insert(1);
            reader.decoded_windows.insert(1, current);
            reader.decoded_widths.insert(1, 2048);
            reader.decoded_bytes.insert(1, current.byte_len());
            assert_eq!(current.byte_len(), READER_MODEL_BYTES);
            view.pending = None;
        }
        app.update_reader_geometry(&ui);
        app.restore_position(&ui);
        let rows = ui.get_pages();
        for index in 0..3 {
            let mut row = rows.row_data(index).unwrap();
            row.loaded = index == 1;
            rows.set_row_data(index, row);
        }
        app.navigate_chapter(&ui, false);
        let target = app
            .view()
            .reader
            .as_ref()
            .unwrap()
            .chapter_navigation
            .as_ref()
            .unwrap()
            .chapter
            .clone();
        let old = app.view().reader.as_ref().unwrap().anchor.clone();
        ui.invoke_reader_double_tap(0.5, 0.9);
        ui.invoke_reader_zoom_frame(0.4);
        assert!(ui.get_reader_zoom_animating());
        assert!((ui.get_zoom() - 1.4).abs() < 0.001);
        app.settle_chapter_navigation_zoom(&ui);
        assert!(
            (ui.get_zoom() - 1.4).abs() < 0.001,
            "settlement itself preserves the current frame before entry policy resets it"
        );
        let mut view = app.view();
        assert!(
            view.pending.is_none(),
            "viewport reconciliation cannot commit pending destination progress"
        );
        let reader = view.reader.as_mut().unwrap();
        assert_eq!(reader.chapter_navigation.as_ref().unwrap().chapter, target);
        assert!(reader.anchor.offset() > old.offset() + 0.04);
        assert_eq!(
            reader.ratios[1],
            dimensions.height as f32 / dimensions.width as f32
        );
        let actual_start = ((-ui.get_scroll_y() / reader.viewport_width - reader.page_position(1))
            / reader.ratios[1]) as f64;
        let actual_end = actual_start
            + f64::from(reader.viewport_height / reader.viewport_width / reader.ratios[1]);
        assert!((reader.anchor.offset() - actual_start).abs() < 0.000001);
        let (width, (a, b), budget) = reader.window_request(1, dimensions).unwrap();
        let replacement =
            reader_engine::plan_display_window(dimensions, width, a, b, budget).unwrap();
        assert!(replacement.covers(actual_start, actual_end));
        assert!(replacement.byte_len() <= READER_MODEL_BYTES / 2);
        let stale = reader_engine::plan_display_window(
            dimensions,
            width,
            old.offset(),
            old.offset() + (b - a),
            budget,
        )
        .unwrap();
        assert!(
            !stale.covers(actual_start, actual_end),
            "the previous anchor really would miss visible rows under this tight budget"
        );
        reader.decoded_windows.insert(1, replacement);
        reader.decoded_bytes.insert(1, replacement.byte_len());
        reader.requested.remove(&1);
        assert!(reader.has_visible_pixels(1));
        assert!(!reader.handoff_waits_for_visible(2));
        let (width, (a, b), budget) = reader.window_request(2, dimensions).unwrap();
        let destination =
            reader_engine::plan_display_window(dimensions, width, a, b, budget).unwrap();
        assert!(reader.retained_bytes_without(2) + destination.byte_len() <= READER_MODEL_BYTES);
    }

    #[test]
    fn chapter_target_is_captured_before_zoom_settlement_changes_live_owner() {
        const CHILD: &str = "READER_CHAPTER_TARGET_BEFORE_ZOOM_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_chapter_navigation::tests::chapter_target_is_captured_before_zoom_settlement_changes_live_owner", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (app, ui, _directory, _receiver) = zoom_fixture();
        mount_zoom_fixture(&app, &ui);
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            measure(reader, 1, 100, 100, false);
            retain(reader, 1, 0, 100, 100);
            reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.95).unwrap();
        }
        app.update_reader_geometry(&ui);
        app.restore_position(&ui);
        let original = app.view().reader.as_ref().unwrap().chapter.clone();
        ui.invoke_reader_double_tap(0.5, 0.9);
        ui.invoke_reader_zoom_frame(0.4);
        assert_eq!(app.view().reader.as_ref().unwrap().chapter, original);
        app.settle_chapter_navigation_zoom(&ui);
        assert_eq!(
            app.view().reader.as_ref().unwrap().chapter.local_id,
            "chapter-2",
            "this fixture really changes the live owner on settlement"
        );
        mount_zoom_fixture(&app, &ui);
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            measure(reader, 1, 100, 100, false);
            retain(reader, 1, 0, 100, 100);
            reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.95).unwrap();
        }
        app.update_reader_geometry(&ui);
        app.restore_position(&ui);
        ui.invoke_reader_double_tap(0.5, 0.9);
        ui.invoke_reader_zoom_frame(0.4);
        app.navigate_chapter(&ui, false);
        let view = app.view();
        let reader = view.reader.as_ref().unwrap();
        assert_eq!(reader.chapter.local_id, "chapter-2");
        assert_eq!(reader.anchor.page_index, 2);
        assert_eq!(reader.anchor.offset(), 0.);
        assert_eq!(ui.get_zoom(), 1.);
        assert!(reader.chapter_navigation.is_none());
    }

    #[test]
    fn zoomed_chapter_buttons_enter_at_one_in_every_reading_mode() {
        const CHILD: &str = "READER_CHAPTER_FIT_SCALE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_chapter_navigation::tests::zoomed_chapter_buttons_enter_at_one_in_every_reading_mode", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (app, ui, _directory, _receiver) = zoom_fixture();
        for mode in [
            ReadingMode::Webtoon,
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            mount_zoom_fixture(&app, &ui);
            {
                let mut view = app.view();
                let reader = view.reader.as_mut().unwrap();
                reader.mode = mode;
            }
            ui.set_reading_mode(mode_index(mode));
            app.update_reader_geometry(&ui);
            app.reader_zoom(&ui, 2., 0.5, 0.5);
            assert_eq!(ui.get_zoom(), 2.);
            app.navigate_chapter(&ui, true);
            {
                let view = app.view();
                let reader = view.reader.as_ref().unwrap();
                assert_eq!(reader.mode, mode);
                assert_eq!(reader.anchor.page_index, 0);
                assert_eq!(ui.get_zoom(), 1.);
                let expected = if mode == ReadingMode::Webtoon {
                    1. - f64::from(reader.viewport_height / reader.viewport_width) / 10.
                } else {
                    0.
                };
                assert!((reader.anchor.offset() - expected).abs() < 0.000001);
            }
            app.reader_zoom(&ui, 2., 0.5, 0.5);
            app.navigate_chapter(&ui, false);
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            assert_eq!(reader.mode, mode);
            assert_eq!(reader.anchor.page_index, 1);
            assert_eq!(reader.anchor.offset(), 0.);
            assert_eq!(ui.get_zoom(), 1.);
        }
    }

    #[test]
    fn settled_reflow_keeps_gap_rows_boundaries_tiles_and_noop_pan_consistent() {
        const CHILD: &str = "READER_SETTLED_GAP_REFLOW_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_chapter_navigation::tests::settled_reflow_keeps_gap_rows_boundaries_tiles_and_noop_pan_consistent", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (app, ui, _directory, _receiver) = zoom_fixture();
        mount_zoom_fixture(&app, &ui);
        let model = ui.get_pages();
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(2, 2);
        pixels.make_mut_bytes().fill(88);
        let image = slint::Image::from_rgba8(pixels);
        let tile_model = ModelRc::new(VecModel::from(vec![TileRow {
            image: image.clone(),
            fraction: 1.,
            ..Default::default()
        }]));
        let mut row = model.row_data(1).unwrap();
        row.tiles = tile_model.clone();
        model.set_row_data(1, row);
        let assert_geometry = || {
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            let width = ui.get_reader_width();
            let rows = ui.get_pages();
            let boundaries = ui.get_reader_boundaries();
            for index in 0..3 {
                assert!(
                    (rows.row_data(index).unwrap().position - reader.page_position(index)).abs()
                        < 0.00001
                );
            }
            for index in 0..2 {
                let previous = rows.row_data(index).unwrap();
                let next = rows.row_data(index + 1).unwrap();
                let boundary = boundaries.row_data(index).unwrap().position * width;
                assert!((boundary - (previous.position + previous.ratio) * width).abs() < 0.01);
                assert!((next.position * width - boundary - 96.).abs() < 0.01);
            }
            assert!(rows == model);
            assert!(rows.row_data(1).unwrap().tiles == tile_model);
            assert_eq!(
                rows.row_data(1).unwrap().tiles.row_data(0).unwrap().image,
                image
            );
        };
        app.reader_zoom(&ui, 2., 0.5, 0.4);
        assert_geometry();
        app.reader_zoom(&ui, 1., 0.5, 0.4);
        assert_geometry();
        ui.window().set_size(slint::LogicalSize::new(500., 650.));
        app.reflow(&ui, ui.get_reader_width(), ui.get_reader_viewport_height());
        assert_geometry();
        app.view().reader.as_mut().unwrap().mode_switching = true;
        ui.set_scroll_y(ui.get_scroll_y() - 10.);
        app.view().reader.as_mut().unwrap().mode_switching = false;
        let pan = ui.get_scroll_y();
        app.reflow(&ui, ui.get_reader_width(), ui.get_reader_viewport_height());
        assert_eq!(
            ui.get_scroll_y(),
            pan,
            "duplicate settled callbacks cannot restore a stale pan"
        );
        assert_geometry();
    }

    #[test]
    fn pages_cancel_drops_pending_chapter_intent_and_snapshots_settled_zoom() {
        const CHILD: &str = "READER_PAGES_CANCEL_PENDING_CHAPTER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_chapter_navigation::tests::pages_cancel_drops_pending_chapter_intent_and_snapshots_settled_zoom", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (app, ui, _directory, _receiver) = zoom_fixture();
        mount_zoom_fixture(&app, &ui);
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.retained.remove(&2);
            reader.decoded_windows.remove(&2);
            reader.decoded_widths.remove(&2);
        }
        app.navigate_chapter(&ui, false);
        assert!(
            app.view()
                .reader
                .as_ref()
                .unwrap()
                .chapter_navigation
                .is_some()
        );
        let zoom = start_intermediate_zoom(&app, &ui);
        app.flush();
        let before = app.services.domain.snapshot().unwrap();
        app.open_page_overview(&ui);
        assert_eq!(ui.get_section(), 23);
        assert!(!ui.get_reader_zoom_animating());
        let (chapter, anchor, width, height, x, y) = {
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            assert!(reader.chapter_navigation.is_none());
            assert!(reader.reverse_demand.is_none());
            assert!(reader.window_demand.is_none());
            assert!((reader.viewport_width - ui.get_reader_width()).abs() < 0.01);
            assert!((reader.viewport_height - ui.get_reader_viewport_height()).abs() < 0.01);
            assert_eq!(ui.get_zoom(), zoom);
            let source_y = (reader.page_position(reader.anchor.page_index)
                + reader.ratios[reader.anchor.page_index] * reader.anchor.offset() as f32)
                * reader.viewport_width;
            assert!(
                (-ui.get_scroll_y() - source_y).abs() < 0.01,
                "Pages must suspend the actual settled source point: actual={}, source={source_y}, anchor={:?}, width={}, height={}, page_start={}, ratio={}",
                -ui.get_scroll_y(),
                reader.anchor,
                reader.viewport_width,
                reader.viewport_height,
                reader.page_position(reader.anchor.page_index),
                reader.ratios[reader.anchor.page_index]
            );
            (
                reader.chapter.clone(),
                reader.anchor.clone(),
                reader.viewport_width,
                reader.viewport_height,
                ui.get_scroll_x(),
                ui.get_scroll_y(),
            )
        };
        app.flush();
        let after = app.services.domain.snapshot().unwrap();
        assert_eq!(
            after.progress, before.progress,
            "Pages settlement cannot save a new reading position"
        );
        assert_eq!(
            after.history, before.history,
            "Pages settlement cannot invent history"
        );
        app.overview_back(&ui);
        assert_eq!(ui.get_section(), 6);
        assert_eq!(app.view().reader.as_ref().unwrap().chapter, chapter);
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, anchor);
        assert_eq!(app.view().reader.as_ref().unwrap().viewport_width, width);
        assert_eq!(app.view().reader.as_ref().unwrap().viewport_height, height);
        assert_eq!(ui.get_zoom(), zoom);
        assert_eq!((ui.get_scroll_x(), ui.get_scroll_y()), (x, y));
        // Model the restarted destination worker's late final publication after
        // Back. It may populate its row but cannot resurrect the old route.
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            retain(reader, 2, 0, 1000, 1000);
            reader.requested.remove(&2);
        }
        app.apply_chapter_navigation(&ui);
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
        assert_eq!(app.view().reader.as_ref().unwrap().chapter, chapter);
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, anchor);
        assert!(
            app.view()
                .reader
                .as_ref()
                .unwrap()
                .chapter_navigation
                .is_none()
        );
        assert_eq!(app.view().reader.as_ref().unwrap().viewport_width, width);
        assert_eq!(app.view().reader.as_ref().unwrap().viewport_height, height);
        assert_eq!(ui.get_zoom(), zoom);
        assert_eq!((ui.get_scroll_x(), ui.get_scroll_y()), (x, y));
    }

    #[test]
    fn pages_zoom_owner_change_preserves_completion_progress_and_history() {
        const CHILD: &str = "READER_PAGES_ZOOM_OWNER_NEUTRALITY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_chapter_navigation::tests::pages_zoom_owner_change_preserves_completion_progress_and_history", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (app, ui, _directory, _receiver) = zoom_fixture();
        mount_zoom_fixture(&app, &ui);
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            measure(reader, 1, 100, 100, false);
            retain(reader, 1, 0, 100, 100);
            reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.95).unwrap();
            reader.completed = false;
            reader.completion_eligible = false;
            reader.segments[0].completed = true;
            reader.segments[1].completed = false;
            reader.segments[2].completed = false;
        }
        app.update_reader_geometry(&ui);
        app.restore_position(&ui);
        ui.invoke_reader_double_tap(0.5, 0.9);
        ui.invoke_reader_zoom_frame(0.4);
        assert!(ui.get_reader_zoom_animating());
        assert_eq!(
            app.view().reader.as_ref().unwrap().chapter.local_id,
            "chapter-1"
        );
        app.flush();
        let pending = app.view().reader.as_ref().unwrap().progress_record();
        app.progress_checkpoint
            .flush(Some(pending.clone()))
            .unwrap();
        let before = app.services.domain.snapshot().unwrap();
        app.view().pending = Some(pending.clone());
        let completion = app
            .view()
            .reader
            .as_ref()
            .unwrap()
            .segments
            .iter()
            .map(|s| s.completed)
            .collect::<Vec<_>>();
        app.open_page_overview(&ui);
        assert_eq!(ui.get_section(), 23);
        {
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            assert_eq!(
                reader.chapter.local_id, "chapter-2",
                "the settled viewport really crosses chapter ownership"
            );
            assert_eq!(
                reader
                    .segments
                    .iter()
                    .map(|s| s.completed)
                    .collect::<Vec<_>>(),
                completion
            );
            assert!(
                !reader.completed,
                "Pages cannot complete a chapter by reconciling zoom"
            );
            assert_eq!(
                view.pending.as_ref(),
                Some(&pending),
                "Pages preserves the existing pending reading checkpoint"
            );
            let source_y = (reader.page_position(reader.anchor.page_index)
                + reader.ratios[reader.anchor.page_index] * reader.anchor.offset() as f32)
                * reader.viewport_width;
            assert!((-ui.get_scroll_y() - source_y).abs() < 0.01);
        }
        app.flush();
        let after = app.services.domain.snapshot().unwrap();
        assert_eq!(after.progress, before.progress);
        assert_eq!(after.history, before.history);
        let anchor = app.view().reader.as_ref().unwrap().anchor.clone();
        let pan = (ui.get_scroll_x(), ui.get_scroll_y());
        app.overview_back(&ui);
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, anchor);
        assert_eq!((ui.get_scroll_x(), ui.get_scroll_y()), pan);
        assert!((ui.get_zoom() - 1.4).abs() < 0.001);
        app.flush();
        let returned = app.services.domain.snapshot().unwrap();
        assert_eq!(returned.progress, before.progress);
        assert_eq!(returned.history, before.history);
        assert!(!app.view().reader.as_ref().unwrap().completion_eligible);
        // Periodic checkpoints drain pending state. Exit also clears Overview
        // before its flush; neither operation is a genuine reader selection.
        app.checkpoint_progress();
        app.checkpoint_progress();
        app.navigate(&ui, 4);
        let exited = app.services.domain.snapshot().unwrap();
        assert_eq!(exited.progress, before.progress);
        assert_eq!(exited.history, before.history);
    }

    #[test]
    fn real_reader_actions_reenable_completion_after_neutral_pages_return() {
        const CHILD: &str = "READER_PAGES_COMPLETION_REAL_INPUT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Independent app directories keep each follow-up free of the
            // previous case's intentional History-clear sync tombstones.
            for action in 0..3 {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "app::reader_chapter_navigation::tests::real_reader_actions_reenable_completion_after_neutral_pages_return",
                        "--nocapture",
                    ])
                    .env(CHILD, action.to_string())
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        }
        let (app, ui, _directory, _receiver) = zoom_fixture();
        let action: usize = std::env::var(CHILD).unwrap().parse().unwrap();
        {
            mount_zoom_fixture(&app, &ui);
            app.services
                .domain
                .update(|state| {
                    state.progress.clear();
                    state.history.clear();
                    Ok(())
                })
                .unwrap();
            {
                let mut view = app.view();
                let reader = view.reader.as_mut().unwrap();
                measure(reader, 1, 100, 100, false);
                retain(reader, 1, 0, 100, 100);
                reader.anchor = ResumeAnchor::new(reader.page_id(1), 1, 0.95).unwrap();
                reader.completed = false;
                reader.completion_eligible = false;
            }
            app.update_reader_geometry(&ui);
            app.restore_position(&ui);
            ui.invoke_reader_double_tap(0.5, 0.9);
            ui.invoke_reader_zoom_frame(0.4);
            app.open_page_overview(&ui);
            app.overview_back(&ui);
            let chapter = app.view().reader.as_ref().unwrap().chapter.clone();
            assert_eq!(chapter.local_id, "chapter-2");
            assert!(!app.view().reader.as_ref().unwrap().completion_eligible);
            app.checkpoint_progress();
            app.checkpoint_progress();
            app.progress_checkpoint.flush(None).unwrap();
            let before = app.services.domain.snapshot().unwrap();
            // Decode/restoration may publish again after returning from Pages;
            // a duplicate position callback is still neutral.
            app.position_internal(
                &ui,
                -ui.get_scroll_y(),
                ui.get_reader_width(),
                ui.get_reader_viewport_height(),
                PositionOrigin::Restoration,
            );
            app.position(
                &ui,
                -ui.get_scroll_y(),
                ui.get_reader_width(),
                ui.get_reader_viewport_height(),
            );
            assert!(!app.view().reader.as_ref().unwrap().completion_eligible);
            app.checkpoint_progress();
            app.progress_checkpoint.flush(None).unwrap();
            let restored = app.services.domain.snapshot().unwrap();
            assert_eq!(restored.progress, before.progress);
            assert_eq!(restored.history, before.history);
            match action {
                0 => app.reader_seek(&ui, 1.),
                1 => {
                    // Move further into the already selected tall final image.
                    app.position(
                        &ui,
                        -ui.get_scroll_y() + 20.,
                        ui.get_reader_width(),
                        ui.get_reader_viewport_height(),
                    );
                }
                _ => {
                    app.open_page_overview(&ui);
                    let id = app.view().reader.as_ref().unwrap().page_id(2);
                    app.overview_select(&ui, &id);
                    assert_eq!(ui.get_section(), 6);
                }
            }
            assert!(app.view().reader.as_ref().unwrap().completion_eligible);
            assert!(app.view().reader.as_ref().unwrap().completed);
            assert!(app.view().pending.as_ref().unwrap().completed);
            app.flush();
            let after = app.services.domain.snapshot().unwrap();
            let progress = after
                .progress
                .iter()
                .find(|p| p.chapter == chapter)
                .unwrap();
            assert!(progress.completed);
            assert!(progress.anchor.is_cleared());
            assert!(after.history.iter().any(|h| h.chapter == chapter));
        }
    }

    #[test]
    fn ordinary_detail_history_resume_has_no_chapter_edge_intent() {
        let mut reader = reader(ReadingMode::Webtoon, true);
        reader.chapter_navigation = None;
        let before = reader.progress_record();
        assert!(reader.navigation_metadata_target().is_none());
        assert!(reader.navigation_pixel_target().is_none());
        assert!(reader.navigation_pixel_targets().is_empty());
        assert!(
            reader
                .navigation_span(
                    2,
                    reader_engine::PageDimensions {
                        width: 100,
                        height: 2000
                    }
                )
                .is_none()
        );
        assert_eq!(reader.progress_record().anchor, before.anchor);
    }

    #[test]
    fn absent_or_pruned_neighbor_requests_metadata_by_stable_chapter() {
        let mut reader = reader(ReadingMode::Webtoon, true);
        let target = reader.segments.remove(0).chapter;
        assert_eq!(reader.navigation_metadata_target(), Some((&target, true)));
        assert!(reader.navigation_pixel_target().is_none());
        assert_eq!(reader.anchor.page_index, 4);
        reader.segments.insert(
            0,
            reader_continuity::ChapterSegment {
                chapter: target,
                title: "Previous".into(),
                start: 0,
                end: 3,
                source: None,
                completed: false,
            },
        );
        assert!(reader.navigation_metadata_target().is_none());
        assert_eq!(reader.navigation_pixel_target(), Some(2));
    }

    #[test]
    fn unknown_last_requests_actual_tail_without_moving_saved_anchor() {
        let reader = reader(ReadingMode::Webtoon, true);
        let before = reader.progress_record().anchor;
        assert_eq!(reader.navigation_pixel_target(), Some(2));
        assert!(reader.navigation_layout(None).unwrap().anchor.is_none());
        let dimensions = reader_engine::PageDimensions {
            width: 100,
            height: 1000,
        };
        let span = reader.navigation_span(2, dimensions).unwrap();
        assert_eq!(span, (0.85, 1.));
        let plan = reader_engine::plan_display_window(dimensions, 100, span.0, span.1, 1024 * 1024)
            .unwrap();
        assert_eq!(plan.first_row + plan.rows, plan.full_height);
        assert_eq!(reader.progress_record().anchor, before);
    }

    #[test]
    fn next_starts_first_image_and_previous_uses_real_trailing_viewport_in_every_mode() {
        for mode in [
            ReadingMode::Webtoon,
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            for backwards in [false, true] {
                let mut reader = reader(mode, backwards);
                let index = if backwards { 2 } else { 6 };
                measure(&mut reader, index, 100, 1000, false);
                let layout = reader.navigation_layout(None).unwrap();
                let offset = if backwards && mode == ReadingMode::Webtoon {
                    0.85
                } else {
                    0.
                };
                assert_eq!(layout.anchor, Some((index, offset)), "{mode:?} {backwards}");
                assert_eq!(reader.navigation_pixel_target(), Some(index));
                retain(&mut reader, index, 0, 1000, 1000);
                assert!(reader.navigation_pixel_target().is_none());
                assert_eq!(
                    reader.anchor.page_index, 4,
                    "preparation keeps the live anchor"
                );
            }
        }
    }

    #[test]
    fn split_last_half_and_first_half_follow_reading_order_in_all_pagers() {
        for mode in [
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            for backwards in [false, true] {
                let mut reader = reader(mode, backwards);
                let index = if backwards { 2 } else { 6 };
                measure(&mut reader, index, 100, 1000, true);
                let layout = reader.navigation_layout(None).unwrap();
                assert_eq!(
                    layout.anchor,
                    Some((index, if backwards { 0.5 } else { 0. }))
                );
                if backwards {
                    assert_eq!(layout.spans, vec![(index, (0.5, 1.))]);
                    retain(&mut reader, index, 0, 500, 1000);
                    assert_eq!(reader.navigation_pixel_target(), Some(index));
                    retain(&mut reader, index, 500, 500, 1000);
                    assert!(reader.navigation_pixel_target().is_none());
                }
            }
        }
    }

    #[test]
    fn short_tail_keeps_ready_images_while_probing_previous_geometry() {
        let mut reader = reader(ReadingMode::Webtoon, true);
        measure(&mut reader, 2, 100, 100, false);
        retain(&mut reader, 2, 0, 100, 100);
        let layout = reader.navigation_layout(None).unwrap();
        assert!(layout.anchor.is_none());
        assert_eq!(reader.navigation_pixel_targets(), vec![2, 1]);
        assert_eq!(reader.navigation_pixel_target(), Some(1));
        measure(&mut reader, 1, 100, 200, false);
        let layout = reader.navigation_layout(None).unwrap();
        assert_eq!(layout.anchor, Some((1, 0.75)));
        assert_eq!(layout.spans, vec![(2, (0., 1.)), (1, (0.75, 1.))]);
        retain(&mut reader, 1, 150, 50, 200);
        assert!(reader.navigation_pixel_target().is_none());
    }

    #[test]
    fn short_first_images_need_real_start_viewport_before_entry() {
        let mut reader = reader(ReadingMode::Webtoon, false);
        measure(&mut reader, 6, 100, 100, false);
        retain(&mut reader, 6, 0, 100, 100);
        assert_eq!(reader.navigation_pixel_targets(), vec![6, 7]);
        assert_eq!(reader.navigation_pixel_target(), Some(7));
        measure(&mut reader, 7, 100, 200, false);
        let layout = reader.navigation_layout(None).unwrap();
        assert_eq!(layout.anchor, Some((6, 0.)));
        assert_eq!(layout.spans, vec![(6, (0., 1.)), (7, (0., 0.25))]);
    }

    #[test]
    fn true_tail_counts_gaps_and_clamps_to_target_chapter_start() {
        let mut reader = reader(ReadingMode::Webtoon, true);
        reader.gap_pixels = 40.;
        measure(&mut reader, 2, 100, 100, false);
        measure(&mut reader, 1, 100, 200, false);
        let layout = reader.navigation_layout(None).unwrap();
        assert!((layout.anchor.unwrap().1 - 0.8).abs() < 0.000001);
        reader.viewport_height = 100000.;
        measure(&mut reader, 0, 100, 100, false);
        let layout = reader.navigation_layout(None).unwrap();
        assert_eq!(layout.anchor, Some((0, 0.)));
        assert_eq!(layout.position, Some(0.));
    }

    #[test]
    fn effective_presentation_geometry_wins_over_native_dimensions_and_placeholder_ratios() {
        let mut reader = reader(ReadingMode::Webtoon, true);
        measure(&mut reader, 2, 100, 1000, false);
        reader.native_dimensions.insert(
            2,
            reader_engine::PageDimensions {
                width: 900,
                height: 200,
            },
        );
        reader.ratios[2] = 0.1;
        assert_eq!(
            reader.navigation_layout(None).unwrap().anchor,
            Some((2, 0.85))
        );
    }

    #[test]
    fn retained_previous_recipe_waits_for_real_replacement_pixels() {
        let mut reader = reader(ReadingMode::Webtoon, true);
        measure(&mut reader, 2, 100, 1000, false);
        retain(&mut reader, 2, 0, 1000, 1000);
        assert!(reader.navigation_pixel_target().is_none());
        reader.decoded_widths.clear();
        // The real worker can publish a fresh presentation before final pixels.
        measure(&mut reader, 2, 100, 1000, false);
        assert_eq!(reader.navigation_pixel_target(), Some(2));
        assert_eq!(reader.anchor.page_index, 4);
        reader.decoded_widths.insert(2, 100);
        assert!(reader.navigation_pixel_target().is_none());
    }

    #[test]
    fn reversed_and_cancelled_intents_release_old_target_identity() {
        let mut reader = reader(ReadingMode::Webtoon, true);
        let before = reader.progress_record().anchor;
        assert_eq!(reader.navigation_pixel_targets(), vec![2]);
        reader.chapter_navigation = Some(ChapterNavigation {
            chapter: reader.segments[2].chapter.clone(),
            backwards: false,
        });
        assert_eq!(reader.navigation_pixel_targets(), vec![6]);
        reader.failed.insert(2);
        reader.chapter_navigation = None;
        assert!(reader.navigation_pixel_targets().is_empty());
        assert_eq!(reader.progress_record().anchor, before);
    }

    #[test]
    fn retained_controller_navigation_and_terminal_errors_preserve_the_live_reader() {
        const CHILD: &str = "READER_EXPLICIT_CHAPTER_NAVIGATION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::reader_chapter_navigation::tests::retained_controller_navigation_and_terminal_errors_preserve_the_live_reader", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let directory = tempfile::tempdir().unwrap();
        let services = Services::open(&directory.path().join("data")).unwrap();
        crate::reader_settings::init().unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(1000., 600.));
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
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::default(),
            generation: AtomicU64::new(17),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            metadata_refresh: Arc::default(),
            continuation_work: Default::default(),
        });
        fn mount(app: &Arc<Controller>, ui: &AppWindow, mode: ReadingMode) {
            let mut reader = reader(mode, true);
            reader.chapter_navigation = None;
            reader.preload_epoch = app.generation.load(Ordering::Acquire);
            for index in 0..reader.pages.len() {
                measure(&mut reader, index, 100, 1000, mode != ReadingMode::Webtoon);
                retain(&mut reader, index, 0, 1000, 1000);
            }
            let chapters: Vec<Chapter> = reader
                .segments
                .iter()
                .enumerate()
                .map(|(index, segment)| Chapter {
                    id: segment.chapter.clone(),
                    title: segment.title.clone(),
                    label: String::new(),
                    number: Some(index.to_string()),
                    volume: None,
                    source_order: index as i64,
                    language: None,
                    scanlator: None,
                    uploaded_at: None,
                })
                .collect();
            seed_fixture_metadata(app, &chapters);
            *app.view() = View {
                section: 6,
                reader: Some(reader),
                chapters,
                ..Default::default()
            };
            ui.set_section(6);
            ui.set_reading_mode(mode_index(mode));
            app.mount_pages(ui);
        }
        for mode in [
            ReadingMode::Webtoon,
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            mount(&app, &ui, mode);
            let model = ui.get_pages();
            let nonce = {
                let mut view = app.view();
                let reader = view.reader.as_mut().unwrap();
                reader.preloading = Some(reader.segments[2].chapter.clone());
                reader.preload_nonce
            };
            app.navigate_chapter(&ui, true);
            {
                let view = app.view();
                let reader = view.reader.as_ref().unwrap();
                assert_eq!(reader.chapter.local_id, "chapter-0");
                assert_eq!(reader.anchor.page_index, 2);
                assert_eq!(
                    reader.anchor.offset(),
                    if mode == ReadingMode::Webtoon {
                        0.85
                    } else {
                        0.5
                    }
                );
                assert!(reader.chapter_navigation.is_none());
                assert!(
                    reader.preloading.is_none(),
                    "a retained entry cancels the obsolete metadata slot"
                );
                assert_eq!(reader.preload_nonce, nonce.wrapping_add(1));
                let pending = view.pending.as_ref().unwrap();
                assert_eq!(pending.chapter, reader.chapter);
                assert!(
                    pending.completed,
                    "selecting the final source image completes the chapter"
                );
                assert!(
                    pending.anchor.is_cleared(),
                    "completed chapters have no resume page"
                );
                assert!(ui.get_pages() == model, "navigation retains the page model");
                assert_eq!(app.generation.load(Ordering::Acquire), 17);
            }
            // Re-add a retained destination after the scheduler has legitimately
            // released its old offscreen pixels. The same live reader remains.
            {
                let mut view = app.view();
                retain(view.reader.as_mut().unwrap(), 3, 0, 1000, 1000);
            }
            app.navigate_chapter(&ui, false);
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            assert_eq!(reader.chapter.local_id, "chapter-1");
            assert_eq!(reader.anchor.page_index, 3);
            assert_eq!(reader.anchor.offset(), 0.);
        }

        mount(&app, &ui, ReadingMode::Webtoon);
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.chapter_navigation = Some(ChapterNavigation {
                chapter: reader.segments[0].chapter.clone(),
                backwards: true,
            });
            reader.retained.remove(&2);
            reader.failed.insert(2);
            let due = std::time::Instant::now() + std::time::Duration::from_secs(60);
            reader.image_retry_due.insert(reader.row_page_id(2), due);
        }
        let before = app.view().reader.as_ref().unwrap().anchor.clone();
        app.apply_chapter_navigation(&ui);
        assert!(
            app.view()
                .reader
                .as_ref()
                .unwrap()
                .chapter_navigation
                .is_some(),
            "an automatic retry keeps the intent live"
        );
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, before);
        app.view().reader.as_mut().unwrap().image_retry_due.clear();
        app.apply_chapter_navigation(&ui);
        assert!(
            app.view()
                .reader
                .as_ref()
                .unwrap()
                .chapter_navigation
                .is_none(),
            "terminal page failure releases pending intent"
        );
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, before);
        assert!(!ui.get_reader_continuation_error().is_empty());

        mount(&app, &ui, ReadingMode::Webtoon);
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            let chapter = reader.segments[0].chapter.clone();
            reader.chapter_navigation = Some(ChapterNavigation {
                chapter: chapter.clone(),
                backwards: true,
            });
            reader
                .continuity_failures
                .insert(chapter, "metadata failed".into());
        }
        app.apply_chapter_navigation(&ui);
        assert!(
            app.view()
                .reader
                .as_ref()
                .unwrap()
                .chapter_navigation
                .is_none()
        );
        assert_eq!(ui.get_reader_continuation_error(), "metadata failed");
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, before);

        mount(&app, &ui, ReadingMode::Webtoon);
        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.failed.insert(2);
            assert!(reader.navigation_pixels_ready(2, (0.85, 1.)));
        }
        app.navigate_chapter(&ui, true);
        {
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            assert!(!reader.failed.contains(&2));
            assert!(
                !reader.decoded_widths.contains_key(&2),
                "explicit retry requires a real replacement completion"
            );
            assert!(reader.chapter_navigation.is_some());
            assert_eq!(reader.anchor, before);
        }
        {
            let mut view = app.view();
            retain(view.reader.as_mut().unwrap(), 2, 0, 1000, 1000);
        }
        app.apply_chapter_navigation(&ui);
        let nonce = {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            assert_eq!(reader.chapter.local_id, "chapter-0");
            reader.chapter_navigation = Some(ChapterNavigation {
                chapter: reader.segments[1].chapter.clone(),
                backwards: false,
            });
            reader.preloading = Some(reader.segments[2].chapter.clone());
            reader.preload_nonce
        };
        app.navigate_chapter(&ui, true);
        {
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            assert!(
                reader.chapter_navigation.is_none(),
                "inventory-edge reversal cancels the pending entry"
            );
            assert!(reader.preloading.is_none());
            assert_eq!(reader.preload_nonce, nonce.wrapping_add(1));
            assert_eq!(reader.chapter.local_id, "chapter-0");
        }
    }
}

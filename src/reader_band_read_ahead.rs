//! Idle read-ahead for the image worker: bands of pages stored as strips, and
//! the encoded bytes of pages beyond the decode window.
//!
//! A strip enters the display window when the viewport nears its leading edge.
//! Decoding it at that moment makes the window shift (and the scroll admission
//! behind it) wait for a codec. While the image worker has nothing queued it
//! instead decodes, one band at a time, the bands just beyond the window edge in
//! the scroll direction, so the shift only has to resample. Visible work always
//! wins: the worker looks at its queue between bands, the step runs only when
//! `defers_prefetch` and the reverse-handoff budgets say the page is not
//! waiting behind something else, and the decode permit is taken at prefetch
//! priority unless the worker already holds one.
use super::*;

/// Pages beyond the decode window whose bytes are fetched while the viewer is
/// idle, and the most encoded bytes those may hold (the worker's encoded cache
/// is 32 MiB; the pages being read keep the rest).
const FETCH_AHEAD_PAGES: usize = 3;
const FETCH_AHEAD_BYTES: usize = 12 * 1024 * 1024;
/// Marks the active-image slot while an idle fetch runs. `request_images`
/// cancels it as soon as visible work is queued behind it.
pub(super) const FETCH_AHEAD_SLOT: usize = usize::MAX;

/// Output rows to read ahead beyond the window edge, in viewport heights. A
/// window shift can also advance the edge by up to two display strips (the
/// edge sits one to two strips past the viewport and snaps to the strip grid),
/// so that distance is the floor: the strips a shift brings in are never
/// further than this.
const READ_AHEAD_VIEWPORTS: f32 = 2.;
const READ_AHEAD_MIN_ROWS: u32 = 2 * reader_engine::DISPLAY_TILE_ROWS;

// The engine trims resident bands to the same number the reader budgets for
// its model, so read-ahead can never hold more than the model may.
const _: () = assert!(READER_MODEL_BYTES as u64 == reader_engine::BAND_RETAIN_BYTES);

impl PageInput {
    /// Whether idle fetch-ahead helps this page: it comes over the network.
    /// Downloaded pages are local files.
    fn fetches_ahead(&self) -> bool {
        match self {
            Self::Remote(_) => true,
            #[cfg(test)]
            Self::Local { .. } => true,
            Self::Verified(_) => false,
        }
    }
}

impl Reader {
    /// Nothing the viewer is looking at waits for the worker.
    fn idle_for_read_ahead(&self) -> bool {
        if self.mode_switching
            || self.pages.is_empty()
            || !self.requested.is_empty()
            || self.reverse_target().is_some()
            || self.chapter_navigation.is_some()
        {
            return false;
        }
        let anchor = self.anchor.page_index.min(self.pages.len() - 1);
        (anchor..=self.visible_end())
            .all(|i| self.failed.contains(&i) || self.has_visible_pixels(i))
    }

    /// Pages just beyond the decode window in the scroll direction whose bytes
    /// are worth fetching now, nearest first.
    pub(super) fn fetch_ahead_candidates(&self) -> Vec<usize> {
        let (Some(first), Some(last)) = (self.desired.first(), self.desired.last()) else {
            return Vec::new();
        };
        if !self.idle_for_read_ahead() {
            return Vec::new();
        }
        let beyond: Vec<usize> = if self.continuity_backwards {
            (first.saturating_sub(FETCH_AHEAD_PAGES)..*first)
                .rev()
                .collect()
        } else {
            (last + 1..=(last + FETCH_AHEAD_PAGES).min(self.pages.len() - 1)).collect()
        };
        beyond
            .into_iter()
            .filter(|i| !self.failed.contains(i) && self.pages[*i].fetches_ahead())
            .collect()
    }

    /// What the worker may read ahead for `index` right now, or `None` while
    /// the page has nothing ahead of its window in the scroll direction, is not
    /// a page the viewer is about to reach, or other work has the right of way.
    pub(super) fn band_read_ahead(&self, index: usize) -> Option<reader_engine::ReadAhead> {
        if self.mode != ReadingMode::Webtoon
            || self.mode_switching
            || self.viewport_width <= 0.
            || self.viewport_height <= 0.
            || self.pages.is_empty()
            || self.failed.contains(&index)
            || !self.desired.contains(&index)
            || self.requested.contains(&index)
            || !self.retained.contains(&index)
            // A reverse handoff or explicit navigation owns the budget.
            || self.reverse_target().is_some()
            || self.chapter_navigation.is_some()
            || self.defers_prefetch(index)
        {
            return None;
        }
        let anchor = self.anchor.page_index.min(self.pages.len() - 1);
        let end = self.visible_end();
        // Pixels the viewer is waiting for come first.
        if (anchor..=end).any(|i| !self.failed.contains(&i) && !self.has_visible_pixels(i)) {
            return None;
        }
        // The page being read, or the one just past it in the scroll direction.
        let near = anchor.saturating_sub(usize::from(self.continuity_backwards))
            ..=end.saturating_add(usize::from(!self.continuity_backwards));
        if !near.contains(&index) {
            return None;
        }
        let window = *self.decoded_windows.get(&index)?;
        let width = *self.decoded_widths.get(&index)?;
        let backwards = self.continuity_backwards;
        let more = if backwards {
            window.first_row > 0
        } else {
            window.first_row + window.rows < window.full_height
        };
        if !more {
            return None;
        }
        let viewport_rows = self.viewport_height * window.width as f32 / self.viewport_width;
        Some(reader_engine::ReadAhead {
            width,
            options: self.display_options,
            window,
            backwards,
            depth_rows: ((viewport_rows * READ_AHEAD_VIEWPORTS).ceil() as u32)
                .max(READ_AHEAD_MIN_ROWS),
            max_bytes: READER_MODEL_BYTES as u64,
        })
    }
}

impl Controller {
    /// One idle step of the image worker. `true` when a band was decoded (or
    /// the permit was momentarily busy) and the caller should look at its queue
    /// again; `false` when there is nothing to read ahead.
    pub(super) fn read_ahead_step(
        app: &Weak<Self>,
        epoch: u64,
        page: &mut Option<usize>,
        encoded: &mut EncodedPages,
        decoded: &mut reader_engine::DisplayDecoder,
        permit: &mut Option<crate::decode_admission::Permit>,
    ) -> bool {
        Self::read_ahead_bands(app, epoch, page, encoded, decoded, permit)
            || Self::fetch_ahead_step(app, epoch, encoded)
    }

    /// Fetch, but do not decode, the next page beyond the decode window whose
    /// bytes are not held yet. `true` when it made progress (fetched or gave up
    /// on one page) so the caller looks at its queue and calls again.
    fn fetch_ahead_step(app: &Weak<Self>, epoch: u64, encoded: &mut EncodedPages) -> bool {
        let Some(app) = app.upgrade() else {
            return false;
        };
        if !app.image_worker_live.load(Ordering::Acquire)
            || app.generation.load(Ordering::Acquire) != epoch
        {
            return false;
        }
        let (index, input, chapter, source) = {
            let view = app.view();
            let Some(reader) = view.reader.as_ref() else {
                return false;
            };
            if view.section != 6 {
                return false;
            }
            let candidates = reader.fetch_ahead_candidates();
            if encoded.bytes_of(candidates.iter().copied()) >= FETCH_AHEAD_BYTES {
                return false;
            }
            let Some(index) = candidates
                .into_iter()
                .find(|i| !encoded.holds(epoch, *i) && !encoded.skipped(epoch, *i))
            else {
                return false;
            };
            (
                index,
                reader.pages[index].clone(),
                reader.owner(index).clone(),
                reader.source_for(index),
            )
        };
        let cancel = CancelToken::default();
        *app.active_image.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((FETCH_AHEAD_SLOT, cancel.clone()));
        let outcome = (|| -> Result<()> {
            match &input {
                PageInput::Remote(page) => {
                    let source = source.ok_or_else(|| text::tr("Source unavailable"))?;
                    if !source.availability().is_available() {
                        return Err(text::tr("Source unavailable"));
                    }
                    let weak = Arc::downgrade(&app);
                    let request = app.chapter_cache.prepare(
                        &chapter,
                        page,
                        &source,
                        cancel.clone(),
                        Arc::new(move || {
                            weak.upgrade().is_some_and(|owner| {
                                owner.generation.load(Ordering::Acquire) == epoch
                            })
                        }),
                    )?;
                    // Without a cache scope the bytes cannot be kept for later.
                    let key = request
                        .memory_key()
                        .ok_or_else(|| text::tr("Operation cancelled"))?;
                    let read = app.chapter_cache.read_with_chunks(&request, &mut |_| {})?;
                    request.validate_active()?;
                    cancel.check().map_err(error)?;
                    encoded.insert_ahead(
                        epoch,
                        index,
                        &key,
                        read.page,
                        Some(FetchedAhead {
                            pending: read.pending,
                            reused: read.from_cache,
                        }),
                    );
                    Ok(())
                }
                #[cfg(test)]
                PageInput::Local { .. } => {
                    let page = input.read(source.as_ref(), &cancel)?;
                    encoded.insert_ahead(epoch, index, "", page, None);
                    Ok(())
                }
                PageInput::Verified(_) => Err(text::tr("Operation cancelled")),
            }
        })();
        app.active_image
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if app.generation.load(Ordering::Acquire) != epoch {
            return false;
        }
        if outcome.is_err() {
            // The page's own task fetches it again and reports what failed.
            encoded.skip(epoch, index);
        }
        !cancel.is_cancelled()
    }

    fn read_ahead_bands(
        app: &Weak<Self>,
        epoch: u64,
        page: &mut Option<usize>,
        encoded: &EncodedPages,
        decoded: &mut reader_engine::DisplayDecoder,
        permit: &mut Option<crate::decode_admission::Permit>,
    ) -> bool {
        let (Some(index), Some(app)) = (*page, app.upgrade()) else {
            return false;
        };
        if !app.image_worker_live.load(Ordering::Acquire)
            || app.generation.load(Ordering::Acquire) != epoch
        {
            *page = None;
            return false;
        }
        let Some(source) = encoded.pages.get(&index).cloned() else {
            return false;
        };
        let request = {
            let view = app.view();
            let Some(reader) = view.reader.as_ref() else {
                return false;
            };
            if view.section != 6
                || reader.pages.get(index).is_none()
                || reader
                    .source
                    .as_ref()
                    .is_some_and(|s| !s.availability().is_available())
            {
                return false;
            }
            let Some(request) = reader.band_read_ahead(index) else {
                return false;
            };
            request
        };
        // Bands are read ahead under whatever permit the worker holds for the
        // pixels it keeps; with none, only at prefetch priority and only if it
        // is free right now (a visible thumbnail or the viewer would win).
        let borrowed = if permit.is_none() {
            *permit = crate::decode_admission::SHARED
                .try_acquire(crate::decode_admission::Priority::Prefetch);
            if permit.is_none() {
                return false;
            }
            true
        } else {
            false
        };
        let cancel = CancelToken::default();
        *app.active_image.lock().unwrap_or_else(|e| e.into_inner()) = Some((index, cancel.clone()));
        #[cfg(test)]
        let before = decoded.band_stats();
        let step = decoded.read_ahead(&source, reader_engine::current_limits(), request);
        app.active_image
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if borrowed {
            permit.take();
        }
        // Leaving the read-ahead window, a new generation or a failing band
        // ends read-ahead for this page; whatever it decoded is only a cache.
        // (A band that fails to decode surfaces when a render needs it.)
        let stale = cancel.is_cancelled() || app.generation.load(Ordering::Acquire) != epoch;
        if stale || step.is_err() {
            *page = None;
            return false;
        }
        if matches!(step, Ok(reader_engine::ReadAheadStep::Decoded)) {
            #[cfg(test)]
            reader_handoff_tests::log_bands(&app, index, before, decoded);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::reader_handoff_tests::{fixture_app, split_page};
    use reader_engine::DISPLAY_TILE_ROWS;

    /// A webtoon reader on two tall pages whose first has a published window
    /// of 6,144 rows (a viewport of 800 rows at the top).
    fn reader() -> Reader {
        let mut reader = super::super::tests::tall_reader();
        reader.ratios = vec![24576. / 512.; 2];
        reader.viewport_width = 512.;
        reader.viewport_height = 800.;
        reader.target_width = 512;
        reader.desired.extend([0, 1]);
        let plan = reader_engine::DisplayWindowPlan {
            width: 512,
            full_height: 24576,
            first_row: 0,
            rows: 3 * DISPLAY_TILE_ROWS,
        };
        reader.retained.insert(0);
        reader.decoded_widths.insert(0, 512);
        reader.decoded_bytes.insert(0, plan.byte_len());
        reader.decoded_windows.insert(0, plan);
        reader.native_dimensions.insert(
            0,
            reader_engine::PageDimensions {
                width: 512,
                height: 24576,
            },
        );
        reader
    }

    #[test]
    fn read_ahead_covers_the_rows_past_the_window_edge_in_the_scroll_direction() {
        let mut r = reader();
        let ahead = r.band_read_ahead(0).expect("rows remain below the window");
        assert!(!ahead.backwards);
        assert_eq!(ahead.window, r.decoded_windows[&0]);
        assert_eq!(ahead.width, 512);
        assert_eq!(ahead.max_bytes, READER_MODEL_BYTES as u64);
        // Two viewport heights (1,600 rows) are less than the two strips a
        // window shift can bring in.
        assert_eq!(ahead.depth_rows, 2 * DISPLAY_TILE_ROWS);
        // A taller viewport asks for two of its own heights.
        r.viewport_height = 3000.;
        assert_eq!(r.band_read_ahead(0).unwrap().depth_rows, 6000);
        // The top of the window has no rows above it; a window that reaches
        // the page end has none below.
        r.viewport_height = 800.;
        r.continuity_backwards = true;
        assert!(r.band_read_ahead(0).is_none());
        let plan = reader_engine::DisplayWindowPlan {
            first_row: 20480,
            rows: 4096,
            ..r.decoded_windows[&0]
        };
        r.decoded_windows.insert(0, plan);
        r.anchor = ResumeAnchor::new("page-0", 0, 0.9).unwrap();
        assert!(r.band_read_ahead(0).unwrap().backwards);
        r.continuity_backwards = false;
        assert!(r.band_read_ahead(0).is_none());
    }

    #[test]
    fn read_ahead_waits_for_visible_work_and_the_handoff_budgets() {
        let mut r = reader();
        assert!(r.band_read_ahead(0).is_some());
        // Not a page the viewer is about to reach.
        r.anchor = ResumeAnchor::new("page-1", 1, 0.5).unwrap();
        r.retained.insert(1);
        r.decoded_windows.insert(1, r.decoded_windows[&0]);
        r.decoded_widths.insert(1, 512);
        assert!(r.band_read_ahead(0).is_none(), "a page behind the viewer");
        r.anchor = ResumeAnchor::new("page-0", 0, 0.).unwrap();
        // A queued or failed page, or one outside the read-ahead window.
        r.requested.insert(0);
        assert!(r.band_read_ahead(0).is_none());
        r.requested.clear();
        r.failed.insert(0);
        assert!(r.band_read_ahead(0).is_none());
        r.failed.clear();
        r.desired.remove(&0);
        assert!(r.band_read_ahead(0).is_none());
        r.desired.insert(0);
        // The visible rows still lack pixels: that work is first.
        let plan = r.decoded_windows[&0];
        r.decoded_windows.insert(
            0,
            reader_engine::DisplayWindowPlan {
                first_row: DISPLAY_TILE_ROWS,
                rows: plan.rows - DISPLAY_TILE_ROWS,
                ..plan
            },
        );
        assert!(!r.has_visible_pixels(0));
        assert!(r.band_read_ahead(0).is_none());
        r.decoded_windows.insert(0, plan);
        // A reverse handoff owns the model budget.
        r.reverse_demand = Some(r.row_page_id(0));
        r.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
        assert!(r.reverse_target().is_some());
        assert!(r.band_read_ahead(0).is_none());
        r.reverse_demand = None;
        r.anchor = ResumeAnchor::new("page-0", 0, 0.).unwrap();
        // Paged modes show whole pages: no rows to read ahead.
        r.mode = ReadingMode::LeftToRight;
        assert!(r.band_read_ahead(0).is_none());
    }

    #[test]
    fn a_visible_long_page_defers_read_ahead_of_other_pages() {
        let mut r = reader();
        // Page 0 is visible with a window short of its end; page 1 is retained
        // with rows beyond its window. Page 1 must wait: defers_prefetch.
        r.retained.insert(1);
        let plan = r.decoded_windows[&0];
        r.decoded_windows.insert(1, plan);
        r.decoded_widths.insert(1, 512);
        assert!(r.defers_prefetch(1));
        assert!(r.band_read_ahead(1).is_none());
    }

    #[test]
    fn idle_read_ahead_decodes_bands_until_done_and_stops_for_stale_work() {
        const CHILD: &str = "READER_READ_AHEAD_STEP_CHILD";
        if !crate::app::reader_handoff_tests::in_child(
            "app::reader_band_read_ahead::tests::idle_read_ahead_decodes_bands_until_done_and_stops_for_stale_work",
            CHILD,
        ) {
            return;
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let ui = AppWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let PageInput::Local { path, .. } = split_page(dir.path(), 0, 512, 24576) else {
            unreachable!()
        };
        let page = crate::page_store::PageImage::from_stored(std::fs::read(path).unwrap()).unwrap();
        let mut reader = reader();
        reader.pages.truncate(1);
        reader.ratios.truncate(1);
        reader.desired = BTreeSet::from([0]);
        let (app, _receiver) = fixture_app(&ui, dir.path(), reader, true);
        let generation = app.generation.load(Ordering::Acquire);
        let limits = reader_engine::current_limits();
        let mut encoded = EncodedPages::default();
        encoded.read(generation, 0, || Ok(page.clone())).unwrap();
        let mut decoded = reader_engine::DisplayDecoder::default();
        let plan = app.view().reader.as_ref().unwrap().decoded_windows[&0];
        decoded
            .render_window_tiles(
                &page,
                limits,
                512,
                0.,
                800. / 24576.,
                32 << 20,
                reader_engine::DisplayOptions::default(),
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(decoded.band_stats().ahead, 0);
        let weak = Arc::downgrade(&app);
        let mut hint = Some(0);
        let mut permit = None;

        // Another decode holds the shared permit: read-ahead takes none.
        let held = crate::decode_admission::SHARED
            .acquire(crate::decode_admission::Priority::Viewer, || true)
            .unwrap();
        assert!(!Controller::read_ahead_step(
            &weak,
            generation,
            &mut hint,
            &mut encoded,
            &mut decoded,
            &mut permit
        ));
        assert_eq!(decoded.band_stats().ahead, 0);
        assert!(permit.is_none());
        drop(held);

        // A different generation (navigation, chapter or profile switch)
        // cancels it and forgets the page.
        assert!(!Controller::read_ahead_step(
            &weak,
            generation + 1,
            &mut hint,
            &mut encoded,
            &mut decoded,
            &mut permit
        ));
        assert_eq!((hint, decoded.band_stats().ahead), (None, 0));

        // Otherwise one band per call until the rows past the window are in,
        // never above the model budget, and the borrowed permit is returned.
        hint = Some(0);
        let mut steps = 0;
        while Controller::read_ahead_step(
            &weak,
            generation,
            &mut hint,
            &mut encoded,
            &mut decoded,
            &mut permit,
        ) {
            steps += 1;
            assert!(permit.is_none(), "the prefetch permit is not kept");
            assert!(decoded.resident_bands().1 <= READER_MODEL_BYTES as u64);
            assert!(steps < 20);
        }
        // Rows 6144..10240 are bands 3 and 4 (the window's own band 2 and the
        // background band were rendered already).
        assert_eq!(decoded.band_stats().ahead, 2, "{plan:?}");
        assert_eq!(hint, Some(0), "done is not stale");
        assert!(
            crate::decode_admission::SHARED
                .try_acquire(crate::decode_admission::Priority::Viewer)
                .is_some()
        );
        app.image_worker_live.store(false, Ordering::Release);
    }

    #[test]
    fn fetch_ahead_goes_a_few_pages_past_the_decode_window_in_the_scroll_direction() {
        let mut r = super::super::tests::tall_reader();
        r.pages = (0..12)
            .map(|i| PageInput::Local {
                path: PathBuf::new(),
                id: format!("page-{i}"),
            })
            .collect();
        r.ratios = vec![2.; 12];
        r.viewport_width = 400.;
        r.viewport_height = 600.;
        r.anchor = ResumeAnchor::new("page-5", 5, 0.).unwrap();
        r.desired = (4..=7).collect();
        let plan = reader_engine::DisplayWindowPlan {
            width: 400,
            full_height: 800,
            first_row: 0,
            rows: 800,
        };
        r.retained.extend(4..=7);
        r.decoded_windows.extend((4..=7).map(|i| (i, plan)));
        r.decoded_widths.extend((4..=7).map(|i| (i, 400)));
        assert_eq!(r.fetch_ahead_candidates(), [8, 9, 10]);
        r.continuity_backwards = true;
        assert_eq!(r.fetch_ahead_candidates(), [3, 2, 1]);
        // A failed page is left to its own task; the page list ends.
        r.failed.insert(2);
        assert_eq!(r.fetch_ahead_candidates(), [3, 1]);
        r.continuity_backwards = false;
        r.desired = (8..=11).collect();
        assert!(
            r.fetch_ahead_candidates().is_empty(),
            "nothing past the end"
        );
        // Visible work, a queued task or a handoff come first.
        r.desired = (4..=7).collect();
        r.requested.insert(6);
        assert!(r.fetch_ahead_candidates().is_empty());
        r.requested.clear();
        r.decoded_windows.remove(&5);
        assert!(
            r.fetch_ahead_candidates().is_empty(),
            "visible rows lack pixels"
        );
    }
}

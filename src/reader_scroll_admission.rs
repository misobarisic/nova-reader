//! Ordinary Webtoon gestures advance only through final decoded row ranges.
//! Explicit resume/seek remain authoritative; blocked input is never replayed.
use super::*;

#[derive(Clone)]
pub(super) struct WindowDemand {
    identity: String,
    forward: bool,
}

impl Reader {
    pub(super) fn retain_window_direction(&mut self, forward: bool) {
        if self
            .window_demand
            .as_ref()
            .is_some_and(|d| d.forward != forward)
        {
            self.window_demand = None;
        }
    }

    /// Pages to prefetch beyond `from` in the scroll direction: enough to cover
    /// about two viewport heights, at least one and at most four. Decode work
    /// beyond the visible rows still waits behind `defers_prefetch`, and what is
    /// retained is bounded by the model budget, farthest first.
    pub(super) fn read_ahead_pages(&self, from: usize, backwards: bool) -> usize {
        const MAX_PAGES: usize = 4;
        if self.mode != ReadingMode::Webtoon
            || self.viewport_width <= 0.
            || self.viewport_height <= 0.
            || from >= self.pages.len()
        {
            return 1;
        }
        let wanted = self.viewport_height * 2.;
        let mut covered = 0.;
        let mut pages = 0;
        let mut index = from;
        while pages < MAX_PAGES && covered < wanted {
            index = match if backwards {
                index.checked_sub(1)
            } else {
                Some(index + 1).filter(|next| *next < self.pages.len())
            } {
                Some(next) => next,
                None => break,
            };
            covered += self.ratios[index] * self.viewport_width;
            pages += 1;
        }
        pages.max(1)
    }

    pub(super) fn window_demand_target(&self) -> Option<usize> {
        let demand = self.window_demand.as_ref()?;
        (0..self.pages.len()).find(|i| self.row_page_id(*i) == demand.identity)
    }

    pub(super) fn demand_window_span(
        &self,
        index: usize,
        dimensions: reader_engine::PageDimensions,
        span: (f64, f64),
    ) -> (f64, f64) {
        if self.mode != ReadingMode::Webtoon {
            return span;
        }
        let Some(demand) = self
            .window_demand
            .as_ref()
            .filter(|d| self.row_page_id(index) == d.identity)
        else {
            return span;
        };
        let width = self.target_width.min(dimensions.width).max(1);
        let height =
            (u64::from(dimensions.height) * u64::from(width) / u64::from(dimensions.width)).max(1);
        // Request one adjacent output row as well as every admitted visible
        // row. The planner adds overlapping overscan when headroom permits.
        // No spare row means ordinary admission fails honestly; it cannot move
        // the anchor into a blank/disjoint replacement to manufacture space.
        let row = 1. / height as f64;
        if demand.forward {
            (span.0, (span.1 + row).min(1.))
        } else {
            ((span.0 - row).max(0.), span.1)
        }
    }

    pub(super) fn decoded_scroll_offset(
        &mut self,
        requested: f32,
        width: f32,
        viewport: f32,
    ) -> f32 {
        let current = (self.page_position(self.anchor.page_index)
            + self.ratios[self.anchor.page_index] * self.anchor.offset() as f32)
            * width;
        if (requested - current).abs() < 0.001 {
            return requested;
        }
        if !self.failed.contains(&self.anchor.page_index)
            && self.retained.contains(&self.anchor.page_index)
            && !self.has_visible_pixels(self.anchor.page_index)
        {
            // Explicit seek/resume chooses a new source point immediately;
            // subsequent gestures wait there for its replacement pixels.
            return current;
        }
        let forward = requested > current;
        let mut blocked = None;
        let mut admitted = requested;
        for step in 0..self.pages.len() {
            let index = if forward {
                self.anchor
                    .page_index
                    .checked_add(step)
                    .filter(|i| *i < self.pages.len())
            } else {
                self.anchor.page_index.checked_sub(step)
            };
            let Some(index) = index else {
                break;
            };
            let page_start = self.page_position(index) * width;
            let page_end = page_start + self.ratios[index] * width;
            if (forward && page_start > requested + viewport) || (!forward && page_end < requested)
            {
                break;
            }
            if self.failed.contains(&index) {
                continue;
            }
            let Some(plan) = self
                .decoded_windows
                .get(&index)
                .filter(|_| self.retained.contains(&index))
            else {
                continue;
            };
            let first = page_start
                + (page_end - page_start) * plan.first_row as f32 / plan.full_height as f32;
            let last = page_start
                + (page_end - page_start) * (plan.first_row + plan.rows) as f32
                    / plan.full_height as f32;
            // Keep float-to-anchor round trips inside the covered physical row.
            let inset = ((page_end - page_start) / plan.full_height as f32 * 0.25)
                .max(current.abs().max(page_end.abs()) * f32::EPSILON * 2.);
            if forward {
                if plan.first_row > 0 && first > current.max(page_start) + 0.01 {
                    admitted = requested.min((page_start - viewport).max(current));
                    blocked = Some(index);
                    break;
                }
                if plan.first_row + plan.rows < plan.full_height
                    && requested + viewport > last + 0.01
                {
                    admitted = requested.min((last - viewport - inset).max(current));
                    blocked = Some(index);
                    break;
                }
            } else {
                if index < self.anchor.page_index && plan.first_row + plan.rows < plan.full_height {
                    admitted = requested.max(page_end.min(current));
                    blocked = Some(index);
                    break;
                }
                if plan.first_row > 0 && requested < first - 0.01 {
                    admitted = requested.max((first + inset).min(current));
                    blocked = Some(index);
                    break;
                }
            }
        }
        self.window_demand = blocked.map(|index| WindowDemand {
            identity: self.row_page_id(index),
            forward,
        });
        admitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_crosses_a_covered_page_head_and_checks_the_previous_tail() {
        let mut r = super::super::tests::tall_reader();
        r.ratios = vec![10.; 2];
        r.viewport_width = 400.;
        r.viewport_height = 600.;
        r.target_width = 400;
        r.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
        r.retained.extend([0, 1]);
        let previous_tail = reader_engine::DisplayWindowPlan {
            width: 400,
            full_height: 4000,
            first_row: 3000,
            rows: 1000,
        };
        for current_rows in [1000, 4000] {
            r.decoded_windows.insert(
                1,
                reader_engine::DisplayWindowPlan {
                    width: 400,
                    full_height: 4000,
                    first_row: 0,
                    rows: current_rows,
                },
            );
            r.decoded_windows.insert(0, previous_tail);
            assert!(r.has_visible_pixels(1));
            assert_eq!(
                r.decoded_scroll_offset(3980., 400., 600.),
                3980.,
                "a covered current head is not an intra-page missing-row edge"
            );
            assert!(r.window_demand.is_none());
        }
        // Entering a previous page still requires its actual trailing rows.
        r.decoded_windows.insert(
            0,
            reader_engine::DisplayWindowPlan {
                width: 400,
                full_height: 4000,
                first_row: 0,
                rows: 1000,
            },
        );
        assert_eq!(r.decoded_scroll_offset(3980., 400., 600.), 4000.);
        assert_eq!(r.window_demand_target(), Some(0));
        // A larger jump can enter a loaded tail, but cannot outrun its true
        // leading decoded edge while the adjacent replacement is prepared.
        r.decoded_windows.insert(0, previous_tail);
        let admitted = r.decoded_scroll_offset(0., 400., 600.);
        assert!((3000. ..=3000.5).contains(&admitted));
        assert_eq!(r.window_demand_target(), Some(0));
    }

    #[test]
    fn large_gestures_stop_at_decoded_edges_and_request_overlapping_adjacent_rows() {
        let mut r = super::super::tests::tall_reader();
        r.ratios = vec![20.; 2];
        r.viewport_width = 400.;
        r.viewport_height = 600.;
        r.target_width = 400;
        r.anchor = ResumeAnchor::new("page-0", 0, 0.5).unwrap();
        r.retained.insert(0);
        r.decoded_windows.insert(
            0,
            reader_engine::DisplayWindowPlan {
                width: 400,
                full_height: 8000,
                first_row: 3000,
                rows: 3000,
            },
        );
        let stopped = r.decoded_scroll_offset(0., 400., 600.);
        assert!((3000. ..=3000.26).contains(&stopped));
        r.anchor = ResumeAnchor::new("page-0", 0, f64::from(stopped) / 8000.).unwrap();
        let admitted_anchor = r.anchor.clone();
        let dimensions = reader_engine::PageDimensions {
            width: 400,
            height: 8000,
        };
        let (width, (a, b), budget) = r.window_request(0, dimensions).unwrap();
        assert!(a < 3000. / 8000.);
        let reverse = reader_engine::plan_display_window(dimensions, width, a, b, budget).unwrap();
        assert!(reverse.first_row < 3000);
        assert!(reverse.covers(3000. / 8000., 3600. / 8000.));
        r.decoded_windows.insert(0, reverse);
        let next = r.decoded_scroll_offset(0., 400., 600.);
        assert!(next < 3000. && next >= 0.);
        assert_eq!(
            r.anchor, admitted_anchor,
            "decoding/admission alone never replays overshoot"
        );
        r.anchor = ResumeAnchor::new("page-0", 0, 0.5).unwrap();
        r.decoded_windows.insert(
            0,
            reader_engine::DisplayWindowPlan {
                width: 400,
                full_height: 8000,
                first_row: 3000,
                rows: 3000,
            },
        );
        let stopped = r.decoded_scroll_offset(20000., 400., 600.);
        assert!((5399.74..=5400.).contains(&stopped));
        r.anchor = ResumeAnchor::new("page-0", 0, f64::from(stopped) / 8000.).unwrap();
        let (width, (a, b), budget) = r.window_request(0, dimensions).unwrap();
        assert!(b > 6000. / 8000.);
        let forward = reader_engine::plan_display_window(dimensions, width, a, b, budget).unwrap();
        assert!(forward.first_row + forward.rows > 6000);
        assert!(forward.covers(5400. / 8000., 6000. / 8000.));
    }

    #[test]
    fn no_spare_overlap_row_follows_limit_policy_and_keeps_admitted_pixels() {
        let mut r = super::super::tests::tall_reader();
        r.pages.truncate(1);
        r.ratios = vec![2.];
        r.viewport_width = 2048.;
        r.viewport_height = 1024.;
        r.target_width = 4096;
        r.anchor = ResumeAnchor::new("page-0", 0, 0.5).unwrap();
        let dimensions = reader_engine::PageDimensions {
            width: 4096,
            height: 8192,
        };
        let current = reader_engine::DisplayWindowPlan {
            width: 4096,
            full_height: 8192,
            first_row: 4096,
            rows: 2048,
        };
        r.retained.insert(0);
        r.decoded_windows.insert(0, current);
        r.decoded_bytes.insert(0, current.byte_len());
        assert_eq!(current.byte_len(), READER_MODEL_BYTES);
        assert_eq!(r.decoded_scroll_offset(0., 2048., 1024.), 2048.);
        assert!(r.has_visible_pixels(0));
        let (width, (a, b), budget) = r.window_request(0, dimensions).unwrap();
        assert!(reader_engine::plan_display_window(dimensions, width, a, b, budget).is_err());
        assert_eq!(r.anchor.offset(), 0.5);
        assert_eq!(r.decoded_windows[&0], current);
    }

    #[test]
    fn decoded_guard_keeps_chapter_gaps_distinct_from_unknown_loading_edges() {
        let mut r = super::super::tests::tall_reader();
        r.ratios = vec![2.; 2];
        r.viewport_width = 400.;
        r.viewport_height = 600.;
        r.gap_pixels = 96.;
        r.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
        r.retained.insert(0);
        r.decoded_windows.insert(
            0,
            reader_engine::DisplayWindowPlan {
                width: 400,
                full_height: 800,
                first_row: 0,
                rows: 400,
            },
        );
        assert_eq!(r.decoded_scroll_offset(0., 400., 600.), 800.);
        assert_eq!(r.window_demand_target(), Some(0));
        assert_eq!(r.loaded_scroll_offset(0., 400., 600., |i| i == 1), 896.);
        r.anchor = ResumeAnchor::new("page-0", 0, 0.).unwrap();
        assert_eq!(r.loaded_scroll_offset(9000., 400., 600., |i| i == 0), 344.);
    }
}

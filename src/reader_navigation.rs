//! Mihon-compatible tap regions, evaluated in viewport coordinates.
//!
//! Adapted from Mihon v0.21.3, commit
//! feadad5e1e2bf638be6e2ca317d11657b09d4596:
//! https://github.com/mihonapp/mihon/tree/feadad5e1e2bf638be6e2ca317d11657b09d4596/app/src/main/java/eu/kanade/tachiyomi/ui/reader/viewer
//! Sources: navigation/{LNavigation,KindlishNavigation,EdgeNavigation,
//! RightAndLeftNavigation,DisabledNavigation}.kt; ViewerNavigation.kt;
//! pager/{PagerConfig,PagerViewers}.kt; webtoon/WebtoonConfig.kt.
//! Copyright Tachiyomi and Mihon contributors. Licensed under Apache-2.0;
//! see THIRD_PARTY_NOTICES.md and the packaged source/notice catalogue.
//! Nova changes: pure Rust viewport-coordinate lookup, explicit logical actions,
//! finite-input validation/clamping, and coordinate inversion before lookup.
use crate::reader_settings::{TapInversion, TapZones};
use reader_domain::ReadingMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapAction {
    ToggleControls,
    Previous,
    Next,
}

#[derive(Clone, Copy)]
enum RegionAction {
    Previous,
    Next,
    Left,
    Right,
}

#[derive(Clone, Copy)]
struct Region {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    action: RegionAction,
}
impl Region {
    const fn new(left: f32, top: f32, right: f32, bottom: f32, action: RegionAction) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
            action,
        }
    }
    fn contains(self, x: f32, y: f32) -> bool {
        // Android RectF includes its left/top edges and excludes right/bottom.
        self.left <= x && x < self.right && self.top <= y && y < self.bottom
    }
}

// Keep upstream's decimal thresholds: these are 0.33/0.66, not exact thirds.
const L: &[Region] = &[
    Region::new(0., 0.33, 0.33, 0.66, RegionAction::Previous),
    Region::new(0., 0., 1., 0.33, RegionAction::Previous),
    Region::new(0.66, 0.33, 1., 0.66, RegionAction::Next),
    Region::new(0., 0.66, 1., 1., RegionAction::Next),
];
const KINDLE: &[Region] = &[
    Region::new(0.33, 0.33, 1., 1., RegionAction::Next),
    Region::new(0., 0.33, 0.33, 1., RegionAction::Previous),
];
const EDGE: &[Region] = &[
    Region::new(0., 0., 0.33, 1., RegionAction::Next),
    Region::new(0.33, 0.66, 0.66, 1., RegionAction::Previous),
    Region::new(0.66, 0., 1., 1., RegionAction::Next),
];
const RIGHT_LEFT: &[Region] = &[
    Region::new(0., 0., 0.33, 1., RegionAction::Left),
    Region::new(0.66, 0., 1., 1., RegionAction::Right),
];

/// Coordinates are fractions of the entire visible reader viewport, independent
/// of the page/tile under the pointer. Non-finite coordinates open the controls;
/// finite out-of-range coordinates are clamped to 0..=1. After inversion, the
/// exact right/bottom endpoint is outside upstream's half-open regions and opens
/// the controls.
///
/// Only spatial Left/Right regions depend on reading direction. L, Kindle and
/// Edge already express Previous/Next; applying RTL to them would invert twice.
/// Webtoon and vertical paging use L as Default; horizontal pagers use RightLeft.
pub fn tap_action(
    x: f32,
    y: f32,
    scheme: TapZones,
    inversion: TapInversion,
    mode: ReadingMode,
) -> TapAction {
    if !x.is_finite() || !y.is_finite() {
        return TapAction::ToggleControls;
    }
    let mut x = x.clamp(0., 1.);
    let mut y = y.clamp(0., 1.);
    if matches!(inversion, TapInversion::Horizontal | TapInversion::Both) {
        x = 1. - x;
    }
    if matches!(inversion, TapInversion::Vertical | TapInversion::Both) {
        y = 1. - y;
    }
    let scheme = match (scheme, mode) {
        (TapZones::Default, ReadingMode::Webtoon | ReadingMode::Vertical) => TapZones::L,
        (TapZones::Default, _) => TapZones::RightLeft,
        (scheme, _) => scheme,
    };
    let regions = match scheme {
        TapZones::L => L,
        TapZones::Kindle => KINDLE,
        TapZones::Edge => EDGE,
        TapZones::RightLeft => RIGHT_LEFT,
        TapZones::Disabled | TapZones::Default => &[],
    };
    let Some(region) = regions.iter().find(|r| r.contains(x, y)) else {
        return TapAction::ToggleControls;
    };
    match region.action {
        RegionAction::Previous => TapAction::Previous,
        RegionAction::Next => TapAction::Next,
        RegionAction::Left if mode == ReadingMode::RightToLeft => TapAction::Next,
        RegionAction::Right if mode == ReadingMode::RightToLeft => TapAction::Previous,
        RegionAction::Left => TapAction::Previous,
        RegionAction::Right => TapAction::Next,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TapAction::{Next as N, Previous as P, ToggleControls as M};

    fn at(scheme: TapZones, x: f32, y: f32) -> TapAction {
        tap_action(x, y, scheme, TapInversion::None, ReadingMode::LeftToRight)
    }
    #[test]
    fn upstream_region_grids_preserve_logical_and_spatial_actions() {
        for (scheme, grid) in [
            (TapZones::L, [[P, P, P], [P, M, N], [N, N, N]]),
            (TapZones::Kindle, [[M, M, M], [P, N, N], [P, N, N]]),
            // The source rectangles leave top-center as Menu even though its
            // ASCII illustration labels the entire top row Next.
            (TapZones::Edge, [[N, M, N], [N, M, N], [N, P, N]]),
            (TapZones::RightLeft, [[P, M, N], [P, M, N], [P, M, N]]),
            (TapZones::Disabled, [[M, M, M], [M, M, M], [M, M, M]]),
        ] {
            for (row, y) in [0.1, 0.5, 0.9].into_iter().enumerate() {
                for (column, x) in [0.1, 0.5, 0.9].into_iter().enumerate() {
                    assert_eq!(at(scheme, x, y), grid[row][column], "{scheme:?} ({x}, {y})");
                }
            }
        }
    }
    #[test]
    fn l_corners_and_decimal_boundaries_follow_source_rectangles() {
        for (x, y, expected) in [
            (0., 0., P),
            (0.99, 0., P),
            (0., 0.99, N),
            (0.99, 0.99, N),
            (0.329, 0.33, P),
            (0.33, 0.33, M),
            (0.659, 0.33, M),
            (0.66, 0.33, N),
            (0.5, 0.329, P),
            (0.5, 0.33, M),
            (0.5, 0.659, M),
            (0.5, 0.66, N),
            (0.33, 0.66, N),
            (0.66, 0.66, N),
            (1., 0.1, M),
            (0.1, 1., M),
        ] {
            assert_eq!(at(TapZones::L, x, y), expected, "({x}, {y})");
        }
    }
    #[test]
    fn kindle_edge_and_right_left_boundaries_match_upstream() {
        for (scheme, x, y, expected) in [
            (TapZones::Kindle, 0., 0.329, M),
            (TapZones::Kindle, 0., 0.33, P),
            (TapZones::Kindle, 0.329, 0.33, P),
            (TapZones::Kindle, 0.33, 0.33, N),
            (TapZones::Edge, 0.329, 0., N),
            (TapZones::Edge, 0.33, 0., M),
            (TapZones::Edge, 0.659, 0.659, M),
            (TapZones::Edge, 0.66, 0., N),
            (TapZones::Edge, 0.33, 0.66, P),
            (TapZones::Edge, 0.66, 0.66, N),
            (TapZones::RightLeft, 0.329, 0.5, P),
            (TapZones::RightLeft, 0.33, 0.5, M),
            (TapZones::RightLeft, 0.659, 0.5, M),
            (TapZones::RightLeft, 0.66, 0.5, N),
        ] {
            assert_eq!(at(scheme, x, y), expected, "{scheme:?} ({x}, {y})");
        }
    }
    #[test]
    fn defaults_follow_the_viewer_and_only_spatial_regions_reverse_for_rtl() {
        for mode in [ReadingMode::Webtoon, ReadingMode::Vertical] {
            assert_eq!(
                tap_action(0.5, 0.1, TapZones::Default, TapInversion::None, mode),
                P
            );
            assert_eq!(
                tap_action(0.5, 0.9, TapZones::Default, TapInversion::None, mode),
                N
            );
        }
        for (mode, left, right) in [
            (ReadingMode::LeftToRight, P, N),
            (ReadingMode::RightToLeft, N, P),
        ] {
            for scheme in [TapZones::Default, TapZones::RightLeft] {
                assert_eq!(tap_action(0.1, 0.5, scheme, TapInversion::None, mode), left);
                assert_eq!(
                    tap_action(0.9, 0.5, scheme, TapInversion::None, mode),
                    right
                );
                assert_eq!(tap_action(0.5, 0.1, scheme, TapInversion::None, mode), M);
            }
        }
        for scheme in [TapZones::L, TapZones::Kindle, TapZones::Edge] {
            for (x, y) in [(0.1, 0.1), (0.5, 0.5), (0.9, 0.9)] {
                assert_eq!(
                    tap_action(x, y, scheme, TapInversion::None, ReadingMode::RightToLeft),
                    at(scheme, x, y)
                );
            }
        }
    }
    #[test]
    fn inversion_reflects_coordinates_before_lookup_without_double_rtl() {
        for mode in [
            ReadingMode::Webtoon,
            ReadingMode::Vertical,
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
        ] {
            for scheme in [
                TapZones::Default,
                TapZones::L,
                TapZones::Kindle,
                TapZones::Edge,
                TapZones::RightLeft,
                TapZones::Disabled,
            ] {
                for (inversion, x, y) in [
                    (TapInversion::Horizontal, 0.9, 0.2),
                    (TapInversion::Vertical, 0.1, 0.8),
                    (TapInversion::Both, 0.9, 0.8),
                ] {
                    assert_eq!(
                        tap_action(0.1, 0.2, scheme, inversion, mode),
                        tap_action(x, y, scheme, TapInversion::None, mode),
                        "{scheme:?} {mode:?} {inversion:?}"
                    );
                }
            }
        }
        assert_eq!(
            tap_action(
                0.1,
                0.5,
                TapZones::RightLeft,
                TapInversion::Horizontal,
                ReadingMode::RightToLeft
            ),
            P
        );
        assert_eq!(
            tap_action(
                0.5,
                0.1,
                TapZones::L,
                TapInversion::Vertical,
                ReadingMode::RightToLeft
            ),
            N
        );
    }
    #[test]
    fn disabled_and_invalid_inputs_always_leave_controls_accessible() {
        for inversion in [
            TapInversion::None,
            TapInversion::Horizontal,
            TapInversion::Vertical,
            TapInversion::Both,
        ] {
            for (x, y) in [(0., 0.), (0.5, 0.5), (1., 1.), (-100., 100.)] {
                assert_eq!(
                    tap_action(
                        x,
                        y,
                        TapZones::Disabled,
                        inversion,
                        ReadingMode::RightToLeft
                    ),
                    M
                );
            }
        }
        for (x, y) in [
            (f32::NAN, 0.5),
            (0.5, f32::NAN),
            (f32::INFINITY, 0.),
            (0., f32::NEG_INFINITY),
        ] {
            assert_eq!(at(TapZones::L, x, y), M);
        }
        assert_eq!(at(TapZones::RightLeft, -2., 0.5), P);
        assert_eq!(at(TapZones::RightLeft, 2., 0.5), M);
    }
}

# Nova Reader patch to i-slint-core 1.18.0

This directory is the exact crates.io `i-slint-core` 1.18.0 package, with the
four input corrections below and this provenance note added. Its original `LICENSES/`, package
metadata and upstream source attribution are retained.

- Upstream: <https://github.com/slint-ui/slint>, version 1.18.0.
- Registry package: <https://crates.io/crates/i-slint-core/1.18.0>.
- Published archive SHA-256:
  `76b8b730a020446b367e0033e6a1e17719df6b35fd4a65f723e86a7147fbc983`.

`input.rs` now emits each raw two-finger pinch delta as the current distance
scale divided by the previous scale, minus one. `ScaleRotateGestureHandler`
accumulates by multiplying `(1 + delta)`, so additive scale differences drift
and a 2× → 1× movement zeros the accumulator permanently until fingers lift.
The source regression checks actual distances 1× → 1.5× → 2× → 1× → 1.5× → 1×,
then 4× → coincident fingers → 1× → 2×. The effective scale is floored at both
`f32::EPSILON` and `last_scale * f32::EPSILON`, and that same effective scale is
stored for the next event. This keeps the relative factor representable above
zero even after a large contraction, and restores the correct absolute scale
when fingers separate again.

When one finger leaves before pinch recognition, `TwoFingersDown` now emits
`Exit` and clears primary ownership instead of synthesizing a fresh press for
the remaining existing finger. Its moves/release stay suppressed, preventing
an aborted two-finger interaction from becoming a tap, hold or page drag. The
source regression covers both lift orders, cancellation and a fresh contact
after both fingers release. Recognized pinch lifecycle events are unchanged.

`items/flickable/data_ringbuffer.rs` requires at least 8 milliseconds between
the oldest and newest velocity samples before starting a release fling.
Timestamps are millisecond-quantized UI delivery times; queued motion drained
across one clock tick could previously turn four 20 px deltas into an 80,000
px/s fling. Such a short window now yields zero velocity without changing the
drag itself. Eight milliseconds covers approximately two 240 Hz intervals;
a full five-sample history at 240 Hz spans about 16 milliseconds. Ordinary
60/120/240 Hz histories retain their mean velocity and direction. Source tests
cover coincident timestamps, the 1 ms burst, the duration boundary, regular
cadences, slow motion and reversal, compiled by the workspace adapter
`tests/flick_velocity.rs`; `tests/reader_gestures.rs` exercises the
burst and an ordinary fling through the real AppWindow touch path.

This is a minimum observation window, not a maximum-speed policy or a recovery
of physical event timestamps: queued delivery spanning 8 ms or more can still
produce an inaccurate estimate. Platform backends, renderer behavior and
deceleration are unchanged. Remove these patches when the pinned upstream
version includes the corresponding input corrections.

`items/flickable.rs` makes a Flickable that pans along one axis only refuse
a drag that leans clearly towards the other axis
(`should_capture_mouse_direction`): horizontal needs the horizontal distance
past the threshold and at least the vertical one, vertical the reverse. An
exactly diagonal drag, and Flickables that pan both ways (a zoomed reader
page), keep upstream behaviour. Upstream claims on either axis passing the 8 px
threshold, so a sideways flick on a horizontal rail inside a vertical list
(History's "Pick up where you left off", Browse rails and chip rows) was taken
over by the list once the finger drifted 8 px vertically: the page moved and
the rail stopped or skipped. `tests/nested_flick.rs` drags a rail diagonally
inside a list and checks only the rail moves, and that a vertical drag still
scrolls the list.


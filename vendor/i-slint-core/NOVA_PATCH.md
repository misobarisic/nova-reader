# Nova Reader patch to i-slint-core 1.18.0

This directory is the exact crates.io `i-slint-core` 1.18.0 package, with the
two input corrections below and this provenance note added. Its original `LICENSES/`, package
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

No gesture thresholds, platform backends, renderer behavior or other source
files are changed. Remove this patch when the pinned upstream version includes
these input corrections.

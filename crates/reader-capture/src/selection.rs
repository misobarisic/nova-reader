//! Frozen logical artwork coordinates. No display tiles or live reader state
//! participate after selection starts.
use crate::{PageRect, Rect};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug)]
pub struct Selection {
    pub canvas: Rect,
    pub crop: Rect,
    bounds: Rect,
    step: f64,
}
impl Selection {
    pub fn new(viewport: Rect, pages: &[PageRect]) -> Result<Self, String> {
        if !viewport.valid() || pages.is_empty() || pages.iter().any(|p| !p.rect.valid()) {
            return Err("Capture selection geometry is unavailable".into());
        }
        let top = pages.iter().map(|p| p.rect.y).fold(f64::INFINITY, f64::min);
        let bottom = pages
            .iter()
            .map(|p| p.rect.bottom())
            .fold(f64::NEG_INFINITY, f64::max);
        let bounds = Rect {
            y: top,
            height: bottom - top,
            ..viewport
        };
        let canvas = viewport
            .intersection(bounds)
            .ok_or("No artwork in capture selection")?;
        Ok(Self {
            canvas,
            crop: canvas,
            bounds,
            step: viewport.height,
        })
    }
    /// Replace provisional chapter bounds after an incoming source is resolved.
    /// Already selected known artwork keeps its exact logical coordinates.
    pub fn reflow_bounds(&mut self, pages: &[PageRect]) -> Result<(), String> {
        let updated = Self::new(self.canvas, pages)?;
        self.bounds = updated.bounds;
        self.canvas = self
            .canvas
            .intersection(self.bounds)
            .ok_or("No artwork in capture selection")?;
        self.crop = self
            .crop
            .intersection(self.canvas)
            .ok_or("No artwork in capture selection")?;
        Ok(())
    }
    /// Retain source positions on artwork already known when an intervening
    /// placeholder acquires its natural height. Unknown endpoints remain a
    /// logical viewport distance from the frozen loaded anchor.
    pub fn reanchor_known(
        &mut self,
        previous: &[PageRect],
        current: &[PageRect],
        known: impl Fn(usize) -> bool,
    ) -> Result<(), String> {
        let map = |y: f64| {
            previous
                .iter()
                .find(|page| known(page.index) && y >= page.rect.y && y <= page.rect.bottom())
                .and_then(|page| {
                    current
                        .iter()
                        .find(|next| next.index == page.index)
                        .map(|next| {
                            next.rect.y + (y - page.rect.y) / page.rect.height * next.rect.height
                        })
                })
                .unwrap_or(y)
        };
        for rect in [&mut self.canvas, &mut self.crop] {
            let top = map(rect.y);
            let bottom = map(rect.bottom());
            if bottom <= top {
                return Err("Capture selection anchors are unavailable".into());
            }
            rect.y = top;
            rect.height = bottom - top;
        }
        self.reflow_bounds(current)
    }
    pub fn can_extend(&self, above: bool) -> bool {
        if above {
            self.canvas.y > self.bounds.y + 0.01
        } else {
            self.canvas.bottom() < self.bounds.bottom() - 0.01
        }
    }
    pub fn extend(&mut self, above: bool) -> bool {
        if !self.can_extend(above) {
            return false;
        }
        if above {
            let top = (self.canvas.y - self.step).max(self.bounds.y);
            let delta = self.canvas.y - top;
            self.canvas.y = top;
            self.canvas.height += delta;
            self.crop.height += self.crop.y - top;
            self.crop.y = top;
        } else {
            let bottom = (self.canvas.bottom() + self.step).min(self.bounds.bottom());
            self.canvas.height = bottom - self.canvas.y;
            self.crop.height = bottom - self.crop.y;
        }
        true
    }
    pub fn normalized(&self) -> Rect {
        Rect {
            x: (self.crop.x - self.canvas.x) / self.canvas.width,
            y: (self.crop.y - self.canvas.y) / self.canvas.height,
            width: self.crop.width / self.canvas.width,
            height: self.crop.height / self.canvas.height,
        }
    }
    pub fn set_normalized(&mut self, value: Rect) -> bool {
        if !value.valid() {
            return false;
        }
        let x = value.x.clamp(0., 0.999);
        let y = value.y.clamp(0., 0.999);
        let right = value.right().clamp(x + 0.001, 1.);
        let bottom = value.bottom().clamp(y + 0.001, 1.);
        self.crop = Rect {
            x: self.canvas.x + x * self.canvas.width,
            y: self.canvas.y + y * self.canvas.height,
            width: (right - x) * self.canvas.width,
            height: (bottom - y) * self.canvas.height,
        };
        true
    }
    pub fn first_page(&self, pages: &[PageRect], cropped: bool) -> Option<usize> {
        let rect = if cropped { self.crop } else { self.canvas };
        pages
            .iter()
            .find(|p| p.rect.intersection(rect).is_some())
            .map(|p| p.index)
    }
}

/// A request ticket is invalidated by any newer preview, adjustment, cancel or
/// reader change. Admission checks and final publication use the same ticket.
#[derive(Clone, Default)]
pub struct Revision(Arc<AtomicU64>);
impl Revision {
    pub fn advance(&self) -> Ticket {
        let value = self.0.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        Ticket {
            revision: self.0.clone(),
            value,
        }
    }
}
#[derive(Clone)]
pub struct Ticket {
    revision: Arc<AtomicU64>,
    value: u64,
}
impl Ticket {
    pub fn current(&self) -> bool {
        self.revision.load(Ordering::Acquire) == self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(y: f64, h: f64) -> Rect {
        Rect {
            x: 10.,
            y,
            width: 100.,
            height: h,
        }
    }
    #[test]
    fn independent_ends_cross_pages_and_preserve_horizontal_crop() {
        let pages = [
            PageRect {
                index: 0,
                rect: rect(-200., 300.),
            },
            PageRect {
                index: 1,
                rect: rect(100., 500.),
            },
        ];
        let mut selection = Selection::new(rect(0., 100.), &pages).unwrap();
        selection.set_normalized(Rect {
            x: 0.2,
            y: 0.1,
            width: 0.6,
            height: 0.8,
        });
        assert!(selection.extend(true));
        assert!((selection.crop.width - 60.).abs() < 1e-8);
        assert_eq!(
            (selection.crop.x, selection.crop.y, selection.crop.height),
            (30., -100., 190.)
        );
        assert!(selection.extend(false));
        assert_eq!(selection.crop.bottom(), 200.);
        assert_eq!(selection.first_page(&pages, true), Some(0));
        while selection.extend(true) {}
        while selection.extend(false) {}
        assert_eq!(selection.canvas, rect(-200., 800.));
        assert_eq!(selection.crop.x, 30.);
    }
    #[test]
    fn tall_single_page_keeps_source_anchor_and_rejects_nonfinite_drags() {
        let mut s = Selection::new(
            rect(1234., 600.),
            &[PageRect {
                index: 9,
                rect: rect(0., 25_000.),
            }],
        )
        .unwrap();
        s.extend(true);
        s.extend(false);
        assert_eq!(s.canvas, rect(634., 1800.));
        let old = s.crop;
        assert!(!s.set_normalized(Rect {
            x: f64::NAN,
            y: 0.,
            width: 1.,
            height: 1.
        }));
        assert_eq!(s.crop, old);
    }
    #[test]
    fn resolving_intervening_placeholder_preserves_known_source_endpoints() {
        let previous = [
            PageRect {
                index: 0,
                rect: rect(0., 100.),
            },
            PageRect {
                index: 1,
                rect: rect(100., 50.),
            },
            PageRect {
                index: 2,
                rect: rect(150., 100.),
            },
        ];
        let current = [
            PageRect {
                index: 0,
                rect: rect(-250., 100.),
            },
            PageRect {
                index: 1,
                rect: rect(-150., 300.),
            },
            PageRect {
                index: 2,
                rect: rect(150., 100.),
            },
        ];
        let mut selection = Selection::new(rect(50., 150.), &previous).unwrap();
        selection
            .reanchor_known(&previous, &current, |index| index != 1)
            .unwrap();
        assert_eq!(selection.canvas, rect(-200., 400.));
        assert_eq!(selection.crop, selection.canvas);
    }

    #[test]
    fn old_preview_and_export_tickets_cannot_publish_after_cancel_or_new_request() {
        let revisions = Revision::default();
        let old = revisions.advance();
        let latest = revisions.advance();
        assert!(!old.current());
        assert!(latest.current());
        revisions.advance();
        assert!(!latest.current());
    }
}

//! The picture fitted in its pane, zoomed and panned. It behaves as `rune_kit`'s image stage:
//! a click zooms to 100% about the pointer and another fits it; Ctrl or ⌘ with the wheel (a
//! pinch) zooms about the pointer; a zoomed picture pans with a drag or the wheel. Its controls
//! live in the toolbar rather than over the picture, and the toolbar shows the zoom level, which
//! the kit's stage keeps to itself.

use std::cell::Cell;

use rune::{Cursor, Drag, El, Phase, Tx, el};

use crate::state::State;

/// How far a press moves before it pans rather than clicks.
const DRAG_THRESHOLD: f32 = 3.0;
/// Room left around a fitted picture.
const INSET: f32 = 24.0;
pub const STEP: f32 = 1.25;
pub const KEY: &str = "picture-stage";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Gesture {
    #[default]
    Idle,
    Panning {
        moved: bool,
        at: [f32; 2],
    },
}

#[derive(Clone, Copy, Debug)]
struct Geom {
    /// How far the fit shrinks the picture.
    fit: f32,
    size: [f32; 2],
    natural: [f32; 2],
}

/// One picture's zoom: a `scale` of 1 is the fit, `pan` points from the middle.
#[derive(Debug)]
pub struct Zoom {
    scale: f32,
    pan: [f32; 2],
    gesture: Gesture,
    geom: Cell<Geom>,
}

impl Default for Zoom {
    fn default() -> Zoom {
        Zoom { scale: 1.0, pan: [0.0; 2], gesture: Gesture::Idle, geom: Cell::new(Geom { fit: 1.0, size: [0.0; 2], natural: [1.0; 2] }) }
    }
}

impl Zoom {
    /// Fits a picture of `natural` pixels in a stage of `size` points, never growing it.
    pub fn measure(&self, natural: [f32; 2], size: [f32; 2]) {
        let [nw, nh] = natural.map(|v| v.max(1.0));
        let fit = 1f32.min((size[0] - 2.0 * INSET).max(1.0) / nw).min((size[1] - 2.0 * INSET).max(1.0) / nh);
        self.geom.set(Geom { fit, size, natural: [nw, nh] });
    }

    /// The zoom as a percentage of the picture's pixels.
    pub fn level(&self) -> u32 {
        (self.scale * self.geom.get().fit * 100.0).round() as u32
    }

    fn zoomed(&self) -> bool {
        self.scale > 1.0
    }

    fn min_zoom(&self) -> f32 {
        0.25f32.min(1.0 / self.geom.get().fit)
    }

    fn max_zoom(&self) -> f32 {
        8.0f32.max(1.0 / self.geom.get().fit)
    }

    pub fn can_zoom_in(&self) -> bool {
        self.scale < self.max_zoom()
    }

    pub fn can_zoom_out(&self) -> bool {
        self.scale > self.min_zoom()
    }

    pub fn is_fit(&self) -> bool {
        (self.scale - 1.0).abs() < f32::EPSILON
    }

    pub fn is_actual(&self) -> bool {
        self.level() == 100
    }

    fn bound(&self, offset: f32, natural: f32) -> f32 {
        let limit = (natural * self.geom.get().fit * (self.scale - 1.0) / 2.0).max(0.0);
        offset.clamp(-limit, limit)
    }

    fn set_pan(&mut self, pan: [f32; 2]) {
        let n = self.geom.get().natural;
        self.pan = [self.bound(pan[0], n[0]), self.bound(pan[1], n[1])];
    }

    fn set_zoom(&mut self, scale: f32, pan: [f32; 2], tx: &mut Tx) {
        self.scale = scale;
        self.set_pan(pan);
        if !self.zoomed() && matches!(self.gesture, Gesture::Panning { .. }) {
            self.gesture = Gesture::Idle;
        }
        tx.instant();
    }

    /// Zooms to `zoom` about `at`, points from the stage's top-left.
    fn zoom_at(&mut self, zoom: f32, at: [f32; 2], tx: &mut Tx) {
        let next = zoom.clamp(self.min_zoom(), self.max_zoom());
        let ratio = next / self.scale;
        let size = self.geom.get().size;
        let a = [at[0] - size[0] / 2.0, at[1] - size[1] / 2.0];
        self.set_zoom(next, [a[0] - (a[0] - self.pan[0]) * ratio, a[1] - (a[1] - self.pan[1]) * ratio], tx);
    }

    fn middle(&self) -> [f32; 2] {
        let size = self.geom.get().size;
        [size[0] / 2.0, size[1] / 2.0]
    }

    pub fn zoom_by(&mut self, factor: f32, tx: &mut Tx) {
        self.zoom_at(self.scale * factor, self.middle(), tx);
    }

    pub fn fit(&mut self, tx: &mut Tx) {
        self.set_zoom(1.0, [0.0; 2], tx);
    }

    /// One picture pixel to a point, about the middle.
    pub fn actual(&mut self, tx: &mut Tx) {
        self.zoom_at(1.0 / self.geom.get().fit, self.middle(), tx);
    }

    /// The picture's rect in the stage, `[x, y, w, h]`.
    fn rect(&self) -> [f32; 4] {
        let g = self.geom.get();
        let (w, h) = (g.natural[0] * g.fit * self.scale, g.natural[1] * g.fit * self.scale);
        let pan = [self.bound(self.pan[0], g.natural[0]), self.bound(self.pan[1], g.natural[1])];
        [g.size[0] / 2.0 + pan[0] - w / 2.0, g.size[1] / 2.0 + pan[1] - h / 2.0, w, h]
    }

    fn drag(&mut self, d: &Drag, tx: &mut Tx) {
        let [x, y, _, _] = self.rect();
        let at = [x + d.x, y + d.y];
        match (d.phase, self.gesture) {
            (Phase::Down, _) => self.gesture = if self.zoomed() { Gesture::Panning { moved: false, at } } else { Gesture::Idle },
            (Phase::Move, Gesture::Panning { moved, at: from }) => {
                let delta = [at[0] - from[0], at[1] - from[1]];
                if delta == [0.0, 0.0] || (!moved && delta[0].hypot(delta[1]) < DRAG_THRESHOLD) {
                    return;
                }
                self.gesture = Gesture::Panning { moved: true, at };
                self.set_pan([self.pan[0] + delta[0], self.pan[1] + delta[1]]);
                tx.instant();
            }
            (Phase::Up, _) if d.cancelled() => self.gesture = Gesture::Idle,
            // The click that ends a pan neither fits nor zooms.
            (Phase::Up, Gesture::Panning { moved: true, .. }) => self.gesture = Gesture::Idle,
            (Phase::Up, _) => {
                self.gesture = Gesture::Idle;
                if self.is_fit() { self.zoom_at(1.0 / self.geom.get().fit, at, tx) } else { self.fit(tx) }
            }
            _ => {}
        }
    }
}

/// The stage: `image` fitted in the pane, zoomed and panned as `zoom` says. Measure `zoom`
/// first.
pub fn stage(zoom: &Zoom, image: El, k: &rune_kit::Kit) -> El {
    let [x, y, w, h] = zoom.rect();
    let cursor = match (zoom.zoomed(), zoom.gesture) {
        (true, Gesture::Panning { .. }) => Cursor::Grabbing,
        (true, _) => Cursor::Grab,
        _ => Cursor::Default,
    };
    let image = image.absolute(x, y).size(w, h).cursor(cursor).on_drag(|s: &mut State, d: &mut Drag, tx: &mut Tx| s.zoom.drag(d, tx));
    let mut stage = el()
        .key(KEY)
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .fill(k.c.layer_01)
        .clip_soft(0.0)
        .on_wheel(|s: &mut State, e: rune::WheelEvent, tx: &mut Tx| {
            let z = &mut s.zoom;
            let pinch = e.mods.ctrl || e.mods.cmd;
            if !pinch && !z.zoomed() {
                return false;
            }
            if pinch {
                // A browser's delta runs the other way.
                z.zoom_at(z.scale * (e.dy / 100.0).exp(), [e.x, e.y], tx);
            } else {
                z.set_pan([z.pan[0] + e.dx, z.pan[1] + e.dy]);
                tx.instant();
            }
            true
        })
        .child(image);
    if cfg!(target_os = "macos") {
        stage = stage.on_magnify(|s: &mut State, e: rune::MagnifyEvent, tx: &mut Tx| {
            let z = &mut s.zoom;
            z.zoom_at(z.scale * (1.0 + e.delta).max(0.01), [e.x, e.y], tx);
            true
        });
    }
    stage
}

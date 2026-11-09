//! Zoom and pan state of the displayed image, including the "fit to window"
//! computation and cursor-anchored zooming.

use egui::{Pos2, Rect, Vec2};

/// Smallest zoom relative to physical pixels (1 %).
pub const MIN_SCALE: f32 = 0.01;
/// Largest zoom relative to physical pixels (3200 %).
pub const MAX_SCALE: f32 = 32.0;
/// Zoom factor applied per wheel notch or `+`/`-` key press.
pub const ZOOM_STEP: f32 = 1.25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomMode {
    /// Scale the image so it fills the window while keeping its aspect ratio.
    /// Re-evaluated every frame, so it follows window resizes automatically.
    Fit,
    /// One image pixel equals one physical screen pixel.
    Actual,
    /// A zoom level chosen by the user with the wheel or keys.
    Custom,
}

#[derive(Clone, Debug)]
pub struct Viewport {
    pub mode: ZoomMode,
    /// Logical points per image pixel.
    pub scale: f32,
    /// Offset of the image centre from the view centre, in points.
    pub pan: Vec2,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            mode: ZoomMode::Fit,
            scale: 1.0,
            pan: Vec2::ZERO,
        }
    }

    /// Back to "fit to window", centred.
    pub fn reset_fit(&mut self) {
        self.mode = ZoomMode::Fit;
        self.pan = Vec2::ZERO;
    }

    /// 100 % (physical pixels), centred.
    pub fn set_actual(&mut self) {
        self.mode = ZoomMode::Actual;
        self.pan = Vec2::ZERO;
    }

    /// `min(W / w, H / h)`: the largest scale at which the whole image fits.
    pub fn fit_scale(view: Rect, image_size: Vec2) -> f32 {
        if image_size.x <= 0.0 || image_size.y <= 0.0 {
            return 1.0;
        }
        (view.width() / image_size.x).min(view.height() / image_size.y)
    }

    /// Updates the scale for the automatic modes, clamps the pan and returns
    /// the rectangle (in points) where the image should be drawn.
    pub fn layout(&mut self, view: Rect, image_size: Vec2, pixels_per_point: f32) -> Rect {
        match self.mode {
            ZoomMode::Fit => {
                self.scale = Self::fit_scale(view, image_size);
                self.pan = Vec2::ZERO;
            }
            ZoomMode::Actual => {
                self.scale = 1.0 / pixels_per_point;
            }
            ZoomMode::Custom => {}
        }
        self.clamp_pan(view, image_size);
        Rect::from_center_size(view.center() + self.pan, image_size * self.scale)
    }

    /// Keeps the image centred on any axis where it is smaller than the view,
    /// and prevents empty gaps on any axis where it is larger.
    fn clamp_pan(&mut self, view: Rect, image_size: Vec2) {
        let drawn = image_size * self.scale;
        let max_x = ((drawn.x - view.width()) / 2.0).max(0.0);
        let max_y = ((drawn.y - view.height()) / 2.0).max(0.0);
        self.pan.x = self.pan.x.clamp(-max_x, max_x);
        self.pan.y = self.pan.y.clamp(-max_y, max_y);
    }

    /// Multiplies the zoom by `factor`, keeping the image point under `anchor`
    /// (or the view centre when `None`) fixed on screen.
    /// Call [`Self::layout`] first so `scale` reflects the current mode.
    pub fn zoom_by(
        &mut self,
        factor: f32,
        anchor: Option<Pos2>,
        view: Rect,
        image_size: Vec2,
        pixels_per_point: f32,
    ) {
        let min = MIN_SCALE / pixels_per_point;
        let max = MAX_SCALE / pixels_per_point;
        let old = self.scale;
        let new = (old * factor).clamp(min, max);
        if (new - old).abs() <= f32::EPSILON || !new.is_finite() {
            return;
        }

        let anchor = anchor.filter(|p| view.contains(*p)).unwrap_or_else(|| view.center());
        let centre = view.center() + self.pan;
        // offset1 = m - (m - offset0) * (s1 / s0)
        let new_centre = anchor - (anchor - centre) * (new / old);
        self.pan = new_centre - view.center();
        self.scale = new;
        self.mode = ZoomMode::Custom;
        self.clamp_pan(view, image_size);
    }

    /// Drags the image by `delta` points.
    pub fn pan_by(&mut self, delta: Vec2, view: Rect, image_size: Vec2) {
        self.pan += delta;
        self.clamp_pan(view, image_size);
    }

    /// Zoom as a percentage relative to physical pixels (100 % = 1:1).
    pub fn percent(&self, pixels_per_point: f32) -> f32 {
        self.scale * pixels_per_point * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn view() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn fit_landscape_and_portrait() {
        let mut vp = Viewport::new();
        let r = vp.layout(view(), Vec2::new(1600.0, 800.0), 1.0);
        assert!(approx(vp.scale, 0.5));
        assert!(approx(r.width(), 800.0) && approx(r.height(), 400.0));
        assert!(approx(r.min.y, 100.0)); // centred vertically

        let r = vp.layout(view(), Vec2::new(400.0, 1200.0), 1.0);
        assert!(approx(vp.scale, 0.5));
        assert!(approx(r.width(), 200.0) && approx(r.height(), 600.0));
        assert!(approx(r.min.x, 300.0)); // centred horizontally
    }

    #[test]
    fn fit_upscales_small_images() {
        let mut vp = Viewport::new();
        vp.layout(view(), Vec2::new(80.0, 60.0), 1.0);
        assert!(approx(vp.scale, 10.0));
    }

    #[test]
    fn actual_size_uses_physical_pixels() {
        let mut vp = Viewport::new();
        vp.set_actual();
        vp.layout(view(), Vec2::new(100.0, 100.0), 1.5);
        assert!(approx(vp.scale, 1.0 / 1.5));
        assert!(approx(vp.percent(1.5), 100.0));
    }

    #[test]
    fn zoom_keeps_point_under_cursor() {
        let mut vp = Viewport::new();
        let image = Vec2::new(4000.0, 3000.0);
        vp.layout(view(), image, 1.0);
        let cursor = pos2(200.0, 150.0);

        let before = vp.layout(view(), image, 1.0);
        let image_pt = (cursor - before.min) / vp.scale;

        vp.zoom_by(2.0, Some(cursor), view(), image, 1.0);
        assert_eq!(vp.mode, ZoomMode::Custom);
        let after = vp.layout(view(), image, 1.0);
        let image_pt_after = (cursor - after.min) / vp.scale;

        assert!(approx(image_pt.x, image_pt_after.x));
        assert!(approx(image_pt.y, image_pt_after.y));
    }

    #[test]
    fn zoom_is_clamped() {
        let mut vp = Viewport::new();
        let image = Vec2::new(100.0, 100.0);
        vp.layout(view(), image, 1.0);
        vp.zoom_by(1e9, None, view(), image, 1.0);
        assert!(approx(vp.scale, MAX_SCALE));
        vp.zoom_by(1e-9, None, view(), image, 1.0);
        assert!(approx(vp.scale, MIN_SCALE));
    }

    #[test]
    fn pan_is_clamped_to_image_bounds() {
        let mut vp = Viewport::new();
        let image = Vec2::new(1600.0, 1200.0);
        vp.set_actual();
        vp.layout(view(), image, 1.0);
        vp.pan_by(Vec2::new(10_000.0, -10_000.0), view(), image);
        assert!(approx(vp.pan.x, 400.0));
        assert!(approx(vp.pan.y, -300.0));

        // Smaller than the view: always centred.
        vp.reset_fit();
        vp.layout(view(), image, 1.0);
        vp.pan_by(Vec2::new(50.0, 50.0), view(), image);
        assert!(approx(vp.pan.x, 0.0) && approx(vp.pan.y, 0.0));
    }
}

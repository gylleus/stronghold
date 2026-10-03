//! Zooming and panning the farm. The farm is laid out and painted to fit its rect as ever;
//! the [`Camera`] then maps those shapes onto the screen, and screen points (the pointer)
//! back into the farm for hit testing.
//!
//! A mouse wheel, a trackpad pinch and ⌘+scroll zoom around the pointer; a trackpad swipe,
//! right- or middle-drag and the arrow keys pan (see [`crate::ui::graph::nav`]); a double-click
//! on empty ground fits the farm again.

use eframe::egui::{self, Pos2, Rect, Shape, Vec2, emath::TSTransform, layers::ShapeIdx, vec2};

/// The fitted farm; it fills the view, so there is nothing to zoom out to.
pub const MIN_ZOOM: f32 = 1.0;
/// Close enough for a single ant's tag to fill a good part of the view.
pub const MAX_ZOOM: f32 = 8.0;

/// What part of the farm is on screen: `zoom` times the fitted size, with the point
/// `center` (a share of the farm's rect per axis) in the middle of the view. Kept relative to
/// the rect so it survives the view resizing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub zoom: f32,
    pub center: Vec2,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            zoom: MIN_ZOOM,
            center: vec2(0.5, 0.5),
        }
    }
}

impl Camera {
    /// Whether the whole farm is shown.
    pub fn is_fit(&self) -> bool {
        self.zoom <= MIN_ZOOM
    }

    /// Maps the farm laid out in `rect` to the screen.
    pub fn transform(&self, rect: Rect) -> TSTransform {
        let focus = rect.min + self.center * rect.size();
        TSTransform::new(
            rect.center().to_vec2() - focus.to_vec2() * self.zoom,
            self.zoom,
        )
    }

    /// The point of the farm laid out in `rect` drawn at `screen`.
    pub fn to_farm(&self, rect: Rect, screen: Pos2) -> Pos2 {
        self.transform(rect).inverse() * screen
    }

    /// Multiplies the zoom by `factor`, keeping the farm point under `anchor` (a screen point)
    /// where it is.
    pub fn zoom_at(&mut self, rect: Rect, anchor: Pos2, factor: f32) {
        let fixed = self.to_farm(rect, anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let focus = fixed - (anchor - rect.center()) / self.zoom;
        self.center = (focus - rect.min) / rect.size();
        self.clamp();
    }

    /// Moves the farm by `delta` screen points.
    pub fn pan(&mut self, rect: Rect, delta: Vec2) {
        self.center -= delta / (self.zoom * rect.size());
        self.clamp();
    }

    /// Keeps the view inside the farm.
    fn clamp(&mut self) {
        let half = 0.5 / self.zoom;
        self.center = vec2(
            self.center.x.clamp(half, 1.0 - half),
            self.center.y.clamp(half, 1.0 - half),
        );
        if !self.center.is_finite() {
            *self = Self::default();
        }
    }
}

/// Moves the shapes of `layer` from `start` on, painted for the fitted farm, to where
/// `transform` puts them, clipped to `clip`. Text is laid out again at the zoomed size rather
/// than stretched, so it stays sharp.
pub fn apply(
    ctx: &egui::Context,
    layer: egui::LayerId,
    start: ShapeIdx,
    transform: TSTransform,
    clip: Rect,
) {
    if transform == TSTransform::IDENTITY {
        return;
    }
    let shapes: Vec<Shape> = ctx.graphics_mut(|g| {
        let list = g.entry(layer);
        (start.0..list.next_idx().0)
            .map(|i| {
                let mut shape = Shape::Noop;
                list.mutate_shape(ShapeIdx(i), |s| {
                    shape = std::mem::replace(&mut s.shape, Shape::Noop);
                });
                shape
            })
            .collect()
    });
    let shapes: Vec<Shape> = ctx.fonts_mut(|fonts| {
        shapes
            .into_iter()
            .map(|shape| zoomed(shape, transform, fonts))
            .collect()
    });
    ctx.graphics_mut(|g| {
        let list = g.entry(layer);
        for (i, shape) in shapes.into_iter().enumerate() {
            list.set(ShapeIdx(start.0 + i), clip, shape);
        }
    });
}

/// `shape` moved by `transform`, with its text laid out again at the new size.
fn zoomed(shape: Shape, transform: TSTransform, fonts: &mut egui::epaint::FontsView<'_>) -> Shape {
    match shape {
        Shape::Vec(shapes) => Shape::Vec(
            shapes
                .into_iter()
                .map(|s| zoomed(s, transform, fonts))
                .collect(),
        ),
        Shape::Text(mut text) => {
            let job = scaled_job(&text.galley.job, transform.scaling);
            text.galley = fonts.layout_job(job);
            text.pos = transform * text.pos;
            text.underline.width *= transform.scaling;
            Shape::Text(text)
        }
        mut other => {
            other.transform(transform);
            other
        }
    }
}

/// `job` with every size in it multiplied by `scale`.
fn scaled_job(job: &egui::text::LayoutJob, scale: f32) -> egui::text::LayoutJob {
    let mut job = job.clone();
    job.wrap.max_width *= scale;
    job.first_row_min_height *= scale;
    for section in &mut job.sections {
        section.leading_space *= scale;
        let format = &mut section.format;
        format.font_id.size *= scale;
        format.extra_letter_spacing *= scale;
        format.line_height = format.line_height.map(|h| h * scale);
        format.expand_bg *= scale;
        format.underline.width *= scale;
        format.strikethrough.width *= scale;
    }
    job
}

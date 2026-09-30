//! Scroll and keyboard navigation: a trackpad swipe pans, a mouse wheel zooms, arrows pan.

use eframe::egui::{self, Event, Key, MouseWheelUnit, Vec2};

/// Zoom factor per scrolled point.
const SCROLL_ZOOM_SPEED: f32 = 0.0015;
/// Arrow-key pan speed, in points per second.
const KEY_PAN_SPEED: f32 = 700.0;

/// The device the latest scroll came from.
///
/// On macOS, winit reports a classic mouse wheel in lines and a trackpad (or Magic Mouse) in
/// points, with touch phases; egui passes that on as [`MouseWheelUnit`]. The source is kept
/// across frames because egui smooths a wheel notch out over several of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScrollSource {
    #[default]
    Trackpad,
    Wheel,
}

impl ScrollSource {
    /// The source of the last scroll event in `events`, if any.
    pub fn from_events(events: &[Event]) -> Option<Self> {
        events.iter().rev().find_map(|event| match event {
            Event::MouseWheel { unit, .. } => Some(match unit {
                MouseWheelUnit::Point => Self::Trackpad,
                MouseWheelUnit::Line | MouseWheelUnit::Page => Self::Wheel,
            }),
            _ => None,
        })
    }
}

/// What a scroll does to the view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollAction {
    /// Move the content by this many points.
    Pan(Vec2),
    /// Multiply the zoom by this factor around the pointer.
    Zoom(f32),
}

/// Routes this frame's (smoothed) scroll `delta`: a trackpad swipe pans in both axes, a wheel
/// zooms (a horizontal wheel or shift+wheel pans sideways), and ctrl held zooms either one.
/// ⌘+scroll never gets here: egui turns it into [`egui::InputState::zoom_delta`].
pub fn route_scroll(
    source: ScrollSource,
    delta: Vec2,
    zoom_modifier: bool,
) -> Option<ScrollAction> {
    if delta == Vec2::ZERO {
        return None;
    }
    let zoom = |amount: f32| ScrollAction::Zoom((amount * SCROLL_ZOOM_SPEED).exp());
    Some(match source {
        _ if zoom_modifier => zoom(delta.x + delta.y),
        ScrollSource::Wheel if delta.y != 0.0 => zoom(delta.y),
        ScrollSource::Wheel => ScrollAction::Pan(Vec2::new(delta.x, 0.0)),
        ScrollSource::Trackpad => ScrollAction::Pan(delta),
    })
}

/// How far the held arrow keys move the content this frame, or `None` when none is held.
/// Plain arrows only, and not while a focused widget (a text field moves its cursor with them)
/// or a modal (the command palette) has them.
pub fn arrow_pan(ctx: &egui::Context) -> Option<Vec2> {
    if ctx.egui_wants_keyboard_input() || ctx.memory(|m| m.top_modal_layer().is_some()) {
        return None;
    }
    let pan = ctx.input(|i| {
        if !i.modifiers.is_none() {
            return Vec2::ZERO;
        }
        let axis = |minus: Key, plus: Key| {
            f32::from(u8::from(i.key_down(plus))) - f32::from(u8::from(i.key_down(minus)))
        };
        // Content moves opposite to the arrow, so the view travels the arrow's way.
        -Vec2::new(
            axis(Key::ArrowLeft, Key::ArrowRight),
            axis(Key::ArrowUp, Key::ArrowDown),
        ) * KEY_PAN_SPEED
            * i.stable_dt.min(0.1)
    });
    (pan != Vec2::ZERO).then_some(pan)
}

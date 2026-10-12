//! Fleet state colours and release opacity.
use crate::model::{NodeState, RELEASED_FADE_MS};
use eframe::egui::Color32;
pub const ALL_STATES: [NodeState; 6] = [
    NodeState::Working,
    NodeState::Idle,
    NodeState::NeedsInput,
    NodeState::Stalled,
    NodeState::Done,
    NodeState::Failed,
];

const FADE_START_ALPHA: f32 = 0.6;
const FADE_END_ALPHA: f32 = 0.1;
pub fn envoy_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(105, 208, 215)
    } else {
        Color32::from_rgb(20, 109, 121)
    }
}

pub fn queen_color(dark_mode: bool) -> Color32 {
    if dark_mode {
        Color32::from_rgb(0xd9, 0xc3, 0x8c)
    } else {
        Color32::from_rgb(0x8c, 0x7a, 0x2e)
    }
}

pub fn state_color(state: NodeState, dark_mode: bool) -> Color32 {
    let (dark, light) = match state {
        NodeState::Working => (0x38bdf8, 0x0284c7),
        NodeState::Idle => (0x8b929c, 0x6b7280),
        NodeState::NeedsInput => (0xfbbf24, 0xb86e00),
        NodeState::Stalled => (0xfb7a3c, 0xd4480f),
        NodeState::Done => (0x5fae7a, 0x2f8552),
        NodeState::Failed => (0xf04c4c, 0xc81e1e),
    };
    let rgb: u32 = if dark_mode { dark } else { light };
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub fn pr_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0xa3, 0x71, 0xf7)
    } else {
        Color32::from_rgb(0x82, 0x50, 0xdf)
    }
}

pub fn fade_alpha(released_at_ms: Option<i64>, now_ms: i64) -> f32 {
    let Some(at) = released_at_ms else {
        return 1.0;
    };
    let t = ((now_ms - at) as f32 / RELEASED_FADE_MS as f32).clamp(0.0, 1.0);
    FADE_START_ALPHA + (FADE_END_ALPHA - FADE_START_ALPHA) * t
}

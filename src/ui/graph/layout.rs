//! Force-directed layout: Fruchterman-Reingold plus extra forces, paused once it settles.

use egui_graphs::{
    CenterGravity, CenterGravityParams, Extra, FruchtermanReingoldState,
    FruchtermanReingoldWithExtras, FruchtermanReingoldWithExtrasState, LayoutForceDirected,
};

/// Forces applied on top of FR repulsion/attraction, as a cons list `(Extra<F, on>, rest)`.
/// Further forces (e.g. run clusters) are added by extending this tuple.
pub type Extras = (Extra<CenterGravity, true>, ());
pub type FleetLayoutState = FruchtermanReingoldWithExtrasState<Extras>;
pub type FleetLayout = LayoutForceDirected<FruchtermanReingoldWithExtras<Extras>>;

/// Average per-node step (canvas units) below which a frame counts as calm.
pub const SETTLE_EPSILON: f32 = 0.2;
/// Consecutive calm frames before the simulation pauses.
pub const SETTLE_FRAMES: u32 = 30;

pub fn initial_state() -> FleetLayoutState {
    FleetLayoutState {
        base: FruchtermanReingoldState {
            k_scale: 0.5,
            ..FruchtermanReingoldState::default()
        },
        extras: (Extra::new(CenterGravityParams { c: 0.3 }), ()),
    }
}

/// Counts calm frames and says when the simulation has settled.
#[derive(Debug, Default, Clone)]
pub struct SettleTracker {
    calm_frames: u32,
}

impl SettleTracker {
    pub fn reset(&mut self) {
        self.calm_frames = 0;
    }

    /// Feeds one step's average displacement; `true` once the layout should pause.
    pub fn observe(&mut self, avg_displacement: Option<f32>) -> bool {
        match avg_displacement {
            Some(avg) if avg < SETTLE_EPSILON => self.calm_frames += 1,
            _ => self.calm_frames = 0,
        }
        self.calm_frames >= SETTLE_FRAMES
    }
}

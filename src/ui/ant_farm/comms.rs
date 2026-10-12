//! A rune priest's scrying chamber and the carved receiver stone on the mountainside.
use super::{
    layout::Scene,
    sprites::Sprites,
    world::{BLUE, GOLD, INK, Prop},
};
use crate::{
    envoy::EnvoyHealth,
    model::{NodeKind, NodeState},
};
use eframe::egui::{Align2, FontId, Painter, Rect, vec2};

pub fn working(scene: &Scene) -> bool {
    scene
        .ants
        .iter()
        .any(|a| a.kind == NodeKind::Envoy && a.state == NodeState::Working)
}

pub fn paint(
    painter: &Painter,
    sprites: &Sprites,
    scene: &Scene,
    health: EnvoyHealth,
    _now_s: f64,
    receipt: crate::ui::envoy_receipt::ReceiptCue,
) {
    let Some(mut room) = scene.comms else { return };
    let px = room.px;
    sprites.passage(painter, room.shaft, scene.px);
    sprites.shoring(painter, room.shaft, scene.px);
    sprites.passage(painter, room.chamber, scene.px);
    let offset = vec2(receipt.offset, 0.0);
    room.chamber = room.chamber.translate(offset);
    room.receiver += offset;
    let prop = if receipt.highlight > 0.0 {
        Prop::ReceivingStone
    } else if matches!(health, EnvoyHealth::Stopped | EnvoyHealth::BridgeDown) {
        Prop::CrackedStone
    } else {
        Prop::RuneStone
    };
    if receipt.highlight > 0.0 {
        sprites.glow(
            painter,
            room.feed(),
            20.0 * px,
            GOLD,
            receipt.highlight * 0.7,
        );
    }
    sprites.standing(painter, prop, room.receiver, px, 1.0);
    let active = working(scene);
    let foot = room.pool() + vec2(0.0, 23.0 * px);
    if active {
        sprites.glow(painter, foot - vec2(0.0, 13.0 * px), 13.0 * px, BLUE, 0.45);
    }
    sprites.prop(
        painter,
        if active {
            Prop::ScryingBasin
        } else {
            Prop::Basin
        },
        room.basin_rect(),
        1.0,
    );
    painter.text(
        room.chamber.min + vec2(5.0, 3.0) * px,
        Align2::LEFT_TOP,
        "SCRYING",
        FontId::monospace(5.0 * px + 2.0),
        GOLD,
    );
}

pub fn tray(
    painter: &Painter,
    sprites: &Sprites,
    scene: &Scene,
    pending: usize,
    hovered: bool,
    offset: f32,
) {
    let Some(room) = scene.comms else { return };
    let rect = room.tray_rect().translate(vec2(offset, 0.0));
    let px = room.px;
    if hovered {
        sprites.glow(painter, rect.center(), rect.width(), GOLD, 0.5);
    }
    sprites.prop(painter, Prop::Tablets, rect, 1.0);
    if pending > 0 {
        let galley = painter.layout_no_wrap(pending.to_string(), FontId::monospace(6.0 * px), INK);
        let badge = Rect::from_min_size(
            rect.min + vec2(17.0 * px - galley.size().x - 2.0 * px, -2.0 * px),
            galley.size() + vec2(2.0, 1.0) * px,
        );
        painter.rect_filled(badge.expand(px), 0.0, INK);
        painter.rect_filled(badge, 0.0, GOLD);
        painter.galley(badge.min + vec2(px, 0.0), galley, INK);
    }
}

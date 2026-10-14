//! Scenery for the Ledger, the clan's knowledge store. No stored records, counts or agent
//! behaviour yet.
use super::{
    layout::Scene,
    sprites::Sprites,
    world::{
        BLUE, Canvas, GOLD, GOLD_DARK, INK, IRON, LIGHT, PARCHMENT, Prop, RED, STONE, STONE_DARK,
        WOOD, WOOD_DARK, WOOD_LIGHT, pts,
    },
};
use eframe::egui::{Align2, FontId, Painter, Rect, pos2, vec2};

/// Fit a quiet upper chamber into unused rock, leaving existing rooms and shafts clear.
/// Narrow panes omit the scenery instead of squeezing functional rooms.
pub(super) fn placement(scene: &Scene) -> Option<Rect> {
    let px = scene.px;
    let top = scene.surface_y + 0.4 * scene.cell();
    let bottom = (top + 48.0 * px).min(scene.gallery.top() - 5.0 * px);
    if bottom - top < 35.0 * px {
        return None;
    }
    let mut occupied = vec![
        scene.throne_hall,
        scene.storage,
        Rect::from_min_max(
            pos2(scene.entrance_x() - 12.0 * px, top),
            pos2(scene.entrance_x() + 12.0 * px, bottom),
        ),
    ];
    if let Some(comms) = scene.comms {
        occupied.extend([comms.chamber, comms.shaft]);
    }
    // Reserve the whole route from each upper room down to the gallery.
    occupied.sort_by(|a, b| a.left().total_cmp(&b.left()));
    let mut left = scene.rect.left() + 8.0 * px;
    let mut best = (left, left);
    for room in occupied {
        let right = (room.left() - 6.0 * px).min(scene.rect.right() - 8.0 * px);
        if right - left > best.1 - best.0 {
            best = (left, right);
        }
        left = left.max(room.right() + 6.0 * px);
    }
    let right = scene.rect.right() - 8.0 * px;
    if right - left > best.1 - best.0 {
        best = (left, right);
    }
    let width = (best.1 - best.0).min(82.0 * px);
    if width < 57.0 * px {
        return None;
    }
    let x = (best.0 + best.1 - width) / 2.0;
    Some(Rect::from_min_max(pos2(x, top), pos2(x + width, bottom)))
}

pub(super) fn paint(painter: &Painter, sprites: &Sprites, scene: &Scene) {
    let Some(room) = placement(scene) else {
        return;
    };
    let px = scene.px;
    let shaft = Rect::from_min_max(
        pos2(room.center().x - 8.0 * px, room.bottom() - px),
        pos2(room.center().x + 8.0 * px, scene.gallery.center().y),
    );
    sprites.passage(painter, shaft, px);
    sprites.shoring(painter, shaft, px);
    sprites.masonry(painter, room);
    painter.text(
        room.min + vec2(5.0, 3.0) * px,
        Align2::LEFT_TOP,
        "LEDGER",
        FontId::monospace(5.0 * px + 2.0),
        GOLD,
    );
    let floor = room.bottom() - 3.0 * px;
    let scale = ((room.height() / px - 13.0) / 34.0).min(1.0) * px;
    sprites.standing(
        painter,
        Prop::Bookcase,
        pos2(room.left() + 19.0 * px, floor),
        scale,
        1.0,
    );
    sprites.standing(
        painter,
        Prop::ArchiveCabinet,
        pos2(room.right() - 14.0 * px, floor),
        scale,
        1.0,
    );
    sprites.standing(
        painter,
        Prop::Lectern,
        pos2(room.center().x + 2.0 * px, floor + px),
        scale,
        1.0,
    );
    // A warm wall lamp links the archive to the rest of the mine.
    sprites.standing(
        painter,
        Prop::Torch,
        pos2(room.right() - 10.0 * px, room.top() + 22.0 * px),
        px * 0.65,
        1.0,
    );
}

pub(super) fn draw(c: &mut Canvas, prop: Prop) {
    match prop {
        Prop::Bookcase => {
            c.rect(2.0, 2.0, 26.0, 30.0, WOOD_DARK);
            for x in [2.0, 26.0] {
                c.rect(x, 2.0, 2.0, 30.0, WOOD);
                c.detail(x, 3.0, 1.0, 27.0, WOOD_LIGHT);
            }
            c.rect(1.0, 1.0, 28.0, 3.0, WOOD);
            c.detail(3.0, 1.0, 23.0, 1.0, WOOD_LIGHT);
            for (row, y) in [12.0, 21.0, 30.0].into_iter().enumerate() {
                for i in 0..6 {
                    let x = 5.0 + i as f32 * 3.1;
                    let h = 5.0 + ((i + row) % 3) as f32;
                    let color = [RED, BLUE, WOOD, GOLD_DARK][(i + row * 2) % 4];
                    c.rect(x, y - h, 2.6, h, color);
                    c.detail(x, y - h + 1.0, 1.0, h - 2.0, color.gamma_multiply(1.3));
                    c.detail(x, y - 1.6, 2.5, 0.8, GOLD);
                    c.detail(x, y - h + 1.0, 2.5, 0.6, PARCHMENT);
                }
                c.rect(3.0, y, 24.0, 2.0, WOOD);
                c.detail(4.0, y, 22.0, 0.8, WOOD_LIGHT);
            }
            for x in [3.0, 26.0] {
                c.detail(x, 3.0, 1.0, 1.0, IRON);
                c.detail(x, 30.0, 1.0, 1.0, IRON);
            }
        }
        Prop::ArchiveCabinet => {
            c.rect(1.0, 4.0, 18.0, 21.0, WOOD_DARK);
            for y in [7.0, 12.0, 17.0] {
                for x in [3.0, 11.0] {
                    c.rect(x, y, 6.0, 4.0, WOOD);
                    c.detail(x + 1.0, y + 1.0, 3.0, 1.0, PARCHMENT);
                    c.detail(x + 2.0, y + 3.0, 2.0, 1.0, INK);
                }
            }
            c.rect(0.8, 3.0, 18.4, 2.0, WOOD_LIGHT);
            c.rect(2.0, 22.0, 16.0, 2.0, WOOD);
            // Indexed stone tablets lying on the cabinet, edges and a carved angular mark.
            c.rect(5.0, 0.8, 10.0, 2.2, STONE);
            c.detail(7.0, 1.0, 4.0, 0.8, STONE_DARK);
            c.detail(11.0, 1.0, 0.8, 1.5, STONE_DARK);
        }
        Prop::Lectern => {
            c.rect(9.0, 9.0, 4.0, 10.0, WOOD);
            c.detail(10.0, 10.0, 1.0, 8.0, WOOD_LIGHT);
            c.cut(
                pts(&[(4.0, 20.0), (9.0, 17.0), (13.0, 17.0), (18.0, 20.0)]),
                WOOD_DARK,
            );
            c.cut(
                pts(&[
                    (1.0, 7.0),
                    (6.0, 2.0),
                    (17.0, 2.0),
                    (21.0, 7.0),
                    (20.0, 11.0),
                    (2.0, 11.0),
                ]),
                WOOD,
            );
            c.cut(
                pts(&[
                    (3.0, 6.0),
                    (6.0, 2.0),
                    (11.0, 3.0),
                    (16.0, 2.0),
                    (19.0, 6.0),
                    (19.0, 9.0),
                    (11.0, 10.0),
                    (3.0, 9.0),
                ]),
                PARCHMENT,
            );
            c.line((11.0, 3.0), (11.0, 10.0), 0.55, WOOD_DARK);
            for y in [5.0, 7.0] {
                c.line((5.0, y), (9.0, y + 0.5), 0.35, WOOD);
                c.line((13.0, y + 0.5), (17.0, y), 0.35, WOOD);
            }
            c.detail(13.0, 8.0, 1.2, 4.0, RED);
            c.detail(8.0, 19.0, 6.0, 1.0, GOLD_DARK);
            c.detail(10.0, 19.0, 2.0, 0.7, LIGHT);
        }
        _ => unreachable!("ledger furniture only"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::layout_tests::{scene, snapshot};
    use super::*;
    #[test]
    fn ledger_fits_unused_rock_and_leaves_functional_rooms_clear() {
        use super::super::layout::{FarmInput, layout};
        use super::super::layout_tests::node;
        use crate::model::{NodeKind, NodeState};
        use std::collections::{BTreeSet, HashSet};
        assert!(
            placement(&scene(&snapshot())).is_some(),
            "normal farm has a ledger"
        );
        for envoy in [false, true] {
            let mut clan = snapshot();
            if envoy {
                clan.nodes
                    .push(node("envoy", NodeKind::Envoy, None, NodeState::Idle));
            }
            let input = FarmInput {
                snapshot: &clan,
                hidden: &BTreeSet::new(),
                pr_ready: &HashSet::new(),
                building: &HashSet::new(),
                now_ms: 0,
            };
            for width in [320.0, 640.0, 960.0, 1280.0, 1920.0] {
                let scene = layout(
                    &input,
                    Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 800.0)),
                );
                let Some(room) = placement(&scene) else {
                    continue;
                };
                assert!(scene.rect.contains_rect(room));
                assert!(room.bottom() < scene.gallery.top());
                // Include the ledger shaft: it must not pass through a stacked room.
                let route = Rect::from_min_max(room.min, pos2(room.right(), scene.gallery.top()));
                assert!(!route.intersects(scene.throne_hall));
                assert!(!route.intersects(scene.storage));
                assert!(!(room.left()..room.right()).contains(&scene.entrance_x()));
                if let Some(comms) = scene.comms {
                    assert!(!route.intersects(comms.chamber));
                    assert!(!route.intersects(comms.shaft));
                }
            }
        }
    }
}

//! The Ledger's chamber: the clan's knowledge store, with its entry count on the sign. Scrolls
//! fly to and from its lectern in [`super::mail`]; the farm shows the newest entries on hover.
use super::{
    layout::Scene,
    sprites::Sprites,
    world::{
        BLUE, Canvas, GOLD, GOLD_DARK, INK, IRON, LIGHT, PARCHMENT, Prop, RED, STONE, STONE_DARK,
        WOOD, WOOD_DARK, WOOD_LIGHT, pts,
    },
};
use eframe::egui::{Align2, FontId, Painter, Pos2, Rect, Vec2, pos2, vec2};

/// Rock left between the throne hall and a chamber stacked under it, in cells.
pub(super) const GAP: f32 = 0.4;
/// A stacked chamber's height, in cells.
pub(super) const HEIGHT: f32 = 2.6;
/// What a stacked chamber adds under the throne hall, in cells; the band keeps the gallery
/// below it.
pub(super) const STACK: f32 = GAP + HEIGHT;
/// Width of a compact chamber in texels, leaving room between its three pieces of furniture.
const COMPACT_WIDTH: f32 = 64.0;

/// Clear two lines of name tags below the surface, including at the smallest farm scales.
pub(super) fn surface_clearance(px: f32) -> f32 {
    (16.0 * px).max(30.0)
}

/// Where the band puts the chamber; see [`spot`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Spot {
    /// In free rock beside the other upper rooms: `left..right` from the view's left, `height`
    /// tall.
    Beside { left: f32, right: f32, height: f32 },
    /// Under the throne hall, on its shaft: the King's library.
    Stacked,
}

/// Where the chamber goes in a `size` pane at `px`: in the widest stretch of free rock beside
/// the upper rooms (`occupied`, their columns from the view's left; `room_h`, the rock's height
/// under them) when it fits there at the farm's scale, which costs no height. Otherwise under the
/// throne hall when the pane is taller than wide, where height is to spare. On wider panes,
/// a compact chamber uses its own scale so the gallery and dwarves need not move to fit it.
pub(super) fn spot(size: Vec2, px: f32, mut occupied: Vec<(f32, f32)>, room_h: f32) -> Spot {
    occupied.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut left = 8.0 * px;
    let mut best = (left, left);
    for (room_left, room_right) in occupied {
        let right = (room_left - 3.0 * px).min(size.x - 8.0 * px);
        if right - left > best.1 - best.0 {
            best = (left, right);
        }
        left = left.max(room_right + 3.0 * px);
    }
    let right = size.x - 8.0 * px;
    if right - left > best.1 - best.0 {
        best = (left, right);
    }
    let width = (best.1 - best.0).min(82.0 * px);
    let height = room_h.min(48.0 * px);
    let beside = |height| {
        let left = (best.0 + best.1 - width) / 2.0;
        Spot::Beside {
            left,
            right: left + width,
            height,
        }
    };
    if width >= 57.0 * px && height >= 35.0 * px {
        return beside(height);
    }
    if size.x > size.y {
        let compact_px = (width / COMPACT_WIDTH).min(px);
        if compact_px >= 1.0 && room_h >= 35.0 * compact_px {
            return beside(35.0 * compact_px);
        }
    }
    // Even a pane too narrow for the compact chamber keeps the Ledger, below the King.
    Spot::Stacked
}

/// Painting and scroll destinations share the chamber's scale, including during a resize.
fn room_px(room: Rect, px: f32) -> f32 {
    px.min(room.width() / COMPACT_WIDTH)
}

/// Where Ledger scrolls are filed and taken: the lectern, straight above the chamber's shaft.
pub(super) fn lectern(scene: &Scene) -> Option<Pos2> {
    let room = scene.ledger?;
    Some(pos2(
        room.center().x,
        room.bottom() - 12.0 * room_px(room, scene.px),
    ))
}

pub(super) fn paint(painter: &Painter, sprites: &Sprites, scene: &Scene, count: usize) {
    let Some(room) = scene.ledger else {
        return;
    };
    let px = room_px(room, scene.px);
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
        if count == 0 {
            "LEDGER".to_owned()
        } else {
            format!("LEDGER · {count}")
        },
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
    use super::super::layout_tests::{snapshot, typical_clan};
    use super::*;
    #[test]
    fn ledger_is_visible_without_overlapping_the_hold_at_every_pane_shape() {
        use super::super::layout::{FarmInput, layout};
        use super::super::layout_tests::node;
        use crate::model::{NodeKind, NodeState};
        use std::collections::{BTreeSet, HashSet};
        let ctx = eframe::egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |_| {});
        output.textures_delta.clear();
        for envoy in [false, true] {
            for mut clan in [
                snapshot(),
                typical_clan(5),
                typical_clan(15),
                typical_clan(30),
            ] {
                clan.nodes.retain(|n| n.kind != NodeKind::Envoy);
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
                for (width, height) in [
                    (320.0, 800.0),
                    (640.0, 800.0),
                    (960.0, 800.0),
                    (1092.0, 1444.0),
                    (1573.0, 1136.0),
                    (1280.0, 800.0),
                    (900.0, 650.0),
                    (1920.0, 1080.0),
                ] {
                    let scene = layout(
                        &input,
                        Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height)),
                    );
                    let at = format!(
                        "{width}x{height}, envoy {envoy}, {} dwarves",
                        clan.nodes.len()
                    );
                    let room = scene
                        .ledger
                        .unwrap_or_else(|| panic!("{at}: missing Ledger"));
                    let hall = scene.throne_hall;
                    let stacked = room.top() > hall.bottom();
                    assert!(scene.rect.contains_rect(room), "{at}");
                    assert!(room.top() > scene.surface_y, "{at}");
                    let tag_height = ctx.fonts_mut(|fonts| {
                        super::super::TAG_ROWS as f32
                            * fonts.row_height(&FontId::proportional(8.0 + 1.5 * scene.px))
                    });
                    assert!(
                        room.top() >= scene.surface_y + tag_height,
                        "{at}: clear of surface name tags"
                    );
                    assert!(room.bottom() < scene.gallery.top(), "{at}");
                    assert!(room.contains(lectern(&scene).unwrap()), "{at}");
                    if stacked {
                        assert_eq!((room.left(), room.right()), (hall.left(), hall.right()));
                    }
                    // Beside, its shaft must miss every room; stacked, it shares the hall's columns,
                    // clear of the neighbours wherever the pane is not tiny.
                    if !stacked || width >= 900.0 {
                        let route =
                            Rect::from_min_max(room.min, pos2(room.right(), scene.gallery.top()));
                        let mut others = vec![scene.storage];
                        if !stacked {
                            others.push(hall);
                        }
                        others.extend(scene.comms.map(|c| c.chamber));
                        others.extend(scene.comms.map(|c| c.shaft));
                        others.extend(scene.tunnels.iter().flat_map(|t| [t.chamber, t.shaft]));
                        others.push(Rect::from_min_max(
                            pos2(scene.entrance_x() - 0.6 * scene.cell(), scene.surface_y),
                            pos2(scene.entrance_x() + 0.6 * scene.cell(), scene.gallery.top()),
                        ));
                        for r in others {
                            assert!(!route.intersects(r), "{at}: {r:?}");
                        }
                        let x = scene.entrance_x();
                        assert!(!(room.left()..room.right()).contains(&x), "{at}");
                    }
                }
            }
        }
    }
}

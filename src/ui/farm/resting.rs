//! The rest room shares the Ledger's dressed stone and the mine's cached cutout props.
use super::{
    layout::{Scene, Tunnel},
    sprites::Sprites,
    world::{
        Canvas, GOLD, GOLD_DARK, INK, IRON, LIGHT, PARCHMENT, Prop, RED, STONE, STONE_DARK,
        STONE_LIGHT, WOOD, WOOD_DARK, WOOD_LIGHT, pts,
    },
};
use eframe::egui::{Painter, Rect, pos2, vec2};

/// The hearth and refreshments occupy the left end of every sleeping row. At very narrow
/// widths the alcove contracts, leaving a full bed slot; furniture scales within it.
pub(super) fn alcove(width: f32, a: f32) -> f32 {
    (4.0 * a).min((width - 2.0 * a).max(0.0))
}

pub(super) fn paint(
    painter: &Painter,
    sprites: &Sprites,
    scene: &Scene,
    room: &Tunnel,
    still: bool,
) {
    let px = scene.px;
    let chamber = room.chamber;
    sprites.masonry(painter, chamber);
    let mut floors: Vec<f32> = Vec::new();
    for bed in &scene.beds {
        let y = bed.rect.bottom();
        if !floors.iter().any(|f| (f - y).abs() < px) {
            floors.push(y);
        }
    }
    // Each sleeping floor opens onto the existing local ladder, including overflow rows.
    for &floor in &floors {
        let entrance = Rect::from_min_max(
            pos2(scene.tunnel_ladder(room).right(), floor - 14.0 * px),
            pos2(chamber.left() + 5.0 * px, floor),
        );
        sprites.passage(painter, entrance, px);
        let doorway = Rect::from_min_size(
            pos2(chamber.left() - 2.0 * px, floor - 17.0 * px),
            vec2(10.0, 19.0) * px,
        );
        sprites.prop(painter, Prop::RestDoor, doorway, 1.0);
    }

    let scale = (alcove(chamber.width(), scene.cell()) / 64.0).min(px);
    let origin = chamber.min + vec2(7.0 * scale, 1.0 * px);
    for (prop, x, y) in [
        (Prop::Rug, 8.0, 23.0),
        (Prop::Hearth, 12.0, 0.0),
        (Prop::Barrel, 41.0, 14.0),
        (Prop::Mug, 44.0, 8.0),
        (Prop::Lantern, 1.0, 0.0),
    ] {
        let [w, h] = prop.size().map(|n| n as f32);
        let rect = Rect::from_min_size(origin + vec2(x, y) * scale, vec2(w, h) * scale);
        let frame = if still {
            0
        } else {
            prop.frame(painter.ctx().input(|i| i.time))
        };
        sprites
            .world
            .borrow_mut()
            .paint(painter, prop, rect, px, frame, 1.0);
    }

    // Wood floors beneath the beds keep wrapped rows legible as bunks, without mine rails.
    for y in floors {
        let left = chamber.left() + alcove(chamber.width(), scene.cell());
        let floor = Rect::from_min_max(
            pos2(left, y),
            pos2(chamber.right() - 3.0 * px, y + 2.0 * px),
        );
        sprites.prop(painter, Prop::RestFloor, floor, 1.0);
    }
}

pub(super) fn draw(c: &mut Canvas, prop: Prop, frame: u8) {
    match prop {
        Prop::Hearth => {
            c.rect(6.0, 1.0, 16.0, 9.0, STONE_DARK);
            c.rect(2.0, 9.0, 24.0, 19.0, STONE);
            c.rect(7.0, 13.0, 14.0, 14.0, INK);
            for y in [11.0, 17.0, 23.0] {
                c.detail(3.0, y, 3.0, 1.0, STONE_LIGHT);
                c.detail(22.0, y, 3.0, 1.0, STONE_DARK);
            }
            c.rect(1.0, 8.0, 26.0, 3.0, STONE_LIGHT);
            c.rect(1.0, 27.0, 26.0, 3.0, STONE_DARK);
            c.line((9.0, 25.0), (19.0, 24.0), 1.0, WOOD);
            super::world::flame(c, 14.0, 25.0, frame, 0.8);
        }
        Prop::Barrel => {
            c.cut(
                pts(&[
                    (4.0, 1.0),
                    (12.0, 1.0),
                    (15.0, 6.0),
                    (14.0, 15.0),
                    (11.0, 18.0),
                    (4.0, 18.0),
                    (1.0, 14.0),
                    (1.0, 5.0),
                ]),
                WOOD,
            );
            for x in [5.0, 9.0, 12.0] {
                c.detail(x, 3.0, 1.0, 14.0, WOOD_DARK);
            }
            c.rect(2.0, 4.0, 12.0, 2.0, IRON);
            c.rect(2.0, 13.0, 12.0, 2.0, IRON);
            c.oval(8.0, 2.0, 5.0, 1.5, WOOD_LIGHT);
            c.detail(7.0, 8.0, 2.0, 2.0, GOLD_DARK);
        }
        Prop::Mug => {
            c.rect(5.0, 2.0, 3.0, 4.0, GOLD_DARK);
            c.rect(1.0, 1.0, 5.0, 6.0, PARCHMENT);
            c.detail(2.0, 2.0, 3.0, 1.0, WOOD_DARK);
            c.detail(2.0, 4.0, 1.0, 2.0, LIGHT);
        }
        Prop::Lantern => {
            c.line((5.0, 1.0), (5.0, 4.0), 0.7, IRON);
            c.rect(2.0, 5.0, 6.0, 8.0, GOLD_DARK);
            c.detail(3.0, 6.0, 4.0, 5.0, LIGHT);
            c.detail(4.0, 7.0, 2.0, 3.0, GOLD);
            c.rect(1.0, 4.0, 8.0, 2.0, IRON);
            c.rect(1.0, 12.0, 8.0, 2.0, IRON);
        }
        Prop::Rug => {
            c.cut(
                pts(&[(4.0, 1.0), (30.0, 1.0), (34.0, 8.0), (1.0, 8.0)]),
                RED,
            );
            c.line((6.0, 3.0), (28.0, 3.0), 0.5, GOLD);
            c.line((4.0, 6.0), (30.0, 6.0), 0.5, GOLD);
            for x in [4.0, 10.0, 16.0, 22.0, 28.0] {
                c.detail(x, 8.0, 1.0, 2.0, PARCHMENT);
            }
        }
        Prop::RestDoor => {
            c.rect(1.0, 1.0, 3.0, 17.0, STONE);
            c.rect(1.0, 1.0, 8.0, 3.0, WOOD);
            c.rect(7.0, 4.0, 2.0, 14.0, WOOD_DARK);
            c.detail(2.0, 5.0, 1.0, 10.0, STONE_LIGHT);
            c.rect(0.0, 17.0, 10.0, 2.0, STONE_LIGHT);
        }
        Prop::RestFloor => {
            c.rect(0.0, 0.0, 32.0, 2.0, WOOD_DARK);
            c.detail(0.0, 0.0, 32.0, 0.6, WOOD_LIGHT);
        }
        _ => unreachable!("rest room furniture"),
    }
}

//! Pixel portraits drawn from the same dwarf parts as the farm.
use crate::ui::dwarf::{self, Action, Look, cutout::Finish, pixel};
use eframe::egui::{self, RichText, Ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Portrait {
    King,
    Still,
    Sleeping,
}

/// Cached code-drawn portrait with nearest-neighbour scaling.
pub fn portrait(ctx: &egui::Context, who: Portrait, size: f32) -> egui::Image<'static> {
    let id = egui::Id::new(("dwarf_portrait", who));
    let texture = ctx
        .data_mut(|data| data.get_temp::<egui::TextureHandle>(id))
        .unwrap_or_else(|| {
            let look = if who == Portrait::King {
                Look::king()
            } else {
                Look::from_seed(7)
            };
            let action = if who == Portrait::Sleeping {
                Action::Sleep
            } else {
                Action::Still
            };
            let drawing = dwarf::draw(
                egui::pos2(48.0, 89.0),
                64.0,
                &look,
                &dwarf::pose(action, 0.0),
                true,
                &Finish {
                    outline: 1.6,
                    ..Finish::INK
                },
            );
            let image = pixel::rasterize(
                &drawing.shapes,
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(96.0, 96.0)),
                [96, 96],
                1,
            );
            let texture = ctx.load_texture(
                format!("dwarf_portrait/{who:?}"),
                image,
                egui::TextureOptions::NEAREST,
            );
            ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
            texture
        });
    egui::Image::new((texture.id(), egui::Vec2::splat(size)))
        .texture_options(egui::TextureOptions::NEAREST)
}

pub fn queen_avatar(ctx: &egui::Context, size: f32) -> egui::Image<'static> {
    portrait(ctx, Portrait::King, size)
}

pub fn empty_state(ui: &mut Ui, who: Portrait, width: f32, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(16.0);
        ui.add(portrait(ui.ctx(), who, width));
        ui.add_space(8.0);
        ui.label(RichText::new(title).strong());
        ui.weak(hint);
    });
}

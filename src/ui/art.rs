//! The colony artwork (see `assets/LICENSES.md`): the Queen's avatar, a worker ant in a comb
//! cell and an anthill, as SVGs rasterized by egui_extras' loader at the size they are drawn.

use eframe::egui::load::{SizeHint, TexturePoll};
use eframe::egui::{self, ImageSource, RichText, TextureId, Ui};

const QUEEN_URI: &str = "bytes://myrstack/queen.svg";
const QUEEN: &[u8] = include_bytes!("../../assets/art/queen.svg");
const ANT_CELL_URI: &str = "bytes://myrstack/ant_cell.svg";
const ANT_CELL: &[u8] = include_bytes!("../../assets/art/ant_cell.svg");
const ANTHILL_URI: &str = "bytes://myrstack/anthill.svg";
const ANTHILL: &[u8] = include_bytes!("../../assets/art/anthill.svg");

/// Rasterized size of the Queen's avatar in the graph; drawn scaled from there, so zooming
/// never re-rasterizes it.
const GRAPH_AVATAR_PX: u32 = 256;

/// The Queen's avatar: a crowned ant on a honey hexagon, the shape of her graph node.
pub fn queen() -> ImageSource<'static> {
    ImageSource::Bytes {
        uri: QUEEN_URI.into(),
        bytes: QUEEN.into(),
    }
}

/// A worker ant in a comb cell.
pub fn ant_cell() -> ImageSource<'static> {
    ImageSource::Bytes {
        uri: ANT_CELL_URI.into(),
        bytes: ANT_CELL.into(),
    }
}

/// An anthill with its chambers and a few ants.
pub fn anthill() -> ImageSource<'static> {
    ImageSource::Bytes {
        uri: ANTHILL_URI.into(),
        bytes: ANTHILL.into(),
    }
}

/// Installs the image loaders and registers the artwork's bytes, so it can be loaded by uri
/// outside of an `egui::Image` too (the graph's Queen).
pub fn install(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
    ctx.include_bytes(QUEEN_URI, QUEEN);
}

/// The Queen's avatar as a texture for the graph, or `None` while it is not loaded (the node
/// then draws its vector fallback).
pub fn queen_texture(ctx: &egui::Context) -> Option<TextureId> {
    let options = egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
    let hint = SizeHint::Size {
        width: GRAPH_AVATAR_PX,
        height: GRAPH_AVATAR_PX,
        maintain_aspect_ratio: true,
    };
    match ctx.try_load_texture(QUEEN_URI, options, hint) {
        Ok(TexturePoll::Ready { texture }) => Some(texture.id),
        _ => None,
    }
}

/// The Queen's avatar, `size` points square.
pub fn queen_avatar(size: f32) -> egui::Image<'static> {
    egui::Image::new(queen()).fit_to_exact_size(egui::vec2(size, size))
}

/// A centered empty state: `image` `width` points wide, a title and a hint under it.
pub fn empty_state(ui: &mut Ui, image: ImageSource<'static>, width: f32, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(16.0);
        ui.add(egui::Image::new(image).max_width(width));
        ui.add_space(8.0);
        ui.label(RichText::new(title).strong());
        ui.weak(hint);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SVGs parse, rasterize and come back as textures.
    #[test]
    fn artwork_loads() {
        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .drop_without_applying_deltas();
        assert!(queen_texture(&ctx).is_some());
        for source in [queen(), ant_cell(), anthill()] {
            let ImageSource::Bytes { uri, bytes } = source else {
                unreachable!()
            };
            ctx.include_bytes(uri.clone(), bytes);
            let poll = ctx.try_load_texture(
                &uri,
                egui::TextureOptions::LINEAR,
                SizeHint::Scale(1.0.into()),
            );
            assert!(matches!(poll, Ok(TexturePoll::Ready { .. })), "{uri}");
        }
    }
}

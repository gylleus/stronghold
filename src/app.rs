//! The eframe `App`: owns UI state and draws the panels each frame.

use eframe::egui;

use crate::APP_NAME;
use crate::hotkey::Hotkey;

/// Top-level application state.
#[derive(Default)]
pub struct MyrstackApp {
    /// Global show-window hotkey; `None` if registration failed.
    hotkey: Option<Hotkey>,
}

impl MyrstackApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let hotkey = Hotkey::register(&cc.egui_ctx)
            .inspect_err(|err| log::warn!("global hotkey unavailable: {err:#}"))
            .ok();
        Self { hotkey }
    }
}

impl eframe::App for MyrstackApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(hotkey) = &mut self.hotkey {
            hotkey.poll(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.heading(APP_NAME);
        });
        egui::CentralPanel::default().show(ui, |_ui| {});
    }
}

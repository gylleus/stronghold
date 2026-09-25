//! The eframe `App`: owns UI state and draws the panels each frame.

use eframe::egui;

use crate::APP_NAME;

/// Top-level application state.
#[derive(Default)]
pub struct MyrstackApp {}

impl MyrstackApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }
}

impl eframe::App for MyrstackApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.heading(APP_NAME);
        });
        egui::CentralPanel::default().show(ui, |_ui| {});
    }
}

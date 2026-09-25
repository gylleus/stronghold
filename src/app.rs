//! The eframe `App`: owns UI state and draws the panels each frame.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;

use crate::APP_NAME;
use crate::fleet::FleetData;
use crate::hotkey::Hotkey;
use crate::model::{FleetSnapshot, NodeId};
use crate::orca::OrcaClient;
use crate::poller::{Poller, PollerCommand, PollerConfig};
use crate::ui;
use crate::ui::graph::GraphView;

/// How often the UI redraws without new data, so relative times keep ticking.
const TICK: Duration = Duration::from_secs(1);

/// Which view fills the central panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Graph,
    Table,
}

/// Top-level application state.
pub struct MyrstackApp {
    /// Global show-window hotkey; `None` if registration failed.
    hotkey: Option<Hotkey>,
    poller: Poller,
    /// Newest snapshot; `None` until the first poll completes.
    data: Option<FleetData>,
    /// Graph model built from `data`.
    snapshot: Option<FleetSnapshot>,
    graph: GraphView,
    /// Node selected in the graph, shared by all views and side panels.
    selected: Option<NodeId>,
    view: View,
}

impl MyrstackApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let hotkey = Hotkey::register(&cc.egui_ctx)
            .inspect_err(|err| log::warn!("global hotkey unavailable: {err:#}"))
            .ok();
        let repaint_ctx = cc.egui_ctx.clone();
        let poller = Poller::spawn(
            OrcaClient::new(),
            PollerConfig::default(),
            Some(Box::new(move || repaint_ctx.request_repaint())),
        );
        Self {
            hotkey,
            poller,
            data: None,
            snapshot: None,
            graph: GraphView::new(),
            selected: None,
            view: View::default(),
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        ui.horizontal(|ui| {
            ui.heading(APP_NAME);
            ui.separator();
            ui.selectable_value(&mut self.view, View::Graph, "Graph");
            ui.selectable_value(&mut self.view, View::Table, "Table");
            if self.view == View::Graph && ui.button("Fit").clicked() {
                self.graph.fit_to_screen();
            }
            ui.separator();
            if let Some(data) = &self.data {
                ui.label(format!("{} terminals", data.terminals.len()));
                ui.separator();
                ui.label(format!(
                    "updated {} ago",
                    ui::table::format_age(now_ms - data.polled_at_ms)
                ));
            }
            if ui.button("Refresh").clicked() {
                self.poller.send(PollerCommand::RefreshNow);
            }
            if let Some(data) = &self.data
                && let Some(first) = data.errors.first()
            {
                ui.separator();
                let text = if data.errors.len() > 1 {
                    format!("{first} (+{} more)", data.errors.len() - 1)
                } else {
                    first.clone()
                };
                ui.add(
                    egui::Label::new(egui::RichText::new(text).color(ui.visuals().error_fg_color))
                        .truncate(),
                )
                .on_hover_text(data.errors.join("\n"));
            }
        });
    }
}

impl eframe::App for MyrstackApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(hotkey) = &mut self.hotkey {
            hotkey.poll(ctx);
        }
        if let Some(data) = self.poller.latest() {
            log::debug!(
                "snapshot: {} terminals, {} runs, {} workers, {} errors",
                data.terminals.len(),
                data.runs.len(),
                data.workers.len(),
                data.errors.len()
            );
            let snapshot = FleetSnapshot::build(&data, now_ms());
            self.graph.sync(&snapshot);
            if let Some(id) = &self.selected
                && snapshot.node(id).is_none()
            {
                self.selected = None;
            }
            self.snapshot = Some(snapshot);
            self.data = Some(data);
        }
        ctx.request_repaint_after(TICK);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now_ms = now_ms();
        egui::Panel::top("top_bar").show(ui, |ui| self.top_bar(ui, now_ms));
        egui::CentralPanel::default().show(ui, |ui| match &self.data {
            None => {
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connecting to Orca…");
                    });
                });
            }
            Some(data) => match self.view {
                View::Graph => {
                    let response = self.graph.show(ui);
                    self.selected = response.selected;
                }
                View::Table => ui::table::terminal_table(ui, data, now_ms),
            },
        });
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

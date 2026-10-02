//! The app side of the dock: one tab viewer drawing each pane with the app's own methods.

use eframe::egui;
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, TabViewer};

use super::{MyrstackApp, View};
use crate::ui;
use crate::ui::dock::Pane;

/// Draws the panes for one frame.
struct Panes<'a> {
    app: &'a mut MyrstackApp,
    now_ms: i64,
}

impl TabViewer for Panes<'_> {
    type Tab = Pane;

    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::new(("pane", *tab))
    }

    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        match tab {
            Pane::Sidebar => "Fleet".into(),
            Pane::Graph if self.app.view == View::Table => "Table".into(),
            Pane::Graph if self.app.view == View::AntFarm => "Ant farm".into(),
            Pane::Graph => "Graph".into(),
            Pane::Details => "Details".into(),
            Pane::Conversation => self.app.conversation_title().into(),
            Pane::Notifications => self.app.notifications_title().into(),
            Pane::Envoy => "Envoy drafts".into(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        let now_ms = self.now_ms;
        match tab {
            Pane::Sidebar => self.app.sidebar_pane(ui, now_ms),
            Pane::Graph => self.app.graph_pane(ui, now_ms),
            Pane::Details => self.app.details_pane(ui, now_ms),
            Pane::Conversation => {
                self.app.conversation_left = Some(ui.max_rect().left());
                self.app.conversation_contents(ui, now_ms);
            }
            Pane::Notifications => self.app.notifications_pane(ui, now_ms),
            Pane::Envoy => self.app.envoy_pane(ui),
        }
    }

    fn is_closeable(&self, tab: &Pane) -> bool {
        matches!(tab, Pane::Conversation | Pane::Notifications | Pane::Envoy)
    }

    /// Closing the conversation hides it as ⌘J does; it returns with the next selection.
    /// Closing the notifications collapses their panel to its strip.
    fn on_close(&mut self, tab: &mut Pane) -> OnCloseResponse {
        if *tab == Pane::Conversation {
            self.app.hide_conversation();
        }
        OnCloseResponse::Close
    }

    fn allowed_in_windows(&self, _tab: &mut Pane) -> bool {
        false
    }

    /// Each pane scrolls its own content; the graph, chat and terminal fill theirs.
    fn scroll_bars(&self, _tab: &Pane) -> [bool; 2] {
        [false, false]
    }
}

impl MyrstackApp {
    /// The dock filling the window below the top bar.
    pub(super) fn panes(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if std::mem::take(&mut self.envoy_core.open_request) {
            self.panes.open_envoy();
        }
        self.panes.set_conversation(self.has_conversation());
        self.panes.sync_notifications();
        self.notifications_strip(ui);
        if let Some(pane) = self.bring_forward.take() {
            self.panes.activate(pane);
        }
        self.conversation_left = None;
        let mut dock = std::mem::take(&mut self.panes);
        let before = dock.clone();
        let style = egui_dock::Style::from_egui(ui.style());
        DockArea::new(dock.dock_mut())
            .id(egui::Id::new("panes"))
            .style(style)
            .show_leaf_collapse_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_close_buttons(true)
            .show_inside(ui, &mut Panes { app: self, now_ms });
        // Closing the conversation's tab took it out already; it returns to where it was.
        if before.contains(Pane::Conversation) && !dock.contains(Pane::Conversation) {
            dock.closed_conversation(before.clone());
        }
        if before.contains(Pane::Notifications) && !dock.contains(Pane::Notifications) {
            dock.closed_notifications(&before);
        }
        self.panes = dock;
        self.apply_panel_layout();
    }

    /// Restores the default pane arrangement.
    pub(super) fn reset_panes(&mut self) {
        self.panes.reset();
    }

    /// The conversation tab's title: whom it talks to.
    fn conversation_title(&self) -> String {
        let count = self.graph.selection().len();
        if count > 1 {
            format!("{count} selected")
        } else if self.shows_chat() {
            "Queen".to_owned()
        } else {
            self.selected_node()
                .map_or_else(|| "Conversation".to_owned(), |node| node.label.clone())
        }
    }

    /// The notifications tab's title, with the unread count.
    fn notifications_title(&self) -> String {
        match self
            .notifications
            .entries()
            .iter()
            .filter(|e| !e.read)
            .count()
        {
            0 => "Notifications".to_owned(),
            n => format!("Notifications ({n})"),
        }
    }

    /// Hides the conversation: the Queen's chat and the selection, like ⌘J over her chat.
    pub(super) fn hide_conversation(&mut self) {
        self.chat_hidden = true;
        self.selected = None;
        self.graph.set_selected(None);
    }

    /// Filters, runs and the whip log.
    fn sidebar_pane(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        let Some(snapshot) = &self.snapshot else {
            ui.weak("Waiting for the first fleet poll…");
            return;
        };
        let mut whip_pick = None;
        let filter = &mut self.filter;
        let (whip_log, whip_status) = (&mut self.whip_log, &self.whip_status);
        let cache = &mut self.sidebar_cache;
        let response = egui::Frame::side_top_panel(ui.style())
            .show(ui, |ui| {
                ui::sidebar::sidebar_cached(ui, snapshot, cache, filter, |ui| {
                    whip_pick = whip_log.section(ui, whip_status, snapshot, now_ms);
                })
            })
            .inner;
        if response.filter_changed {
            self.graph.set_hidden(&self.filter.hidden(snapshot));
        }
        if let Some(run_id) = &response.focus_run {
            self.graph.focus_run(run_id);
        }
        if let Some(repo) = &response.focus_repo {
            self.graph.focus_repo(repo);
        }
        if let Some(id) = &response.focus_node {
            self.graph.center_on(id);
        }
        if let Some(id) = response.select {
            self.graph.set_selected(Some(&id));
            self.selected = Some(id);
        }
        if response.close_shown {
            self.close.request(ui::close::CloseRequest::Shown);
        }
        if let Some(repo) = &response.new_session_repo {
            self.open_spawn_dialog(Some(repo));
        }
        if let Some(id) = whip_pick {
            self.select_and_focus(id);
        }
    }

    /// The graph with its legend and hidden-selection notice, or the table.
    fn graph_pane(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        self.stale_banner(ui, now_ms);
        self.hidden_selection_notice(ui);
        if self.view == View::Graph {
            egui::Panel::bottom("legend")
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .inner_margin(egui::Margin::symmetric(10, 6)),
                )
                .show(ui, ui::graph::legend);
        }
        let canvas = egui::CentralPanel::default().show(ui, |ui| match &self.data {
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.add(egui::Image::new(ui::art::ant_cell()).max_width(96.0));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connecting to Orca…");
                    });
                });
            }
            Some(data) => match self.view {
                View::Graph => {
                    let response = self.graph.show(ui);
                    // A node selected while filtered out is not in the graph; keep it.
                    let hidden_selected = self
                        .selected
                        .as_ref()
                        .is_some_and(|id| self.graph.hidden().contains(id));
                    if response.selected.is_some() || !hidden_selected {
                        self.selected = response.selected;
                    }
                    if !response.close_requested.is_empty() {
                        self.close
                            .request(ui::close::CloseRequest::Nodes(response.close_requested));
                    }
                    if !response.whip_requested.is_empty() {
                        self.request_whip(response.whip_requested);
                    }
                }
                View::Table => ui::table::terminal_table(ui, data, now_ms),
                View::AntFarm => {
                    let Some(snapshot) = &self.snapshot else {
                        return;
                    };
                    self.ant_farm.set_queen_busy(self.queen_thinking());
                    self.ant_farm.set_queen_focused(self.shows_chat());
                    let farm = egui::Rect::from_min_size(ui.cursor().min, ui.available_size());
                    let response = self.ant_farm.show(ui, snapshot, &self.graph);
                    self.worktrees.broom_in_farm(ui, farm);
                    self.broom_in_farm = true;
                    if let Some(id) = &response.clicked {
                        let additive = ui.input(|i| ui::graph::is_additive(i.modifiers));
                        self.graph.click_node(id, additive);
                        self.selected = self.graph.selected();
                    } else if response.clicked_empty
                        && !ui.input(|i| ui::graph::is_additive(i.modifiers))
                    {
                        self.graph.set_selected(None);
                        self.selected = None;
                    }
                    if let Some((hits, additive)) = response.boxed {
                        let mut selection = if additive {
                            self.graph.selection()
                        } else {
                            Default::default()
                        };
                        selection.extend(hits.iter().cloned());
                        let primary = self
                            .graph
                            .selected()
                            .filter(|p| selection.contains(p))
                            .or_else(|| hits.first().cloned());
                        self.graph.set_selection(&selection, primary.as_ref());
                        self.selected = self.graph.selected();
                    }
                    if !response.close_requested.is_empty() {
                        self.close
                            .request(ui::close::CloseRequest::Nodes(response.close_requested));
                    }
                    if !response.whip_requested.is_empty() {
                        self.request_whip(response.whip_requested);
                    }
                }
            },
        });
        if self
            .freshness(now_ms)
            .is_some_and(crate::stale::Freshness::is_failing)
        {
            // Dims the old data; paint only, so the canvas stays usable.
            let veil = ui.visuals().panel_fill.gamma_multiply(0.45);
            ui.painter().rect_filled(canvas.response.rect, 0.0, veil);
        }
    }

    /// The selected node's attributes and actions, or the Queen's review queue, fleet
    /// summary and broadcast.
    fn details_pane(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if !self.has_details() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| ui.weak("Select a node to see its details."));
            return;
        }
        egui::Frame::side_top_panel(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("details_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| self.details_contents(ui, now_ms));
        });
    }
}

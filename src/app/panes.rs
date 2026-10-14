//! The app side of the dock: one tab viewer drawing each pane with the app's own methods.

use eframe::egui;
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, TabViewer};

use super::StrongholdApp;
use crate::ui;
use crate::ui::dock::Pane;

/// Draws the panes for one frame.
struct Panes<'a> {
    app: &'a mut StrongholdApp,
    now_ms: i64,
}

impl TabViewer for Panes<'_> {
    type Tab = Pane;

    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::new(("pane", *tab))
    }

    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        match tab {
            Pane::Sidebar => "Clan".into(),
            Pane::Farm => "Hold".into(),
            Pane::Details => "Details".into(),
            Pane::Conversation => self.app.conversation_title().into(),
            Pane::Notifications => self.app.notifications_title().into(),
            Pane::Envoy => "Envoy".into(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        let now_ms = self.now_ms;
        match tab {
            Pane::Sidebar => self.app.sidebar_pane(ui, now_ms),
            Pane::Farm => self.app.farm_pane(ui, now_ms),
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

    /// Each pane scrolls its own content; the clan, chat and terminal fill theirs.
    fn scroll_bars(&self, _tab: &Pane) -> [bool; 2] {
        [false, false]
    }
}

impl StrongholdApp {
    /// The dock filling the window below the top bar.
    pub(super) fn panes(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if std::mem::take(&mut self.envoy_core.open_request) {
            self.envoy_panel.set_requests(self.envoy_core.requests());
            self.envoy_panel.open(self.envoy_core.drafts());
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
        let count = self.clan.selection().len();
        if count > 1 {
            format!("{count} selected")
        } else if self.shows_chat() {
            "King".to_owned()
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

    /// Hides the conversation: the King's chat and the selection, like ⌘J over his chat.
    pub(super) fn hide_conversation(&mut self) {
        self.chat_hidden = true;
        self.selected = None;
        self.clan.set_selected(None);
    }

    /// Filters, runs and the whip log.
    fn sidebar_pane(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        let Some(snapshot) = &self.snapshot else {
            ui.weak("Waiting for the first clan poll…");
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
            self.clan.set_hidden(&self.filter.hidden(snapshot));
        }
        if let Some(run_id) = &response.focus_run {
            self.clan.focus_run(run_id);
        }
        if let Some(repo) = &response.focus_repo {
            self.clan.focus_repo(repo);
        }
        if let Some(id) = &response.focus_node {
            self.clan.center_on(id);
        }
        if let Some(id) = response.select {
            self.clan.set_selected(Some(&id));
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

    /// The farm with its hidden-selection notice.
    fn farm_pane(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        self.stale_banner(ui, now_ms);
        self.hidden_selection_notice(ui);
        let receipt = ui::envoy_receipt::ReceiptCue::at(
            self.envoy_core.shake_started_ms,
            now_ms,
            self.worktrees.reduced_motion(),
        );
        let canvas = egui::CentralPanel::default().show(ui, |ui| match &self.data {
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.add(ui::art::portrait(ui.ctx(), ui::art::Portrait::Still, 96.0));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connecting to Orca…");
                    });
                });
            }
            Some(data) => {
                self.farm.set_envoy_receipt(receipt);
                let Some(snapshot) = &self.snapshot else {
                    return;
                };
                self.farm.set_art(self.whip_window.art());
                self.farm.set_king_busy(self.king_thinking());
                self.farm.set_king_focused(self.shows_chat());
                self.farm.set_envoy_health(self.envoy_status().health());
                self.farm.set_envoy_pending(self.envoy_core.pending_count());
                let sites = ui::farm::rubble::sites(&data.worktrees, &snapshot.nodes);
                self.farm.set_rubble(sites, self.worktrees.cleanup_job());
                let response = self.farm.show(ui, snapshot, &mut self.clan);
                self.cleanup_in_farm = true;
                if response.open_envoy {
                    self.envoy_core.open_request = true;
                }
                if let Some(id) = &response.clicked {
                    let additive = ui.input(|i| ui::clan_view::is_additive(i.modifiers));
                    self.clan.click_node(id, additive);
                    self.selected = self.clan.selected();
                } else if response.clicked_empty
                    && !ui.input(|i| ui::clan_view::is_additive(i.modifiers))
                {
                    self.clan.set_selected(None);
                    self.selected = None;
                }
                if let Some((hits, additive)) = response.boxed {
                    let mut selection = if additive {
                        self.clan.selection()
                    } else {
                        Default::default()
                    };
                    selection.extend(hits.iter().cloned());
                    let primary = self
                        .clan
                        .selected()
                        .filter(|p| selection.contains(p))
                        .or_else(|| hits.first().cloned());
                    self.clan.set_selection(&selection, primary.as_ref());
                    self.selected = self.clan.selected();
                }
                if !response.close_requested.is_empty() {
                    self.close
                        .request(ui::close::CloseRequest::Nodes(response.close_requested));
                }
                if !response.whip_requested.is_empty() {
                    self.request_whip(response.whip_requested);
                }
                self.park_from_menu(response.park_requested, response.wake_requested);
            }
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

    /// The selected node's attributes and actions, or the King's review queue, clan
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

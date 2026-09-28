//! The app side of the command palette: the state its actions read, and running an action
//! through the same code path as its shortcut or button.

use eframe::egui;

use super::{MyrstackApp, View};
use crate::ui::commands::{self, ActionContext, ActionId, Dispatch};
use crate::ui::selection::{close_targets, whip_targets};
use crate::ui::sidebar::FleetFilter;

impl MyrstackApp {
    /// What the palette's actions need to know about the app now.
    pub(super) fn palette_context(&self) -> ActionContext {
        let current = self.selected.as_ref();
        let selection = self.graph.selection();
        let selected_terminal = current
            .and_then(|id| self.snapshot.as_ref()?.node(id))
            .and_then(crate::ui::terminal::watch_handle)
            .is_some();
        ActionContext {
            has_data: self.snapshot.is_some(),
            whip_targets: whip_targets(current, &selection).len(),
            close_targets: close_targets(current, &selection).len(),
            selected_terminal,
            view: self.view,
            dry_run: self.whip_window.whip().dry_run,
            filters_active: self.filter != FleetFilter::default(),
            show_loose: self.filter.show_loose,
            show_released: self.filter.show_released,
            chat_ready: self.chat_ready(),
        }
    }

    /// Runs a palette action unless it is disabled.
    pub(super) fn run_palette_action(&mut self, ctx: &egui::Context, id: ActionId) {
        if let Some(reason) = commands::disabled_reason(id, &self.palette_context()) {
            log::debug!("palette: {id:?} is disabled: {reason}");
            return;
        }
        log::info!("palette: running {id:?}");
        match commands::dispatch(id) {
            Dispatch::Key(command) => self.run_key_command(ctx, command),
            Dispatch::OpenChat => self.open_chat(),
            Dispatch::Dictate => self.start_dictation(),
            Dispatch::StatusReport => self.status_report(),
            Dispatch::WhipStalled => self.request_whip_stalled(None),
            Dispatch::Broadcast => {
                self.select_queen();
                self.queen.open_broadcast = true;
            }
            Dispatch::ReviewQueue => self.open_review_queue(),
            Dispatch::WhipSettings => self.whip_window.open_window(),
            Dispatch::OpenSettings => self.open_settings(),
            Dispatch::ToggleDryRun => self.toggle_whip_dry_run(),
            Dispatch::ResetLayout => {
                self.view = View::Graph;
                self.graph.reset_layout();
            }
            Dispatch::SetView(view) => self.view = view,
            Dispatch::ClearFilters => self.clear_filters(),
            Dispatch::ShowOnly(state) => {
                self.filter.states = [state].into();
                self.apply_filter();
            }
            Dispatch::ToggleLoose => {
                self.filter.show_loose = !self.filter.show_loose;
                self.apply_filter();
            }
            Dispatch::NewSession => self.open_spawn_dialog(None),
            Dispatch::ToggleReleased => {
                self.filter.show_released = !self.filter.show_released;
                self.apply_filter();
            }
        }
    }

    /// Hides what the filter excludes, as a sidebar filter change does.
    fn apply_filter(&mut self) {
        if let Some(snapshot) = &self.snapshot {
            self.graph.set_hidden(&self.filter.hidden(snapshot));
        }
    }

    /// Saves and applies the whip settings with dry-run flipped, like the settings window.
    fn toggle_whip_dry_run(&mut self) {
        let mut next = self.whip_window.whip().clone();
        next.dry_run = !next.dry_run;
        match self.whip_window.apply(next) {
            Ok(_) => self.whip.update_settings(self.whip_window.whip().clone()),
            Err(err) => log::warn!("palette: could not toggle dry-run: {err}"),
        }
    }
}

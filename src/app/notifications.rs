//! The app side of notifications: every source (agents through the control API's `notify`,
//! nodes entering NeedsInput, the whip, the review queue, the Settings window's tests) goes
//! through [`MyrstackApp::raise`], which runs the dispatcher and carries out its delivery:
//! a toast, and a post to the notifier thread for the macOS pop-up and the sound. It also keeps
//! the inbox saved, draws the bell, the notifications panel and the node badges, and routes clicks on notifications,
//! toasts and inbox entries (select the node, open the link, mark read).

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui;
use serde_json::Value;

use super::{MyrstackApp, now_ms};
use crate::control::ControlError;
use crate::model::NodeId;
use crate::notify::agent::{self, AgentNotice, is_web_url};
use crate::notify::history::{Entry, History};
use crate::notify::{
    self, Incoming, NotificationCenter, NotifySettings, Outcome, Post, Severity, Sound, Source,
};
use crate::ui::dock::{Pane, Side};
use crate::ui::graph::UnreadBadge;
use crate::ui::notifications::{NoticeAction, NotificationsPanel, PanelAction, Toasts};

/// The dispatcher and inbox, the toasts on screen and what the badges were built from.
pub(super) struct AppNotifications {
    center: NotificationCenter,
    /// Where the inbox is saved; `None` in tests.
    path: Option<PathBuf>,
    dirty: bool,
    toasts: Toasts,
    /// The notifications panel's expanded entries.
    panel: NotificationsPanel,
    /// The panel's collapse or side switch, applied once the dock is drawn.
    layout_request: Option<PanelAction>,
    /// The last error posting a notification or playing a sound, for the Settings window.
    last_failure: Option<String>,
    /// History generation and snapshot time the node badges were built from.
    badges_from: Option<(u64, i64)>,
    /// The node selected when last checked: selecting a node marks its notifications read.
    last_selected: Option<NodeId>,
    /// macOS alert sound names, read when the Settings window first needs them.
    system_sounds: Option<Vec<String>>,
}

impl AppNotifications {
    /// Loads the saved inbox.
    pub(super) fn load() -> Self {
        let path = History::default_path();
        let history = path.as_deref().map(History::load).unwrap_or_default();
        log::info!(
            "notifications: {} in the inbox, {} unread",
            history.entries().len(),
            history.unread()
        );
        Self {
            center: NotificationCenter::new(history),
            path,
            dirty: false,
            toasts: Toasts::default(),
            panel: NotificationsPanel::default(),
            layout_request: None,
            last_failure: None,
            badges_from: None,
            last_selected: None,
            system_sounds: None,
        }
    }

    /// The inbox, oldest first.
    pub(super) fn entries(&self) -> &[Entry] {
        self.center.history.entries()
    }

    /// Changes whenever the inbox does.
    /// Holds back Myrstack's own needs-input notification for `target` (see
    /// [`NotificationCenter::hold_cooldown`]).
    pub(super) fn hold_cooldown(&mut self, target: &str, now_ms: i64) {
        self.center.hold_cooldown(target, now_ms);
    }

    pub(super) fn generation(&self) -> u64 {
        self.center.history.generation()
    }

    pub(super) fn last_failure(&self) -> Option<&str> {
        self.last_failure.as_deref()
    }

    pub(super) fn system_sounds(&mut self) -> &[String] {
        self.system_sounds
            .get_or_insert_with(notify::settings::system_sounds)
    }

    fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if let Some(path) = &self.path
            && let Err(e) = self.center.history.save(path)
        {
            log::warn!("notifications: saving {}: {e}", path.display());
        }
    }
}

impl MyrstackApp {
    /// Runs `n` through the dispatcher under the saved settings and delivers it.
    pub(super) fn raise(&mut self, n: Incoming) -> Outcome {
        let settings = self.whip_window.notifications().clone();
        self.raise_with(n, &settings)
    }

    fn raise_with(&mut self, n: Incoming, settings: &NotifySettings) -> Outcome {
        let outcome = self
            .notifications
            .center
            .dispatch(n.clone(), settings, now_ms());
        log::info!(
            "notification ({} {}) {:?}: {}",
            n.source.name(),
            n.severity.name(),
            outcome,
            n.title
        );
        let notes = &mut self.notifications;
        match &outcome {
            Outcome::Delivered { id, delivery } => {
                notes.dirty = true;
                if delivery.in_app
                    && let Some(entry) = notes.center.history.get(*id)
                {
                    notes.toasts.push(entry);
                }
                if let Some(post) = Post::new(&n, *id, delivery, settings) {
                    self.notifier.post(post);
                }
            }
            Outcome::Collapsed { id, .. } | Outcome::Folded { id } => {
                notes.dirty = true;
                // A toast still up shows the new count; nothing pops up again.
                if notes.toasts.ids().contains(id)
                    && let Some(entry) = notes.center.history.get(*id)
                {
                    notes.toasts.push(entry);
                }
            }
            Outcome::RateLimited { .. } | Outcome::Cooldown => {}
        }
        outcome
    }

    /// Per frame: dispatches what the whip raised, takes delivery failures and clicks, marks
    /// a newly selected node's notifications read, updates the node badges and saves.
    pub(super) fn poll_notifications(&mut self, ctx: &egui::Context) {
        for n in self.notifier.incoming() {
            self.raise(n);
        }
        if let Some(failure) = self.notifier.failures().pop() {
            self.notifications.last_failure = Some(failure);
        }
        self.open_clicked_notifications(ctx);
        self.mark_selected_read();
        self.sync_unread_badges();
        self.notifications.save();
    }

    /// Raises a notification for each node that just entered NeedsInput, unless the whip's
    /// notifications are off.
    pub(super) fn notify_needs_input(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let requests = self.needs_input_detector.update(snapshot);
        if !self.whip_window.whip().notify {
            return;
        }
        for request in requests {
            self.raise(request);
        }
    }

    /// Acts on each clicked native notification, as clicking its inbox entry would, and brings
    /// the window forward. Clicks wait for the first snapshot (a click can launch the app).
    fn open_clicked_notifications(&mut self, ctx: &egui::Context) {
        if self.snapshot.is_none() {
            return;
        }
        for clicked in self.notifier.clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            if let [first, rest @ ..] = &clicked.ids[..] {
                for id in rest {
                    self.notifications.center.history.mark_read(*id);
                }
                // A summary's link is not opened: it is one of several.
                self.activate_notice(*first, rest.is_empty());
                continue;
            }
            let select = self
                .snapshot
                .as_ref()
                .and_then(|s| notify::click_target(s, &clicked.targets));
            match select {
                Some(id) => self.select_and_focus(id),
                None if self.is_review_alert(&clicked.targets) => self.open_review_queue(),
                None if clicked
                    .targets
                    .iter()
                    .any(|t| t == super::envoy::DRAFTS_TARGET) =>
                {
                    self.envoy_core.open_request = true;
                }
                None => {}
            }
        }
    }

    /// Marks inbox entry `id` read, selects its node (or opens the review queue for a PR) and,
    /// with `open_url`, opens its link.
    fn activate_notice(&mut self, id: u64, open_url: bool) {
        let notes = &mut self.notifications;
        notes.dirty |= notes.center.history.mark_read(id);
        notes.toasts.remove(id);
        let Some(entry) = notes.center.history.get(id).cloned() else {
            return;
        };
        if open_url && let Some(url) = &entry.url {
            open_in_browser(url);
        }
        let target = entry.target.as_deref();
        let node = target.and_then(|t| notify::resolve_target(self.snapshot.as_ref()?, t));
        match node {
            None if target == Some(super::envoy::DRAFTS_TARGET) => {
                self.envoy_core.open_request = true;
            }
            Some(node) => self.select_and_focus(node),
            None if target.is_some_and(|t| self.is_review_alert(&[t.to_owned()])) => {
                self.open_review_queue();
            }
            None => {}
        }
    }

    fn apply_notice_action(&mut self, action: NoticeAction) {
        let notes = &mut self.notifications;
        match action {
            NoticeAction::Activate { id, open_url } => self.activate_notice(id, open_url),
            NoticeAction::Dismiss(id) | NoticeAction::MarkRead(id) => {
                notes.dirty |= notes.center.history.mark_read(id);
                notes.toasts.remove(id);
            }
            NoticeAction::OpenUrl(id) => {
                notes.dirty |= notes.center.history.mark_read(id);
                notes.toasts.remove(id);
                if let Some(url) = notes.center.history.get(id).and_then(|e| e.url.as_deref()) {
                    open_in_browser(url);
                }
            }
            NoticeAction::MarkAllRead => {
                notes.dirty |= notes.center.history.mark_all_read() > 0;
                notes.toasts.clear();
            }
            NoticeAction::Remove(id) => {
                notes.dirty |= notes.center.history.remove(id);
                notes.toasts.remove(id);
            }
            NoticeAction::Clear => {
                notes.center.history.clear();
                notes.toasts.clear();
                notes.dirty = true;
            }
            NoticeAction::SetQuiet(on) => {
                self.whip_window.set_quiet(on);
            }
        }
    }

    /// Selecting a node marks the notifications about it read.
    fn mark_selected_read(&mut self) {
        if self.selected == self.notifications.last_selected {
            return;
        }
        self.notifications.last_selected.clone_from(&self.selected);
        let (Some(selected), Some(snapshot)) = (&self.selected, &self.snapshot) else {
            return;
        };
        let mut read = Vec::new();
        self.notifications.center.history.mark_read_where(|e| {
            let about = e
                .target
                .as_deref()
                .and_then(|t| notify::resolve_target(snapshot, t))
                .as_ref()
                == Some(selected);
            if about {
                read.push(e.id);
            }
            about
        });
        for id in &read {
            self.notifications.toasts.remove(*id);
        }
        self.notifications.dirty |= !read.is_empty();
    }

    /// Count badges on the nodes with unread notifications.
    fn sync_unread_badges(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let key = (self.notifications.generation(), snapshot.built_at_ms);
        if self.notifications.badges_from == Some(key) {
            return;
        }
        self.notifications.badges_from = Some(key);
        let mut badges: HashMap<NodeId, UnreadBadge> = HashMap::new();
        for entry in self.notifications.entries().iter().filter(|e| !e.read) {
            let Some(node) = entry
                .target
                .as_deref()
                .and_then(|t| notify::resolve_target(snapshot, t))
            else {
                continue;
            };
            let badge = badges.entry(node).or_insert(UnreadBadge {
                count: 0,
                severity: entry.severity,
            });
            badge.count += 1;
            badge.severity = badge.severity.max(entry.severity);
        }
        self.graph.set_unread(badges);
    }

    /// The top bar's bell: opens the notifications panel, or collapses it when it is in front.
    pub(super) fn notifications_bell(&mut self, ui: &mut egui::Ui) {
        let quiet = self.whip_window.notifications().quiet;
        let open = self.panes.is_visible(Pane::Notifications);
        if crate::ui::notifications::bell_button(ui, self.notifications.entries(), quiet, open) {
            self.toggle_notifications();
        }
    }

    /// Opens the notifications panel, or collapses it when it is in front.
    pub(super) fn toggle_notifications(&mut self) {
        if self.panes.is_visible(Pane::Notifications) {
            self.panes.collapse_notifications();
        } else {
            self.panes.open_notifications();
        }
    }

    /// Carries out the panel's collapse or side switch, which wait until the dock is drawn.
    pub(super) fn apply_panel_layout(&mut self) {
        match self.notifications.layout_request.take() {
            Some(PanelAction::Collapse) => self.panes.collapse_notifications(),
            Some(PanelAction::SwitchSide) => self.panes.switch_notifications_side(),
            Some(PanelAction::Notice(_)) | None => {}
        }
    }

    /// The collapsed notifications panel's strip at its side of the window, if collapsed.
    pub(super) fn notifications_strip(&mut self, ui: &mut egui::Ui) {
        let dock = self.panes.notifications;
        if dock.open {
            return;
        }
        let quiet = self.whip_window.notifications().quiet;
        let panel = match dock.side {
            Side::Left => egui::Panel::left("notifications_strip"),
            Side::Right => egui::Panel::right("notifications_strip"),
        };
        let entries = self.notifications.entries();
        let clicked = panel
            .resizable(false)
            .exact_size(crate::ui::notifications::STRIP_WIDTH)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(0))
            .show(ui, |ui| {
                crate::ui::notifications::collapsed_strip(ui, entries, quiet)
            })
            .inner;
        if clicked {
            self.panes.open_notifications();
        }
    }

    /// The notifications pane: the history, newest first.
    pub(super) fn notifications_pane(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        let quiet = self.whip_window.notifications().quiet;
        let right = ui.max_rect().center().x > ui.ctx().content_rect().center().x;
        let action = self.notifications.panel.show(
            ui,
            self.notifications.center.history.entries(),
            quiet,
            right,
            now_ms,
        );
        match action {
            Some(PanelAction::Notice(action)) => self.apply_notice_action(action),
            // The dock is out of `self.panes` while it draws.
            layout @ Some(_) => self.notifications.layout_request = layout,
            None => {}
        }
    }

    /// The pop-ups at the top right, below `top`.
    pub(super) fn notification_toasts(&mut self, ctx: &egui::Context, top: f32) {
        if let Some(action) = self.notifications.toasts.show(ctx, top) {
            self.apply_notice_action(action);
        }
    }

    /// The Settings window's "Test" (a sound) and "Send a test notification".
    pub(super) fn notification_tests(
        &mut self,
        sound: Option<(Sound, u8)>,
        notification: Option<(Severity, NotifySettings)>,
    ) {
        if let Some((sound, volume)) = sound {
            let played = sound
                .path()
                .ok_or_else(|| anyhow::anyhow!("no sound chosen"))
                .and_then(|path| notify::play_sound(&path, f32::from(volume.min(100)) / 100.0));
            self.notifications.last_failure = played.err().map(|e| format!("sound: {e:#}"));
        }
        if let Some((severity, settings)) = notification {
            let n = Incoming::new(
                Source::Test,
                severity,
                &format!("Test notification ({})", severity.name()),
                "This is how agents' notifications of this severity reach you.",
            );
            self.raise_with(n, &settings);
        }
    }

    /// The control API's `notify`: an agent's notification to Karl. The node is the one the
    /// agent named, else the caller's own terminal.
    pub(super) fn control_notify(&mut self, notice: AgentNotice) -> Result<Value, ControlError> {
        // An unknown `agent` is not worth losing the notification over.
        let named = notice.agent.as_deref().and_then(|agent| {
            self.control_node(agent)
                .map(|n| (n.id.clone(), n.label.clone()))
                .inspect_err(|e| log::info!("notify: {}", e.message))
                .ok()
        });
        let unknown_agent = notice.agent.clone().filter(|_| named.is_none());
        let node = named.or_else(|| {
            self.review_agent(&notice.origin)
                .map(|a| (a.id.clone(), a.label.clone()))
        });
        let node = node
            .as_ref()
            .map(|(id, label)| (id.as_str(), label.as_str()));
        let outcome = self.raise(notice.incoming(node));
        let quiet = self.whip_window.notifications().quiets(notice.severity);
        // An `alerts` call right after this reply sees the new entry.
        self.publish_control_status();
        Ok(agent::reply(
            &outcome,
            &notice,
            node,
            unknown_agent.as_deref(),
            quiet,
        ))
    }

    /// What the Settings window's Notifications section shows.
    pub(super) fn notify_status(&mut self) -> (bool, Option<String>, Vec<String>) {
        (
            native_notifications(),
            self.notifications.last_failure().map(str::to_owned),
            self.notifications.system_sounds().to_vec(),
        )
    }
}

/// Whether notifications go through `UNUserNotificationCenter`.
fn native_notifications() -> bool {
    #[cfg(target_os = "macos")]
    {
        notify::native::is_bundled()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// `open <url>` for http(s) links only: a notification's link comes from an agent.
fn open_in_browser(url: &str) {
    if !is_web_url(url) {
        log::warn!("notifications: not opening {url:?}");
        return;
    }
    if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
        log::warn!("notifications: open {url}: {e}");
    }
}

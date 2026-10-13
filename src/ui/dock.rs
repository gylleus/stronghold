//! The window's panes as dock tabs (egui_dock): the sidebar, the farm, the details
//! and the conversation can be dragged, split, tabbed together and resized; the arrangement is
//! saved between launches.
//!
//! The conversation pane comes and goes with what it has to show (see
//! `MyrstackApp::has_conversation`); taking it out remembers the arrangement, so it returns
//! where it was unless the other panes moved meanwhile.
//!
//! The notifications pane is in while its panel is open; collapsing it (its tab's close
//! button or the panel's own) remembers its side and width, and it returns there.

use eframe::egui;
use egui_dock::{DockState, NodeIndex, SurfaceIndex};
use serde::{Deserialize, Serialize};

/// A dock tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pane {
    /// Filters, runs and the whip log.
    Sidebar,
    /// The farm.
    Farm,
    /// The selected node's attributes and actions, or the King's review queue and summary.
    Details,
    /// The King's chat, the selected agent's screen and input, a subagent's transcript or the
    /// multi-selection.
    Conversation,
    /// The notification history, newest first.
    Notifications,
    /// Pending Envoy drafts and delivery history.
    Envoy,
}

/// Panes that are always in the layout; the conversation and notifications can be taken out.
const FIXED: [Pane; 3] = [Pane::Sidebar, Pane::Farm, Pane::Details];

// Default widths as shares of the window, matching the old fixed columns on a 1728 pt
// window: sidebar 260, details 270, conversation 45 %.
const CONVERSATION_SHARE: f32 = 0.45;
const SIDEBAR_SHARE_OF_REST: f32 = 0.27;
const DETAILS_SHARE_OF_MIDDLE: f32 = 0.39;
/// The notifications panel's default width as a share of the window.
const NOTIFICATIONS_SHARE: f32 = 0.2;

/// A window edge the notifications panel sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Side {
    #[default]
    Left,
    Right,
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// The notifications panel: open or collapsed, and where it returns when opened.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationsDock {
    pub open: bool,
    pub side: Side,
    /// Its width as a share of the window.
    pub share: f32,
}

impl Default for NotificationsDock {
    fn default() -> Self {
        Self {
            open: true,
            side: Side::Left,
            share: NOTIFICATIONS_SHARE,
        }
    }
}

/// The pane arrangement and, while the conversation is out, the arrangement it left.
#[derive(Clone)]
pub struct PaneLayout {
    dock: DockState<Pane>,
    stashed: Option<DockState<Pane>>,
    pub notifications: NotificationsDock,
}

impl Default for PaneLayout {
    fn default() -> Self {
        Self::from_dock(default_dock())
    }
}

impl std::fmt::Debug for PaneLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneLayout")
            .field("leaves", &leaves(&self.dock))
            .finish_non_exhaustive()
    }
}

/// Sidebar | farm | details | conversation, as the columns were before docking.
pub fn default_dock() -> DockState<Pane> {
    let mut dock = DockState::new(vec![Pane::Farm]);
    let tree = dock.main_surface_mut();
    let [rest, _] = tree.split_right(
        NodeIndex::root(),
        1.0 - CONVERSATION_SHARE,
        vec![Pane::Conversation],
    );
    let [farm, _] = tree.split_left(rest, SIDEBAR_SHARE_OF_REST, vec![Pane::Sidebar]);
    tree.split_right(farm, 1.0 - DETAILS_SHARE_OF_MIDDLE, vec![Pane::Details]);
    dock
}

impl PaneLayout {
    fn from_dock(dock: DockState<Pane>) -> Self {
        Self {
            dock,
            stashed: None,
            notifications: NotificationsDock::default(),
        }
    }

    pub fn dock_mut(&mut self) -> &mut DockState<Pane> {
        &mut self.dock
    }

    /// Back to [`default_dock`].
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The saved form: RON of the dock state (JSON cannot hold its infinite placeholder
    /// rects), kept as a string so a layout from another
    /// egui_dock version fails alone instead of failing the whole saved state.
    pub fn encode(&self) -> Option<String> {
        ron::to_string(&self.dock)
            .inspect_err(|err| log::warn!("dock: could not save the layout: {err}"))
            .ok()
    }

    /// The layout saved by [`Self::encode`], or `None` if it is unreadable or lacks a pane.
    pub fn decode(saved: &str) -> Option<Self> {
        let dock: DockState<Pane> = ron::from_str(saved)
            .inspect_err(|err| log::warn!("dock: ignoring the saved layout: {err}"))
            .ok()?;
        valid(&dock).then(|| Self::from_dock(dock))
    }

    pub fn contains(&self, pane: Pane) -> bool {
        self.dock.find_tab(&pane).is_some()
    }

    /// Whether `pane` is drawn: in the layout and the active tab of its group.
    pub fn is_visible(&self, pane: Pane) -> bool {
        let Some(path) = self.dock.find_tab(&pane) else {
            return false;
        };
        self.dock
            .leaf(path.node_path())
            .is_ok_and(|leaf| leaf.active == path.tab)
    }

    /// Brings `pane` to the front of its tab group.
    pub fn activate(&mut self, pane: Pane) {
        if let Some(path) = self.dock.find_tab(&pane) {
            let _ = self.dock.set_active_tab(path);
        }
    }

    /// Puts the conversation pane in or takes it out. It returns where it was when the other
    /// panes have not moved since, else as the rightmost column.
    pub fn set_conversation(&mut self, present: bool) {
        match (present, self.dock.find_tab(&Pane::Conversation)) {
            (true, None) => {
                let stashed = self.stashed.take().filter(|stashed| {
                    let without = stashed.filter_tabs(|p| *p != Pane::Conversation);
                    leaves(&without) == leaves(&self.dock)
                });
                match stashed {
                    Some(dock) => self.dock = dock,
                    None => {
                        self.dock.main_surface_mut().split_right(
                            NodeIndex::root(),
                            1.0 - CONVERSATION_SHARE,
                            vec![Pane::Conversation],
                        );
                    }
                }
            }
            (false, Some(path)) => {
                self.stashed = Some(self.dock.clone());
                self.dock.remove_tab(path);
            }
            _ => {}
        }
    }

    /// Opens the drafts beside the details, or brings its existing tab forward.
    pub fn open_envoy(&mut self) {
        if !self.contains(Pane::Envoy) {
            self.dock
                .main_surface_mut()
                .split_right(NodeIndex::root(), 0.65, vec![Pane::Envoy]);
        }
        self.activate(Pane::Envoy);
    }

    /// The conversation's tab was closed in the dock; `before` is the layout that had it.
    pub fn closed_conversation(&mut self, before: PaneLayout) {
        self.stashed = Some(before.dock);
    }

    /// Puts the notifications pane in or takes it out to match whether its panel is open. It
    /// comes in as a full-height column at its side; taken out, it keeps its side and width.
    pub fn sync_notifications(&mut self) {
        match (
            self.notifications.open,
            self.dock.find_tab(&Pane::Notifications),
        ) {
            (true, None) => {
                let share = self.notifications.share.clamp(0.08, 0.6);
                let tree = self.dock.main_surface_mut();
                match self.notifications.side {
                    Side::Left => {
                        tree.split_left(NodeIndex::root(), share, vec![Pane::Notifications]);
                    }
                    Side::Right => {
                        tree.split_right(NodeIndex::root(), 1.0 - share, vec![Pane::Notifications]);
                    }
                }
            }
            (false, Some(path)) => {
                self.remember_notifications_place();
                self.dock.remove_tab(path);
            }
            _ => {}
        }
    }

    /// Collapses the notifications panel to its strip.
    pub fn collapse_notifications(&mut self) {
        self.notifications.open = false;
        self.sync_notifications();
    }

    /// Opens the notifications panel, bringing its tab to the front.
    pub fn open_notifications(&mut self) {
        self.notifications.open = true;
        self.sync_notifications();
        self.activate(Pane::Notifications);
    }

    /// The notifications tab was closed in the dock; `before` is the layout that had it.
    pub fn closed_notifications(&mut self, before: &PaneLayout) {
        self.notifications = before.notifications;
        if let Some((side, share)) = before.notifications_place() {
            self.notifications.side = side;
            self.notifications.share = share;
        }
        self.notifications.open = false;
    }

    /// Moves the open notifications panel to the other side of the window.
    pub fn switch_notifications_side(&mut self) {
        let side = self
            .notifications_place()
            .map_or(self.notifications.side, |(side, _)| side);
        self.collapse_notifications();
        self.notifications.side = side.other();
        self.open_notifications();
    }

    fn remember_notifications_place(&mut self) {
        if let Some((side, share)) = self.notifications_place() {
            self.notifications.side = side;
            self.notifications.share = share;
        }
    }

    /// The side and width share the notifications pane was last drawn at, from the dock's
    /// rects; `None` before it was drawn.
    fn notifications_place(&self) -> Option<(Side, f32)> {
        let path = self.dock.find_tab(&Pane::Notifications)?;
        let rect = self.dock.leaf(path.node_path()).ok()?.rect;
        let area = self
            .dock
            .iter_leaves()
            .map(|(_, leaf)| leaf.rect)
            .filter(|r| r.is_finite() && r.is_positive())
            .reduce(|a, b| a.union(b))?;
        if !rect.is_finite() || !rect.is_positive() {
            return None;
        }
        let side = if rect.center().x < area.center().x {
            Side::Left
        } else {
            Side::Right
        };
        Some((side, (rect.width() / area.width()).clamp(0.08, 0.6)))
    }
}

/// Each tab group's panes, in tree order: the arrangement without sizes.
fn leaves(dock: &DockState<Pane>) -> Vec<Vec<Pane>> {
    dock.iter_leaves()
        .map(|(_, leaf)| leaf.tabs.clone())
        .collect()
}

/// One main surface holding each fixed pane once and the conversation at most once.
fn valid(dock: &DockState<Pane>) -> bool {
    let panes: Vec<Pane> = dock.iter_all_tabs().map(|(_, pane)| *pane).collect();
    let count = |pane: Pane| panes.iter().filter(|p| **p == pane).count();
    dock.iter_surfaces_indexed()
        .all(|(index, _)| index == SurfaceIndex::main())
        && FIXED.iter().all(|pane| count(*pane) == 1)
        && count(Pane::Conversation) <= 1
        && count(Pane::Notifications) <= 1
        && count(Pane::Envoy) <= 1
}

/// Where the top bar's King quick actions line up: the conversation pane's left edge when it
/// sits in the bar's right half (else they would crowd out the bar's own controls).
pub fn quick_actions_anchor(conversation_left: Option<f32>, bar: egui::Rect) -> Option<f32> {
    conversation_left.filter(|x| *x >= bar.center().x)
}

#[cfg(test)]
mod tests {
    use egui_dock::TabIndex;

    use super::*;

    /// Each tab group's first pane with its left edge and width on a `window` wide screen,
    /// left to right, laid out the way DockArea divides the tree.
    fn columns(dock: &DockState<Pane>, window: f32) -> Vec<(Pane, f32, f32)> {
        fn place(
            tree: &egui_dock::Tree<Pane>,
            node: NodeIndex,
            rect: egui::Rect,
            out: &mut Vec<(Pane, f32, f32)>,
        ) {
            match &tree[node] {
                egui_dock::Node::Leaf(leaf) => out.push((leaf.tabs[0], rect.left(), rect.width())),
                egui_dock::Node::Horizontal(split) => {
                    let (left, right) = rect.split_left_right_at_fraction(split.fraction);
                    place(tree, node.left(), left, out);
                    place(tree, node.right(), right, out);
                }
                egui_dock::Node::Vertical(split) => {
                    let (top, bottom) = rect.split_top_bottom_at_fraction(split.fraction);
                    place(tree, node.left(), top, out);
                    place(tree, node.right(), bottom, out);
                }
                egui_dock::Node::Empty => {}
            }
        }
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(window, 900.0));
        let mut out = Vec::new();
        place(dock.main_surface(), NodeIndex::root(), rect, &mut out);
        out.sort_by(|a, b| a.1.total_cmp(&b.1));
        out
    }

    #[test]
    fn default_is_the_old_column_arrangement() {
        let dock = default_dock();
        assert!(valid(&dock));
        let placed = columns(&dock, 1728.0);
        let order: Vec<Pane> = placed.iter().map(|c| c.0).collect();
        assert_eq!(
            order,
            [Pane::Sidebar, Pane::Farm, Pane::Details, Pane::Conversation]
        );
        let width = |pane| placed.iter().find(|c| c.0 == pane).unwrap().2;
        assert!((width(Pane::Conversation) - 0.45 * 1728.0).abs() < 1.0);
        assert!((width(Pane::Sidebar) - 260.0).abs() < 10.0, "{placed:?}");
        assert!((width(Pane::Details) - 270.0).abs() < 10.0, "{placed:?}");
    }

    #[test]
    fn layout_round_trips_through_its_saved_form() {
        let mut layout = PaneLayout::default();
        // Rearrange: tab the details in with the conversation.
        let details = layout.dock.find_tab(&Pane::Details).unwrap();
        let conversation = layout.dock.find_tab(&Pane::Conversation).unwrap();
        layout.dock.move_tab(
            details,
            (conversation.node_path(), egui_dock::TabInsert::Append),
        );
        let saved = layout.encode().unwrap();
        let restored = PaneLayout::decode(&saved).unwrap();
        assert_eq!(leaves(&restored.dock), leaves(&layout.dock));
        assert_ne!(leaves(&restored.dock), leaves(&default_dock()));
    }

    #[test]
    fn unreadable_or_incomplete_layouts_are_rejected() {
        assert!(PaneLayout::decode("not json").is_none());
        let no_farm = DockState::new(vec![Pane::Sidebar, Pane::Details]);
        let saved = ron::to_string(&no_farm).unwrap();
        assert!(PaneLayout::decode(&saved).is_none());
        let twice = DockState::new(vec![Pane::Sidebar, Pane::Farm, Pane::Details, Pane::Farm]);
        let saved = ron::to_string(&twice).unwrap();
        assert!(PaneLayout::decode(&saved).is_none());
        let no_conversation = DockState::new(vec![Pane::Sidebar, Pane::Farm, Pane::Details]);
        let saved = ron::to_string(&no_conversation).unwrap();
        assert!(PaneLayout::decode(&saved).is_some());
    }

    #[test]
    fn reset_restores_the_default() {
        let mut layout = PaneLayout::default();
        layout.set_conversation(false);
        let farm = layout.dock.find_tab(&Pane::Farm).unwrap();
        let sidebar = layout.dock.find_tab(&Pane::Sidebar).unwrap();
        layout
            .dock
            .move_tab(farm, (sidebar.node_path(), egui_dock::TabInsert::Append));
        layout.reset();
        assert_eq!(leaves(&layout.dock), leaves(&default_dock()));
        assert!(layout.stashed.is_none());
    }

    #[test]
    fn conversation_returns_where_it_was() {
        let mut layout = PaneLayout::default();
        // Tab it in with the farm.
        let conversation = layout.dock.find_tab(&Pane::Conversation).unwrap();
        let farm = layout.dock.find_tab(&Pane::Farm).unwrap();
        layout.dock.move_tab(
            conversation,
            (farm.node_path(), egui_dock::TabInsert::Append),
        );
        let arranged = leaves(&layout.dock);
        layout.set_conversation(false);
        assert!(!layout.contains(Pane::Conversation));
        layout.set_conversation(true);
        assert_eq!(leaves(&layout.dock), arranged);
    }

    #[test]
    fn conversation_goes_right_when_the_others_moved() {
        let mut layout = PaneLayout::default();
        layout.set_conversation(false);
        let details = layout.dock.find_tab(&Pane::Details).unwrap();
        let farm = layout.dock.find_tab(&Pane::Farm).unwrap();
        layout
            .dock
            .move_tab(details, (farm.node_path(), egui_dock::TabInsert::Append));
        layout.set_conversation(true);
        assert!(layout.contains(Pane::Conversation));
        let root = &layout.dock.main_surface()[NodeIndex::root()];
        assert!(root.is_horizontal());
        let right = &layout.dock.main_surface()[NodeIndex::root().right()];
        assert_eq!(right.tabs(), Some(&[Pane::Conversation][..]));
    }

    #[test]
    fn a_tab_behind_another_is_not_visible_until_activated() {
        let mut layout = PaneLayout::default();
        let conversation = layout.dock.find_tab(&Pane::Conversation).unwrap();
        let farm = layout.dock.find_tab(&Pane::Farm).unwrap();
        layout.dock.move_tab(
            conversation,
            (farm.node_path(), egui_dock::TabInsert::Insert(TabIndex(0))),
        );
        let farm = layout.dock.find_tab(&Pane::Farm).unwrap();
        let _ = layout.dock.set_active_tab(farm);
        assert!(!layout.is_visible(Pane::Conversation));
        layout.activate(Pane::Conversation);
        assert!(layout.is_visible(Pane::Conversation));
        assert!(!layout.is_visible(Pane::Farm));
    }

    #[test]
    fn quick_actions_follow_the_conversation_only_on_the_right() {
        let bar = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1000.0, 30.0));
        assert_eq!(quick_actions_anchor(Some(600.0), bar), Some(600.0));
        assert_eq!(quick_actions_anchor(Some(100.0), bar), None);
        assert_eq!(quick_actions_anchor(None, bar), None);
    }

    /// Gives each leaf the rect DockArea would, as if the layout was drawn `window` wide.
    fn draw(layout: &mut PaneLayout, window: f32) {
        fn place(tree: &mut egui_dock::Tree<Pane>, node: NodeIndex, rect: egui::Rect) {
            match &mut tree[node] {
                egui_dock::Node::Leaf(leaf) => leaf.rect = rect,
                egui_dock::Node::Horizontal(split) => {
                    let (left, right) = rect.split_left_right_at_fraction(split.fraction);
                    place(tree, node.left(), left);
                    place(tree, node.right(), right);
                }
                egui_dock::Node::Vertical(split) => {
                    let (top, bottom) = rect.split_top_bottom_at_fraction(split.fraction);
                    place(tree, node.left(), top);
                    place(tree, node.right(), bottom);
                }
                egui_dock::Node::Empty => {}
            }
        }
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(window, 900.0));
        place(layout.dock.main_surface_mut(), NodeIndex::root(), rect);
    }

    #[test]
    fn notifications_open_as_a_left_column_by_default() {
        let mut layout = PaneLayout::default();
        layout.sync_notifications();
        let placed = columns(&layout.dock, 1728.0);
        assert_eq!(placed[0].0, Pane::Notifications);
        assert!((placed[0].2 - 0.2 * 1728.0).abs() < 1.0, "{placed:?}");
        let saved = layout.encode().unwrap();
        let restored = PaneLayout::decode(&saved).unwrap();
        assert!(restored.contains(Pane::Notifications));
    }

    #[test]
    fn collapsed_notifications_return_at_their_side_and_width() {
        let mut layout = PaneLayout::default();
        layout.notifications.side = Side::Right;
        layout.notifications.share = 0.25;
        layout.sync_notifications();
        draw(&mut layout, 2000.0);
        let placed = columns(&layout.dock, 2000.0);
        assert_eq!(placed.last().unwrap().0, Pane::Notifications);
        layout.collapse_notifications();
        assert!(!layout.contains(Pane::Notifications));
        assert!(!layout.notifications.open);
        assert_eq!(layout.notifications.side, Side::Right);
        assert!((layout.notifications.share - 0.25).abs() < 0.01);
        layout.open_notifications();
        let placed = columns(&layout.dock, 2000.0);
        let last = placed.last().unwrap();
        assert_eq!(last.0, Pane::Notifications);
        assert!((last.2 - 500.0).abs() < 1.0, "{placed:?}");
    }

    #[test]
    fn notifications_switch_sides() {
        let mut layout = PaneLayout::default();
        layout.sync_notifications();
        draw(&mut layout, 1728.0);
        layout.switch_notifications_side();
        assert!(layout.notifications.open);
        assert_eq!(layout.notifications.side, Side::Right);
        let placed = columns(&layout.dock, 1728.0);
        assert_eq!(placed.last().unwrap().0, Pane::Notifications);
    }

    #[test]
    fn closing_the_notifications_tab_collapses_them_where_they_were() {
        let mut layout = PaneLayout::default();
        layout.notifications.side = Side::Right;
        layout.sync_notifications();
        draw(&mut layout, 1728.0);
        let before = layout.clone();
        let path = layout.dock.find_tab(&Pane::Notifications).unwrap();
        layout.dock.remove_tab(path);
        layout.closed_notifications(&before);
        assert!(!layout.notifications.open);
        assert_eq!(layout.notifications.side, Side::Right);
        layout.sync_notifications();
        assert!(!layout.contains(Pane::Notifications), "stays collapsed");
    }
}

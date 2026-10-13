//! Back/forward over the selected nodes, like a browser's: every new selection is recorded,
//! going back or forward moves through the record, and a new selection after going back drops
//! the forward entries.

use eframe::egui::{self, RichText, Ui};

use crate::model::NodeId;
use crate::ui::icons;

/// How many selections the history keeps; the oldest go first.
pub const MAX_HISTORY: usize = 50;

/// The selections made, oldest first, and which one is current.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionHistory {
    entries: Vec<NodeId>,
    /// Index of the current entry; 0 while `entries` is empty.
    cursor: usize,
}

impl SelectionHistory {
    pub fn current(&self) -> Option<&NodeId> {
        self.entries.get(self.cursor)
    }

    /// Records `selected` as a new entry unless it is the current one (or nothing is
    /// selected). Drops the entries ahead of the current one.
    pub fn record(&mut self, selected: Option<&NodeId>) {
        let Some(id) = selected else {
            return;
        };
        if self.current() == Some(id) {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(id.clone());
        let excess = self.entries.len().saturating_sub(MAX_HISTORY);
        self.entries.drain(..excess);
        self.cursor = self.entries.len() - 1;
    }

    /// Whether [`Self::back`] has somewhere to go from `selected`.
    pub fn can_back(&self, selected: Option<&NodeId>) -> bool {
        self.current()
            .is_some_and(|current| selected != Some(current) || self.cursor > 0)
    }

    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// The node to select going back from `selected`: the previous entry, or the current one
    /// when the selection moved off it (cleared with Esc, or its node closed).
    pub fn back(&mut self, selected: Option<&NodeId>) -> Option<NodeId> {
        if !self.can_back(selected) {
            return None;
        }
        if self.current() == selected {
            self.cursor -= 1;
        }
        self.current().cloned()
    }

    /// The node to select going forward: the next entry.
    pub fn forward(&mut self) -> Option<NodeId> {
        if !self.can_forward() {
            return None;
        }
        self.cursor += 1;
        self.current().cloned()
    }

    /// Drops entries whose node no longer `exists` and merges the neighbours that leaves
    /// equal. A dropped current entry makes the one before it current.
    pub fn prune(&mut self, exists: impl Fn(&NodeId) -> bool) {
        let old = std::mem::take(&mut self.entries);
        let old_cursor = self.cursor;
        self.cursor = 0;
        for (i, id) in old.into_iter().enumerate() {
            if exists(&id) && self.entries.last() != Some(&id) {
                self.entries.push(id);
            }
            if i == old_cursor {
                self.cursor = self.entries.len().saturating_sub(1);
            }
        }
    }
}

/// What the back/forward buttons asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryStep {
    Back,
    Forward,
}

/// The back and forward buttons, each disabled when there is nowhere to go.
pub fn history_buttons(ui: &mut Ui, can_back: bool, can_forward: bool) -> Option<HistoryStep> {
    let family = icons::family(ui.ctx());
    let mut button = |icon: char, enabled: bool, hover: &str| {
        let text = RichText::new(icon.to_string()).family(family.clone());
        ui.add_enabled(enabled, egui::Button::new(text))
            .on_hover_text(hover)
            .on_disabled_hover_text(hover)
            .clicked()
    };
    let back = button(
        icons::ARROW_LEFT,
        can_back,
        "Back to the previous selection — ⌘[",
    );
    let forward = button(icons::ARROW_RIGHT, can_forward, "Forward — ⌘]");
    if back {
        Some(HistoryStep::Back)
    } else if forward {
        Some(HistoryStep::Forward)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> NodeId {
        NodeId(s.to_string())
    }

    fn history(ids: &[&str]) -> SelectionHistory {
        let mut h = SelectionHistory::default();
        for s in ids {
            h.record(Some(&id(s)));
        }
        h
    }

    fn entries(h: &SelectionHistory) -> Vec<&str> {
        h.entries.iter().map(NodeId::as_str).collect()
    }

    #[test]
    fn empty_history_goes_nowhere() {
        let mut h = SelectionHistory::default();
        assert!(!h.can_back(None));
        assert!(!h.can_forward());
        assert_eq!(h.back(None), None);
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn back_and_forward_move_without_recording() {
        let mut h = history(&["a", "king"]);
        assert!(h.can_back(Some(&id("king"))));
        assert!(!h.can_forward());
        assert_eq!(h.back(Some(&id("king"))), Some(id("a")));
        // The app then records the node it selected, which is the current entry.
        h.record(Some(&id("a")));
        assert_eq!(entries(&h), ["a", "king"]);
        assert!(!h.can_back(Some(&id("a"))));
        assert_eq!(h.back(Some(&id("a"))), None);
        assert!(h.can_forward());
        assert_eq!(h.forward(), Some(id("king")));
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn new_selection_after_back_drops_forward_entries() {
        let mut h = history(&["a", "b", "c"]);
        h.back(Some(&id("c")));
        h.back(Some(&id("b")));
        h.record(Some(&id("d")));
        assert_eq!(entries(&h), ["a", "d"]);
        assert!(!h.can_forward());
        assert_eq!(h.current(), Some(&id("d")));
    }

    #[test]
    fn repeated_selection_and_clearing_record_nothing() {
        let mut h = history(&["a", "a"]);
        h.record(None);
        assert_eq!(entries(&h), ["a"]);
    }

    #[test]
    fn back_from_a_cleared_selection_restores_the_current_entry() {
        let mut h = history(&["a", "b"]);
        assert!(h.can_back(None));
        assert_eq!(h.back(None), Some(id("b")));
        assert_eq!(h.back(Some(&id("b"))), Some(id("a")));
    }

    #[test]
    fn history_is_capped_dropping_the_oldest() {
        let names: Vec<String> = (0..MAX_HISTORY + 5).map(|i| format!("n{i}")).collect();
        let mut h = SelectionHistory::default();
        for name in &names {
            h.record(Some(&id(name)));
        }
        assert_eq!(h.entries.len(), MAX_HISTORY);
        assert_eq!(h.entries[0], id("n5"));
        assert_eq!(h.current(), Some(&id(names.last().unwrap())));
    }

    #[test]
    fn prune_drops_gone_nodes_and_merges_duplicates() {
        let mut h = history(&["a", "gone", "a", "b", "gone", "c"]);
        h.back(Some(&id("c")));
        h.back(Some(&id("gone")));
        assert_eq!(h.current(), Some(&id("b")));
        h.prune(|n| n.as_str() != "gone");
        assert_eq!(entries(&h), ["a", "b", "c"]);
        assert_eq!(h.current(), Some(&id("b")));
        assert_eq!(h.forward(), Some(id("c")));
    }

    #[test]
    fn pruning_the_current_entry_makes_the_previous_one_current() {
        let mut h = history(&["a", "b", "closed"]);
        h.prune(|n| n.as_str() != "closed");
        assert_eq!(h.current(), Some(&id("b")));
        // The closed node's selection was cleared, so back reselects b, then a.
        assert_eq!(h.back(None), Some(id("b")));
        assert_eq!(h.back(Some(&id("b"))), Some(id("a")));
    }

    #[test]
    fn pruning_everything_empties_the_history() {
        let mut h = history(&["a", "b"]);
        h.prune(|_| false);
        assert_eq!(h, SelectionHistory::default());
        h.record(Some(&id("c")));
        assert_eq!(entries(&h), ["c"]);
    }
}

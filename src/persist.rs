//! UI state saved between launches through eframe's storage: filters, view, graph layout,
//! selection, the pane arrangement and the command palette's recent actions. Window size and
//! position are saved by eframe itself.

use serde::{Deserialize, Serialize};

use crate::app::View;
use crate::ui::graph::SavedLayout;
use crate::ui::sidebar::FleetFilter;

/// Storage key of [`PersistedState`].
pub const STORAGE_KEY: &str = "myrstack_state";
/// Bumped when the saved format changes incompatibly; other versions are ignored on load.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    pub version: u32,
    pub view: View,
    /// ⌘J hid the Queen's chat shown when nothing is selected.
    pub chat_hidden: bool,
    pub filter: FleetFilter,
    pub layout: SavedLayout,
    /// The command palette's recently run actions, newest first.
    pub recent_actions: Vec<String>,
    /// The dock's pane arrangement, see [`crate::ui::dock::PaneLayout::encode`]; `None` for
    /// the default.
    pub panes: Option<String>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            view: View::default(),
            chat_hidden: false,
            filter: FleetFilter::default(),
            layout: SavedLayout::default(),
            recent_actions: Vec::new(),
            panes: None,
        }
    }
}

impl PersistedState {
    /// The saved state, or `None` if there is none or it is unreadable or from another version.
    pub fn load(storage: &dyn eframe::Storage) -> Option<Self> {
        let state: Self = eframe::get_value(storage, STORAGE_KEY)?;
        (state.version == FORMAT_VERSION).then_some(state)
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, self);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use super::*;
    use crate::model::{NodeId, NodeKind, NodeState};
    use crate::ui::graph::SavedView;

    #[derive(Default)]
    struct MemoryStorage(HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }

    fn sample() -> PersistedState {
        PersistedState {
            version: FORMAT_VERSION,
            view: View::Table,
            chat_hidden: true,
            filter: FleetFilter {
                repos: BTreeSet::from(["myrstack".to_owned()]),
                states: BTreeSet::from([NodeState::NeedsInput, NodeState::Stalled]),
                kinds: BTreeSet::from([NodeKind::Worker]),
                show_loose: false,
                show_released: false,
                text: "hrw".into(),
            },
            layout: SavedLayout {
                positions: [
                    (NodeId::terminal("t1"), [1.5, -2.0]),
                    (NodeId("sub:s:a".into()), [300.0, 40.25]),
                ]
                .into_iter()
                .collect(),
                view: Some(SavedView {
                    zoom: 0.75,
                    pan: [12.0, -8.0],
                }),
                manual_view: true,
                selected: Some(NodeId::terminal("t1")),
            },
            recent_actions: vec!["fit_graph".into(), "open_chat".into()],
            panes: crate::ui::dock::PaneLayout::default().encode(),
        }
    }

    #[test]
    fn round_trips_through_eframe_storage() {
        let mut storage = MemoryStorage::default();
        let state = sample();
        state.save(&mut storage);
        assert_eq!(PersistedState::load(&storage), Some(state));
    }

    #[test]
    fn missing_or_garbage_state_loads_as_none() {
        let mut storage = MemoryStorage::default();
        assert_eq!(PersistedState::load(&storage), None);
        storage.set_string_for_test("not ron at all {");
        assert_eq!(PersistedState::load(&storage), None);
    }

    #[test]
    fn other_versions_are_ignored() {
        let mut storage = MemoryStorage::default();
        PersistedState {
            version: FORMAT_VERSION + 1,
            ..sample()
        }
        .save(&mut storage);
        assert_eq!(PersistedState::load(&storage), None);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let mut storage = MemoryStorage::default();
        storage.set_string_for_test("(version: 1, view: Table)");
        let state = PersistedState::load(&storage).unwrap();
        assert_eq!(state.view, View::Table);
        assert_eq!(state.panes, None);
        assert!(!state.chat_hidden);
        assert_eq!(state.filter, FleetFilter::default());
        assert_eq!(state.layout, SavedLayout::default());
    }

    #[test]
    fn saves_from_before_the_dock_still_load() {
        let mut storage = MemoryStorage::default();
        storage.set_string_for_test(
            "(version: 1, view: Table, details: Collapsed, chat_hidden: true)",
        );
        let state = PersistedState::load(&storage).unwrap();
        assert_eq!(state.view, View::Table);
        assert!(state.chat_hidden);
        assert_eq!(state.panes, None);
    }

    impl MemoryStorage {
        fn set_string_for_test(&mut self, value: &str) {
            self.0.insert(STORAGE_KEY.to_owned(), value.to_owned());
        }
    }
}

//! Parking and waking agents ([`crate::park`]) from the context menus and the control API.

use serde_json::{Value, json};

use super::{MyrstackApp, now_ms};
use crate::control::ControlError;
use crate::control::view::kind_name;
use crate::model::NodeId;

impl MyrstackApp {
    /// Parks `park` and wakes `wake`, as picked in a clan or farm context menu.
    pub(super) fn park_from_menu(&mut self, park: Vec<NodeId>, wake: Vec<NodeId>) {
        for id in &park {
            if let Err(e) = self.set_parked(id, true) {
                log::info!("not parking {id}: {e}");
            }
        }
        for id in &wake {
            let _ = self.set_parked(id, false);
        }
    }

    /// Parks or wakes the node `id`. `Ok(false)` when it already was; an error says why it
    /// can't be parked.
    fn set_parked(&mut self, id: &NodeId, park: bool) -> Result<bool, String> {
        let changed = if park {
            let node = self
                .snapshot
                .as_ref()
                .and_then(|s| s.node(id))
                .ok_or_else(|| format!("no agent {id}"))?;
            self.parking.park(node, now_ms()).map_err(str::to_owned)?
        } else {
            self.parking.unpark(id).is_some()
        };
        if changed {
            log::info!("{} {id}", if park { "parked" } else { "woke" });
            self.overlays_changed = true;
        }
        Ok(changed)
    }

    /// Wakes the agent the API means by `key` if it is parked: a message to a parked agent
    /// wakes it. Returns whether it was parked.
    pub(super) fn wake_for_message(&mut self, key: &str) -> bool {
        let Ok(id) = self.control_node(key).map(|n| n.id.clone()) else {
            return false;
        };
        self.parking.is_parked(&id) && self.set_parked(&id, false).unwrap_or(false)
    }

    /// The control API's `park`: parks (or with `park` false wakes) each agent; one result per
    /// agent, and an error only when none could be resolved.
    pub(super) fn control_park(
        &mut self,
        agents: &[String],
        park: bool,
    ) -> Result<Value, ControlError> {
        let mut results = Vec::new();
        let mut resolved = 0;
        for key in agents {
            let node = match self.control_node(key) {
                Ok(node) => node.clone(),
                Err(e) => {
                    results.push(json!({ "agent": key, "ok": false, "error": e.message }));
                    continue;
                }
            };
            resolved += 1;
            let mut row = json!({
                "agent": key,
                "id": node.id,
                "label": node.label,
                "kind": kind_name(node.kind),
                "worktree": node.worktree_path,
            });
            match self.set_parked(&node.id, park) {
                Ok(changed) => {
                    row["ok"] = json!(true);
                    row["changed"] = json!(changed);
                }
                Err(e) => {
                    row["ok"] = json!(false);
                    row["error"] = json!(e);
                }
            }
            results.push(row);
        }
        if resolved == 0 {
            return Err(ControlError::not_found(
                results
                    .iter()
                    .filter_map(|r| r["error"].as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        Ok(json!({
            "parked": park,
            "results": results,
            "parked_now": self.parking.entries().len(),
        }))
    }
}

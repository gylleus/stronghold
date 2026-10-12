//! Context actions shared by fleet selections.
use super::*;
const PARK_HINT: &str = "Put this agent to sleep in the resting area: out of the live fleet \
     and the whip, its terminal, session and worktree kept to continue later";
const WAKE_HINT: &str = "Bring it back to the live fleet (sending it a message wakes it too)";

/// What a node's context menu asked for this frame.
#[derive(Debug, Default)]
pub struct MenuPick {
    pub close: Vec<NodeId>,
    pub whip: Vec<NodeId>,
    pub park: Vec<NodeId>,
    pub wake: Vec<NodeId>,
}

impl FleetView {
    pub fn node_menu(&self, response: &egui::Response, id: &NodeId) -> MenuPick {
        let Some(node) = self.nodes.get(id) else {
            return MenuPick::default();
        };
        let selection = self.selection();
        let mut picked = Vec::new();
        let mut whip = Vec::new();
        let mut park = Vec::new();
        let mut wake = Vec::new();
        if selection.len() > 1 && selection.contains(&node.id) {
            response.context_menu(|ui| {
                let title = format!("{} selected", selection.len());
                ui.label(egui::RichText::new(title).strong());
                ui.separator();
                let nodes: Vec<&Node> = selection
                    .iter()
                    .filter_map(|id| self.nodes.get(id))
                    .collect();
                let parkable: Vec<NodeId> = nodes
                    .iter()
                    .filter(|n| !n.is_parked() && crate::park::refusal(n).is_none())
                    .map(|n| n.id.clone())
                    .collect();
                let parked: Vec<NodeId> = nodes
                    .iter()
                    .filter(|n| n.is_parked())
                    .map(|n| n.id.clone())
                    .collect();
                if !parkable.is_empty()
                    && ui
                        .button(format!("Park {} in the resting area", parkable.len()))
                        .on_hover_text(PARK_HINT)
                        .clicked()
                {
                    park = parkable;
                    ui.close();
                }
                if !parked.is_empty()
                    && ui
                        .button(format!("Wake {} parked", parked.len()))
                        .on_hover_text(WAKE_HINT)
                        .clicked()
                {
                    wake = parked;
                    ui.close();
                }
                if ui
                    .button(format!("Whip {} selected", selection.len()))
                    .on_hover_text(
                        "Send each selected agent a Myrstack nudge now (the Queen and \
                         subagents are skipped)",
                    )
                    .clicked()
                {
                    whip = selection.iter().cloned().collect();
                    ui.close();
                }
                let close = ui
                    .button(format!("Close {} selected…", selection.len()))
                    .on_hover_text(
                        "Kill the selected sessions in Orca (asks first; the Queen and \
                         subagents are skipped)",
                    );
                if close.clicked() {
                    picked = selection.iter().cloned().collect();
                    ui.close();
                }
            });
            return MenuPick {
                close: picked,
                whip,
                park,
                wake,
            };
        }
        response.context_menu(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(&node.label).strong()).truncate());
            ui.separator();
            let refusal = match node.kind {
                NodeKind::Envoy => None,
                NodeKind::Subagent => Some(crate::close::SUBAGENT_REFUSAL),
                NodeKind::Queen => Some(crate::close::QUEEN_REFUSAL),
                NodeKind::Coordinator
                | NodeKind::Attendant
                | NodeKind::Worker
                | NodeKind::Terminal => None,
            };
            let whip_refusal = match node.kind {
                NodeKind::Envoy => Some("The Envoy listens while idle."),
                NodeKind::Subagent => Some("Subagents have no terminal to nudge."),
                NodeKind::Queen => Some("The Queen is Myrstack itself."),
                NodeKind::Coordinator
                | NodeKind::Attendant
                | NodeKind::Worker
                | NodeKind::Terminal => None,
            };
            if ui
                .add_enabled(whip_refusal.is_none(), egui::Button::new("Whip"))
                .on_hover_text("Send this agent a Myrstack nudge now")
                .on_disabled_hover_text(whip_refusal.unwrap_or_default())
                .clicked()
            {
                whip = vec![node.id.clone()];
                ui.close();
            }
            if node.is_parked() {
                if ui.button("Wake").on_hover_text(WAKE_HINT).clicked() {
                    wake = vec![node.id.clone()];
                    ui.close();
                }
            } else {
                let refusal = crate::park::refusal(node);
                if ui
                    .add_enabled(refusal.is_none(), egui::Button::new("Park"))
                    .on_hover_text(PARK_HINT)
                    .on_disabled_hover_text(refusal.unwrap_or_default())
                    .clicked()
                {
                    park = vec![node.id.clone()];
                    ui.close();
                }
            }
            let close = ui
                .add_enabled(refusal.is_none(), egui::Button::new("Close session…"))
                .on_hover_text("Kill this session in Orca (asks first)")
                .on_disabled_hover_text(refusal.unwrap_or_default());
            if close.clicked() {
                picked = vec![node.id.clone()];
                ui.close();
            }
        });
        MenuPick {
            close: picked,
            whip,
            park,
            wake,
        }
    }
}

//! Feeds the farm the messages agents send each other, and the beams of MCP
//! tool calls; see [`crate::traffic`] and [`crate::traffic::beam`].

use eframe::egui;

use super::StrongholdApp;
use crate::model::{ClanSnapshot, NodeId, NodeKind};
use crate::review::mark::Origin;
use crate::traffic::beam::{Beam, BeamKind, ToolCall};
use crate::traffic::{AgentMessage, Endpoint, MessageKind};
use crate::ui::state_pop::Cue;

/// How long a spawn's beam waits for its new agent to show up in the clan.
const SPAWN_WAIT_S: f64 = 20.0;
/// How long other tool calls wait for their ends (the first clan data, say).
const CALL_WAIT_S: f64 = 3.0;

impl StrongholdApp {
    /// An agent reaching Karl: from the caller at `origin`, else from the node `about`.
    pub(super) fn traffic_to_karl(&self, origin: &Origin, about: Option<&str>, kind: MessageKind) {
        let from = self
            .caller(origin)
            .map(|n| n.id.0.clone())
            .or_else(|| about.map(str::to_owned));
        if let Some(from) = from {
            self.traffic.push(AgentMessage::new(from, "karl", kind));
        }
    }

    /// Hands the messages queued since the last frame to both views (the one not shown drops
    /// them if it isn't opened soon).
    pub(super) fn poll_traffic(&mut self, ctx: &egui::Context) {
        self.poll_tool_calls(ctx);
        let messages = self.traffic.drain();
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let flights: Vec<_> = messages
            .iter()
            .filter_map(|m| {
                let flight = m.resolve(snapshot);
                if flight.is_none() {
                    log::debug!("traffic: {} → {} not in the clan", m.from, m.to);
                }
                flight
            })
            .collect();
        if flights.is_empty() {
            return;
        }
        let now_s = ctx.input(|i| i.time);
        let still = crate::ui::farm::death::reduce_motion();
        for flight in flights {
            log::debug!("traffic: {:?}", flight);
            self.farm.message(flight, now_s, still);
        }
        ctx.request_repaint();
    }
}

impl StrongholdApp {
    /// Pops the agents whose state change just settled in both views, with a soft sound for
    /// the done and the waiting when Karl turned it on.
    pub(super) fn poll_state_pops(&mut self, ctx: &egui::Context) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let now_ms = super::now_ms();
        let cues = self.state_changes.observe(&snapshot.nodes, now_ms);
        if let Some(due) = self.state_changes.next_due(now_ms) {
            ctx.request_repaint_after(due);
        }
        if cues.is_empty() {
            return;
        }
        let now_s = ctx.input(|i| i.time);
        let still = crate::ui::farm::death::reduce_motion();
        for (id, cue) in &cues {
            log::debug!("pop: {id:?} {cue:?}");
            self.farm.pop(id.clone(), *cue, now_s, still);
        }
        self.pop_sound(cues.iter().map(|(_, cue)| *cue));
        ctx.request_repaint();
    }

    /// One sound for a batch of pops, the most urgent cue's.
    fn pop_sound(&mut self, cues: impl Iterator<Item = Cue>) {
        let settings = self.whip_window.notifications();
        if !settings.pop_sounds || settings.quiet {
            return;
        }
        let volume = f32::from(settings.volume_percent.min(100)) / 100.0;
        let cues: Vec<Cue> = cues.collect();
        let Some(name) = [Cue::NeedsInput, Cue::Done]
            .into_iter()
            .filter(|c| cues.contains(c))
            .find_map(Cue::sound)
        else {
            return;
        };
        let Some(path) = crate::notify::Sound::System(name.into()).path() else {
            return;
        };
        if let Err(e) = crate::notify::play_sound(&path, volume) {
            log::warn!("pop sound: {e:#}");
        }
    }

    /// Hands the tool calls whose ends are in the clan to both views as beams; keystrokes go
    /// out as messages. A spawn waits for its new agent; calls that never resolve are dropped.
    fn poll_tool_calls(&mut self, ctx: &egui::Context) {
        if self.traffic.calls.is_empty() {
            return;
        }
        let now_s = ctx.input(|i| i.time);
        let calls = std::mem::take(&mut self.traffic.calls);
        let mut beams = Vec::new();
        for (call, queued_s) in calls {
            let queued_s = queued_s.unwrap_or(now_s);
            let wait = match call.kind {
                BeamKind::Spawn => SPAWN_WAIT_S,
                _ => CALL_WAIT_S,
            };
            match self.resolve_call(&call) {
                Some(Ok(beam)) => beams.push(beam),
                Some(Err(message)) => self.traffic.push(message),
                None if now_s - queued_s < wait => {
                    self.traffic.calls.push((call, Some(queued_s)));
                }
                None => log::debug!("beam: {} {:?} not in the clan", call.tool, call.targets),
            }
        }
        if beams.is_empty() {
            return;
        }
        let still = crate::ui::farm::death::reduce_motion();
        for beam in beams {
            log::debug!("beam: {:?}", beam);
            self.farm.beam(beam, now_s, still);
        }
        ctx.request_repaint();
    }

    /// `call`'s beam, or for a keystroke the message it stands for; `None` until its caller
    /// and at least one target are in the clan.
    fn resolve_call(&self, call: &ToolCall) -> Option<Result<Beam, AgentMessage>> {
        let snapshot = self.snapshot.as_ref()?;
        let from = self.call_origin(&call.origin)?;
        let from_end = crate::traffic::resolve(snapshot, &from)?;
        let mut targets: Vec<Endpoint> = Vec::new();
        if call.kind == BeamKind::Sweep && call.targets.is_empty() {
            targets.extend(clan(snapshot).map(Endpoint::Node));
        }
        for key in &call.targets {
            let end = crate::traffic::resolve(snapshot, key).or_else(|| {
                let node = self.control_node(key).ok()?;
                Some(Endpoint::Node(node.id.clone()))
            });
            if let Some(end) = end.filter(|e| *e != from_end && !targets.contains(e)) {
                targets.push(end);
            }
        }
        if call.kind == BeamKind::Write {
            let Some(Endpoint::Node(to)) = targets.first() else {
                return None;
            };
            let kind = match from.as_str() {
                "king" => MessageKind::Instruction,
                _ => MessageKind::Message,
            };
            return Some(Err(AgentMessage::new(from, to.0.clone(), kind)));
        }
        (!targets.is_empty()).then_some(Ok(Beam {
            from: from_end,
            targets,
            kind: call.kind,
        }))
    }

    /// Who made a call from `origin`: its agent's id, else its terminal handle; the King when
    /// it has no Orca identity (his chat runs outside Orca's terminals).
    fn call_origin(&self, origin: &Origin) -> Option<String> {
        if let Some(node) = self.caller(origin) {
            return Some(node.id.0.clone());
        }
        match (&origin.terminal_handle, &origin.pane_key) {
            (Some(handle), _) => Some(handle.clone()),
            (None, None) => Some("king".to_owned()),
            (None, Some(_)) => None,
        }
    }
}

/// The agents a clan-wide sweep covers: live, awake ones (not the King, not subagents).
fn clan(snapshot: &ClanSnapshot) -> impl Iterator<Item = NodeId> + '_ {
    snapshot
        .nodes
        .iter()
        .filter(|n| n.live && n.parked_at_ms.is_none())
        .filter(|n| !matches!(n.kind, NodeKind::King | NodeKind::Subagent))
        .map(|n| n.id.clone())
}

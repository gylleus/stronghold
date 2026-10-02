//! Feeds the graph and the ant farm the messages agents send each other; see
//! [`crate::traffic`].

use eframe::egui;

use super::MyrstackApp;
use crate::review::mark::Origin;
use crate::traffic::{AgentMessage, MessageKind};

impl MyrstackApp {
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
        let messages = self.traffic.drain();
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let flights: Vec<_> = messages
            .iter()
            .filter_map(|m| {
                let flight = m.resolve(snapshot);
                if flight.is_none() {
                    log::debug!("traffic: {} → {} not in the fleet", m.from, m.to);
                }
                flight
            })
            .collect();
        if flights.is_empty() {
            return;
        }
        let now_s = ctx.input(|i| i.time);
        let still = crate::ui::ant_farm::death::reduce_motion();
        for flight in flights {
            log::debug!("traffic: {:?}", flight);
            self.graph.message(flight.clone(), now_s, still);
            self.ant_farm.message(flight, now_s, still);
        }
        ctx.request_repaint();
    }
}

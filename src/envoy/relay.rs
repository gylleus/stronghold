//! Relays carry attribution and share a durable one-hour budget, including dry runs.

use super::{inbox, secrets};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const RELAY_PREFIX: &str = "Envoy (relayed from Slack, not Karl's instruction):";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayRequest {
    pub to: String,
    pub summary: String,
    pub slack_link: String,
    pub why: String,
}

impl RelayRequest {
    pub fn text(&self) -> String {
        format!(
            "{RELAY_PREFIX} {} | {} | Why: {}",
            self.summary, self.slack_link, self.why
        )
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayRecord {
    pub request: RelayRequest,
    pub at_ms: i64,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelayLimiter {
    records: Vec<RelayRecord>,
}

impl RelayLimiter {
    pub fn records(&self) -> &[RelayRecord] {
        &self.records
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        inbox::load(path).map_err(|e| e.to_string())
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        inbox::save(path, self).map_err(|e| e.to_string())
    }
    pub fn reserve(
        &mut self,
        request: RelayRequest,
        dry_run: bool,
        now_ms: i64,
    ) -> Result<RelayRecord, String> {
        if request.to.trim().is_empty() || request.summary.trim().is_empty() {
            return Err("Relay target and summary are required".into());
        }
        secrets::scan(&request.text())?;
        self.records
            .retain(|r| now_ms.saturating_sub(r.at_ms) < 3_600_000);
        if self
            .records
            .iter()
            .any(|r| r.request.summary.trim() == request.summary.trim())
        {
            return Err("Duplicate relay summary in the last hour".into());
        }
        if self
            .records
            .iter()
            .any(|r| r.request.to == request.to && now_ms.saturating_sub(r.at_ms) < 600_000)
        {
            return Err("Target already received a relay in the last ten minutes".into());
        }
        if self.records.len() >= 6 {
            return Err("Global relay limit: six per hour".into());
        }
        let record = RelayRecord {
            request,
            at_ms: now_ms,
            dry_run,
        };
        self.records.push(record.clone());
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(n: usize) -> RelayRequest {
        RelayRequest {
            to: format!("agent-{n}"),
            summary: format!("Update {n}"),
            slack_link: "https://example.slack.com/archives/C1/p1".into(),
            why: "Relevant task".into(),
        }
    }
    #[test]
    fn target_global_duplicate_and_restart_limits() {
        let mut limiter = RelayLimiter::default();
        for i in 0..6 {
            limiter.reserve(request(i), true, 0).unwrap();
        }
        assert!(
            limiter
                .reserve(request(6), false, 600_000)
                .unwrap_err()
                .contains("Global")
        );
        let mut duplicate = request(0);
        duplicate.to = "other".into();
        assert!(
            limiter
                .reserve(duplicate, true, 1)
                .unwrap_err()
                .contains("Duplicate")
        );
        let mut targeted = request(8);
        targeted.to = "agent-0".into();
        assert!(
            limiter
                .reserve(targeted, true, 1)
                .unwrap_err()
                .contains("Target")
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relays.json");
        limiter.save(&path).unwrap();
        let mut limiter = RelayLimiter::load(&path).unwrap();
        assert_eq!(limiter.records().len(), 6);
        assert!(limiter.reserve(request(6), false, 3_600_000).is_ok());
        assert!(request(0).text().starts_with(RELAY_PREFIX));
    }
}

/// Keep the broadcast routing while replacing the King's authority with the Envoy's.
pub fn attribute_broadcast(plan: &mut crate::broadcast::BroadcastPlan, from: &str, text: &str) {
    for delivery in &mut plan.deliveries {
        delivery.message.from = Some(from.into());
        delivery.message.subject = text
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(100)
            .collect();
        delivery.message.body = Some(text.into());
    }
}

pub fn deliver_broadcast(
    sender: &mut dyn crate::broadcast::BroadcastSender,
    plan: &crate::broadcast::BroadcastPlan,
    traffic: &crate::traffic::TrafficSender,
    from: &str,
) -> serde_json::Value {
    let result = crate::control::commands::deliver_broadcast(sender, plan);
    for delivery in result["deliveries"].as_array().into_iter().flatten() {
        if delivery["ok"] == true
            && let Some(to) = delivery["coordinator"].as_str()
        {
            traffic.send(crate::traffic::AgentMessage::new(
                from,
                to,
                crate::traffic::MessageKind::Relay,
            ));
        }
    }
    result
}

#[cfg(test)]
mod traffic_tests {
    use super::*;
    use crate::broadcast::{BroadcastPlan, BroadcastSender, Delivery};
    use crate::orca::OutgoingMessage;
    use crate::traffic::{AgentMessage, MessageKind, Traffic};
    struct Sender(Vec<OutgoingMessage>);
    impl BroadcastSender for Sender {
        fn send(&mut self, m: &OutgoingMessage) -> Result<Option<String>, String> {
            self.0.push(m.clone());
            if m.to.as_deref() == Some("run:failed") {
                Err("offline".into())
            } else {
                Ok(None)
            }
        }
    }
    #[test]
    fn broadcast_keeps_envoy_attribution_and_draws_only_delivered_relays() {
        let mut plan = BroadcastPlan {
            deliveries: ["ok", "failed"]
                .into_iter()
                .map(|id| Delivery {
                    coordinator_handle: format!("term_{id}"),
                    run_ids: vec![id.into()],
                    message: crate::broadcast::broadcast_message(id, "context"),
                })
                .collect(),
            skipped: vec![],
        };
        let text = format!("{RELAY_PREFIX} Context");
        attribute_broadcast(&mut plan, "term_envoy", &text);
        let mut sender = Sender(Vec::new());
        let traffic = Traffic::default();
        let result = deliver_broadcast(&mut sender, &plan, &traffic.sender(), "term_envoy");
        assert_eq!(result["sent"], 1);
        assert_eq!(result["failed"], 1);
        assert_eq!(
            traffic.drain(),
            [AgentMessage::new(
                "term_envoy",
                "term_ok",
                MessageKind::Relay
            )]
        );
        assert!(
            sender
                .0
                .iter()
                .all(|m| m.from.as_deref() == Some("term_envoy")
                    && m.subject.starts_with(RELAY_PREFIX)
                    && m.body.as_deref() == Some(&text))
        );
    }
}

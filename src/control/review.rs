//! Live PR evidence for reports, read off the UI thread and shared within one request.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::commands::ControlCommand;
use super::protocol::ControlError;
use super::server::{Backend, UNREAD_IN_STATUS, now_ms, status};
use super::view::AlertView;
use crate::review::github::{GhError, GhSource, PrStatus};
use crate::review::{PrRef, ReviewCommand};

pub(super) fn read(backend: &Backend, method: &str) -> Result<Value, ControlError> {
    let queue = backend
        .commands
        .execute(ControlCommand::Review(ReviewCommand::Queue));
    if method == "review_queue" && queue.is_err() {
        return queue;
    }
    let mut queue = queue.unwrap_or_else(|e| json!({"entries": [], "error": e.message}));
    let mut view = (*backend.status.current()).clone();
    let entries = queue["entries"].as_array().cloned().unwrap_or_default();
    let known: BTreeSet<_> = entries
        .iter()
        .filter_map(|e| e["url"].as_str().and_then(PrRef::parse))
        .chain(
            view.alerts
                .iter()
                .filter_map(|a| a.url.as_deref().and_then(PrRef::parse)),
        )
        .collect();
    let mut live = Live::new(backend.github.as_ref());
    let mut refreshed = Vec::new();
    if method != "alerts" {
        for mut entry in entries {
            let evidence = live.get(entry["url"].as_str().and_then(PrRef::parse));
            if matches!(
                evidence["state"].as_str(),
                Some("merged" | "closed" | "draft")
            ) {
                continue;
            }
            entry
                .as_object_mut()
                .expect("review row")
                .extend(evidence.as_object().unwrap().clone());
            refreshed.push(entry);
        }
    }
    queue["count"] = json!(refreshed.len());
    queue["entries"] = json!(refreshed);
    queue["checked_at_ms"] = json!(now_ms());
    queue["github"] = json!({
        "available": live.error.is_none() && queue["error"].is_null(),
        "error": live.error.as_deref().map(Value::from).unwrap_or_else(|| queue["error"].clone()),
    });
    if method != "review_queue" {
        let mut unread = 0;
        for alert in &mut view.alerts {
            if method == "status" {
                if alert.read {
                    continue;
                }
                unread += 1;
                if unread > UNREAD_IN_STATUS {
                    break;
                }
            }
            if alert.source == "review" {
                alert.pr = Some(live.get(alert_pr(alert, &known)));
            }
        }
    }
    match method {
        "review_queue" => Ok(queue),
        "alerts" => Ok(json!({"alerts": view.alerts})),
        _ => {
            let mut result = status(&view);
            for entry in queue["entries"].as_array_mut().unwrap() {
                entry.as_object_mut().unwrap().retain(|key, _| {
                    matches!(
                        key.as_str(),
                        "url"
                            | "title"
                            | "state"
                            | "ci"
                            | "head_sha"
                            | "ankra_on_head"
                            | "marker_on_head"
                            | "fetched_at_ms"
                            | "error"
                    )
                });
            }
            result["review_queue"] = queue;
            Ok(result)
        }
    }
}

/// A bare repo name is only usable when the known URLs identify one owner unambiguously.
fn alert_pr(alert: &AlertView, known: &BTreeSet<PrRef>) -> Option<PrRef> {
    if let Some(pr) = alert.url.as_deref().and_then(PrRef::parse) {
        return Some(pr);
    }
    let mut candidates = BTreeSet::new();
    for word in alert
        .title
        .split(|c: char| !(c.is_ascii_alphanumeric() || "-_./#".contains(c)))
    {
        let Some((repo, number)) = word.split_once('#') else {
            continue;
        };
        let Some(number) = number
            .trim_end_matches('.')
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
        else {
            continue;
        };
        if let Some((owner, repo)) = repo.split_once('/') {
            if let Some(pr) =
                PrRef::parse(&format!("https://github.com/{owner}/{repo}/pull/{number}"))
            {
                candidates.insert(pr);
            }
        } else {
            candidates.extend(known.iter().filter(|pr| pr.repo == repo).map(|pr| PrRef {
                number,
                ..pr.clone()
            }));
        }
    }
    (candidates.len() == 1)
        .then(|| candidates.into_iter().next())
        .flatten()
}

struct Live<'a> {
    source: &'a (dyn GhSource + Sync),
    results: BTreeMap<String, Value>,
    started: Instant,
    error: Option<String>,
    unavailable: bool,
}

impl<'a> Live<'a> {
    fn new(source: &'a (dyn GhSource + Sync)) -> Self {
        Self {
            source,
            results: BTreeMap::new(),
            started: Instant::now(),
            error: None,
            unavailable: false,
        }
    }

    fn get(&mut self, pr: Option<PrRef>) -> Value {
        let Some(pr) = pr else {
            self.error = Some("PR reference missing or ambiguous".into());
            return unknown("PR reference missing or ambiguous");
        };
        let url = pr.url();
        if let Some(evidence) = self.results.get(&url) {
            return evidence.clone();
        }
        // Leave room for one GH_TIMEOUT (45s) before the control client's 200s timeout.
        let result = if self.unavailable || self.started.elapsed() >= Duration::from_secs(90) {
            Err(GhError::Failed(
                "live PR lookup unavailable or report time budget exhausted".into(),
            ))
        } else {
            self.source.view(&url, now_ms())
        };
        let mut evidence = match result {
            Ok(s) => evidence(&s),
            Err(e) => {
                self.unavailable |= matches!(e, GhError::Unavailable(_));
                self.error = Some(e.to_string());
                unknown(&e.to_string())
            }
        };
        evidence["url"] = json!(url);
        self.results.insert(url, evidence.clone());
        evidence
    }
}

fn unknown(error: &str) -> Value {
    json!({"state": null, "ci": null, "head_sha": null, "ankra_verdict": null,
        "ankra_on_head": null, "marker": null, "marker_on_head": null,
        "review_decision": null, "fetched_at_ms": null, "error": error})
}

fn evidence(s: &PrStatus) -> Value {
    json!({"state": s.state, "title": s.title, "ci": s.ci, "head_sha": s.head_sha,
        "ankra_verdict": s.ankra.as_ref().and_then(|a| a.verdict.as_ref()),
        "ankra_on_head": s.ankra_on_head(), "marker": s.marker.as_ref().map(|m| m.kind),
        "marker_on_head": s.marker_on_head(), "review_decision": s.review_decision,
        "fetched_at_ms": s.fetched_at_ms, "error": null})
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::control::server::{ScreenRead, ScreenReader, handle_line};
    use crate::control::view::{AlertView, StatusView};
    use crate::control::{CommandHandler, SharedStatus};
    use crate::review::github::{
        Ankra, GhError, GhSource, Marker, MarkerKind, PrStatus, parse_view,
    };

    const URL: &str = "https://github.com/acme/api/pull/1";

    struct Queue;
    impl CommandHandler for Queue {
        fn execute(&self, _: ControlCommand) -> Result<Value, ControlError> {
            Ok(
                json!({"count": 1, "entries": [{"url": URL, "state": "open", "ci": {"state": "passing", "total": 1}, "head_sha": "123456789"}]}),
            )
        }
    }
    impl ScreenReader for Queue {
        fn read_screen(&self, _: &str) -> Result<ScreenRead, String> {
            unreachable!()
        }
    }

    struct Github(Mutex<Result<&'static str, GhError>>);
    impl GhSource for Github {
        fn view(&self, url: &str, now: i64) -> Result<PrStatus, GhError> {
            let state = self.0.lock().unwrap().clone()?;
            let mut status = parse_view(&json!({"number": 1, "url": url, "state": state, "headRefOid": "abcdef123", "statusCheckRollup": [{"status": "IN_PROGRESS"}]}), now).unwrap();
            status.ankra = Some(Ankra {
                verdict: Some("comments".into()),
                reviewed_sha: Some("123456789".into()),
            });
            status.marker = Some(Marker {
                kind: MarkerKind::Ready,
                note: None,
                sha: Some("abcdef123".into()),
            });
            Ok(status)
        }
    }

    #[test]
    fn alert_references_do_not_guess_an_owner() {
        let mut alert = AlertView {
            title: "PR ready: api#1".into(),
            ..Default::default()
        };
        assert!(alert_pr(&alert, &BTreeSet::new()).is_none());
        let known = BTreeSet::from([PrRef::parse(URL).unwrap()]);
        assert_eq!(alert_pr(&alert, &known), PrRef::parse(URL));
        let mut ambiguous = known;
        ambiguous.insert(PrRef::parse("https://github.com/other/api/pull/2").unwrap());
        assert!(alert_pr(&alert, &ambiguous).is_none());
        alert.title = "PR ready: acme/api#1".into();
        assert_eq!(alert_pr(&alert, &ambiguous), PrRef::parse(URL));
        alert.url = Some("https://github.com/other/api/pull/2/files".into());
        assert_eq!(
            alert_pr(&alert, &ambiguous),
            PrRef::parse("https://github.com/other/api/pull/2")
        );
    }

    #[test]
    fn reports_and_alerts_use_live_state_on_every_call() {
        let github = Arc::new(Github(Mutex::new(Ok("OPEN"))));
        let status = SharedStatus::new();
        status.publish(StatusView {
            alerts: vec![
                AlertView {
                    source: "review".into(),
                    title: "PR ready for review: api#1".into(),
                    url: Some(URL.into()),
                    ..Default::default()
                },
                AlertView {
                    source: "review".into(),
                    title: "wheel fix ready: api#1".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let backend = Backend {
            status,
            commands: Arc::new(Queue),
            screens: Arc::new(Queue),
            github: github.clone(),
        };
        let call = |method| {
            handle_line(&backend, &json!({"v": 1, "method": method}).to_string())
                .into_result()
                .unwrap()
        };
        let first = call("status");
        assert_eq!(first["review_queue"]["entries"][0]["head_sha"], "abcdef123");
        assert_eq!(
            first["review_queue"]["entries"][0]["ci"]["state"],
            "pending"
        );
        assert_eq!(first["review_queue"]["entries"][0]["ankra_on_head"], false);
        assert_eq!(first["review_queue"]["entries"][0]["marker_on_head"], true);
        assert!(
            first["review_queue"]["entries"][0]["fetched_at_ms"]
                .as_i64()
                .unwrap()
                > 0
        );
        for state in ["MERGED", "CLOSED"] {
            *github.0.lock().unwrap() = Ok(state);
            let report = call("status");
            assert_eq!(report["review_queue"]["count"], 0);
            for alert in report["notifications"]["latest_unread"].as_array().unwrap() {
                assert_eq!(alert["pr"]["state"], state.to_lowercase());
            }
            assert_eq!(
                call("alerts")["alerts"][0]["pr"]["state"],
                state.to_lowercase()
            );
            assert_eq!(call("review_queue")["count"], 0);
        }
        *github.0.lock().unwrap() = Err(GhError::Failed("offline".into()));
        let report = call("status");
        assert!(report["review_queue"]["entries"][0]["state"].is_null());
        assert!(report["review_queue"]["entries"][0]["ci"].is_null());
        assert!(report["notifications"]["latest_unread"][0]["pr"]["state"].is_null());
    }
}

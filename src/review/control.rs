//! Review queue methods of the control API: `pr_ready` (an agent marks a PR ready),
//! `review_queue` (read the queue), `review_dismiss` and `review_done`. The app answers them on
//! its UI thread, where the queue lives.

use serde_json::Value;

use crate::control::ControlError;
use crate::control::protocol::Params;
use crate::review::mark::ReadyMark;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewCommand {
    /// Record a mark (its time is when the app parsed it).
    Ready(ReadyMark),
    /// The listed PRs.
    Queue,
    /// Hide a PR until it is marked again.
    Dismiss { url: String },
    /// Hide a PR until it is marked again or pushed to.
    Reviewed { url: String },
}

impl ReviewCommand {
    /// The command for a review method; `None` for other methods.
    pub fn parse(method: &str, params: &Value, now_ms: i64) -> Option<Result<Self, ControlError>> {
        let p = Params::new(params);
        let url = || p.required_str("url").map(str::to_owned);
        Some(match method {
            "pr_ready" => ReadyMark::from_params(params, now_ms).map(Self::Ready),
            "review_queue" => Ok(Self::Queue),
            "review_dismiss" => url().map(|url| Self::Dismiss { url }),
            "review_done" => url().map(|url| Self::Reviewed { url }),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::review::mark::{MarkSource, Origin};

    #[test]
    fn methods_parse_into_commands() {
        let params = json!({
            "url": "https://github.com/depict-org/realtime/pull/255",
            "title": "gate",
            "via": "cli",
            "origin": {"terminal_handle": "term_a", "pane_key": "t:l"},
        });
        let Some(Ok(ReviewCommand::Ready(mark))) = ReviewCommand::parse("pr_ready", &params, 9)
        else {
            panic!("pr_ready parses");
        };
        assert_eq!(mark.pr.number, 255);
        assert_eq!(mark.source, MarkSource::Cli);
        assert_eq!(mark.at_ms, 9);
        assert_eq!(
            mark.origin,
            Origin {
                terminal_handle: Some("term_a".into()),
                pane_key: Some("t:l".into()),
                tab_id: None,
            }
        );
        assert_eq!(
            ReviewCommand::parse("review_queue", &Value::Null, 0),
            Some(Ok(ReviewCommand::Queue))
        );
        assert_eq!(
            ReviewCommand::parse("review_dismiss", &json!({"url": "u"}), 0),
            Some(Ok(ReviewCommand::Dismiss { url: "u".into() }))
        );
        assert_eq!(
            ReviewCommand::parse("review_done", &json!({"url": "u"}), 0),
            Some(Ok(ReviewCommand::Reviewed { url: "u".into() }))
        );
        assert!(matches!(
            ReviewCommand::parse("review_done", &json!({}), 0),
            Some(Err(_))
        ));
        assert!(matches!(
            ReviewCommand::parse("pr_ready", &json!({"url": "nope"}), 0),
            Some(Err(_))
        ));
        assert_eq!(ReviewCommand::parse("status", &Value::Null, 0), None);
        let control = crate::control::ControlCommand::parse("review_queue", &Value::Null);
        assert_eq!(
            control,
            Some(Ok(crate::control::ControlCommand::Review(
                ReviewCommand::Queue
            )))
        );
    }
}

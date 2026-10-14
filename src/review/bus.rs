//! `merge_ready` bus messages as ready marks: a worker's hand-off names its PR URL in the
//! subject, body or payload, and the sender's terminal is the agent.

use crate::orca::{InboxMessage, MessageType};
use crate::review::mark::{MarkSource, Origin, ReadyMark};
use crate::review::pr::find_prs;

/// `merge_ready` messages older than this are history, not news, and are ignored.
pub const MAX_AGE_MS: i64 = 3 * 24 * 3_600_000;
/// Longest subject kept as a mark's summary.
const SUMMARY_CHARS: usize = 200;

/// One mark per PR URL in each recent `merge_ready` message of `messages`.
pub fn marks_from_inbox(messages: &[InboxMessage], now_ms: i64) -> Vec<ReadyMark> {
    let mut marks = Vec::new();
    for m in messages {
        if m.message_type != Some(MessageType::MergeReady) {
            continue;
        }
        let Some(at_ms) = m.created_at.as_ref().and_then(|t| t.epoch_ms()) else {
            continue;
        };
        if now_ms - at_ms > MAX_AGE_MS {
            continue;
        }
        let text = [
            m.subject.as_deref(),
            m.body.as_deref(),
            m.payload.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        let origin = Origin {
            terminal_handle: m.from_handle.clone().filter(|h| !h.is_empty()),
            pane_key: m.sender_pane_key.clone().filter(|k| !k.is_empty()),
            tab_id: None,
        };
        let summary = m
            .subject
            .as_deref()
            .map(|s| s.chars().take(SUMMARY_CHARS).collect::<String>());
        for pr in find_prs(&text) {
            marks.push(ReadyMark {
                pr,
                title: None,
                summary: summary.clone(),
                origin: origin.clone(),
                source: MarkSource::Bus {
                    message_id: m.id.clone(),
                },
                at_ms,
            });
        }
    }
    marks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::commands::parse_inbox;

    fn fixture() -> Vec<InboxMessage> {
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/orca/inbox_merge_ready.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        parse_inbox(text.as_bytes()).unwrap()
    }

    #[test]
    fn merge_ready_messages_become_marks_from_their_sender() {
        let messages = fixture();
        let created = messages[0].created_at.as_ref().unwrap().epoch_ms().unwrap();
        let marks = marks_from_inbox(&messages, created + 60_000);
        assert_eq!(marks.len(), 2, "the heartbeat is skipped");
        let m = &marks[0];
        assert_eq!(m.pr.url(), "https://github.com/acme/storefront/pull/249");
        assert_eq!(
            m.origin.terminal_handle.as_deref(),
            Some("term_442fcb9f-3706-481e-a4c9-acc9707bc9e6")
        );
        assert_eq!(
            m.origin.pane_key.as_deref(),
            Some("16b3a609-4e45-473e-8591-d16268efecb6:e499ad8b-3ccd-4702-87d8-443423aad46f")
        );
        assert_eq!(
            m.source,
            MarkSource::Bus {
                message_id: "msg_93fb229edbe8".into()
            }
        );
        assert_eq!(m.at_ms, created);
        assert_eq!(
            m.summary.as_deref(),
            Some("PR #249 green: empty 'changes' review counts as pass")
        );
        assert_eq!(m.title, None);
        assert_eq!(marks[1].pr.number, 248);
    }

    #[test]
    fn old_messages_and_messages_without_urls_are_skipped() {
        let messages = fixture();
        let created = messages[0].created_at.as_ref().unwrap().epoch_ms().unwrap();
        assert!(marks_from_inbox(&messages, created + MAX_AGE_MS + 1).is_empty());

        let mut m = messages[0].clone();
        m.body = Some("cp-twfn.27 #230: signed, rebased on #232, green".into());
        m.subject = None;
        assert!(marks_from_inbox(&[m.clone()], created).is_empty());
        m.payload = Some(
            r#"{"taskId":"t","prUrl":"https://github.com/o/r/pull/9","also":"https://github.com/o/r/pull/10"}"#
                .into(),
        );
        let marks = marks_from_inbox(&[m], created);
        let numbers: Vec<u64> = marks.iter().map(|m| m.pr.number).collect();
        assert_eq!(numbers, [9, 10], "every PR in the payload");
    }
}

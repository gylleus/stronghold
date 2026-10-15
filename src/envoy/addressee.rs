//! Who an inbound message is for: the user, someone else, or a call for the Envoy's judgement.
//!
//! A Slack mention is `<@U…>` (or `<@U…|name>`); a bare `@name` notifies nobody but still names
//! an addressee, which is how coworkers refer to their agents in text. `<!here>`, `<!channel>`
//! and `<!everyone>` are broadcasts, not addressees. Cached groups and thread ownership also
//! identify messages for the user.

use super::channel::{EventKind, InboundEvent};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressee {
    /// A DM, a member/group mention, or a message in the user's thread.
    User,
    /// Someone else is tagged and the user is not; not for the user unless the Envoy has a strong reason.
    Other,
    /// An unaddressed channel message or a bare name.
    Unclear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub addressee: Addressee,
    pub why: String,
}

impl Verdict {
    /// ` — addressee: the user (a DM)`, appended to an event line.
    pub fn annotation(&self) -> String {
        let who = match self.addressee {
            Addressee::User => crate::user::name(),
            Addressee::Other => crate::user::fill("not {user}"),
            Addressee::Unclear => "unclear".into(),
        };
        format!(" — addressee: {who} ({})", self.why)
    }
}

/// The addressees `text` names, in order: `<@U…>` ids, `<!subteam^…>` groups and bare `@name`s,
/// each once, broadcasts left out.
pub fn mentions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |m: String| {
        if !out.contains(&m) {
            out.push(m);
        }
    };
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        if let Some(inner) = rest.strip_prefix("<@")
            && let Some(end) = inner.find('>')
        {
            let id = inner[..end].split('|').next().unwrap_or_default().trim();
            if !id.is_empty() {
                push(format!("<@{}>", id.to_ascii_uppercase()));
            }
            i += 2 + end + 1;
            continue;
        }
        if let Some(inner) = rest.strip_prefix("<!subteam^")
            && let Some(end) = inner.find('>')
        {
            let id = inner[..end].split('|').next().unwrap_or_default().trim();
            if !id.is_empty() {
                push(format!("<!subteam^{}>", id.to_ascii_uppercase()));
            }
            i += 10 + end + 1;
            continue;
        }
        if rest.starts_with('@')
            && text[..i].chars().next_back().is_none_or(|c| {
                c.is_whitespace() || matches!(c, '(' | ',' | ';' | ':' | '"' | '\'')
            })
        {
            let name: String = rest[1..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
                .collect();
            let name = name.trim_end_matches(['.', '-']);
            if !name.is_empty() && !is_broadcast(name) {
                push(format!("@{}", name.to_lowercase()));
            }
            i += 1 + name.len().max(1);
            continue;
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    out
}

fn is_broadcast(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "here" | "channel" | "everyone" | "all"
    )
}

/// Whether `text` mentions the user's member id.
pub fn mentions_user(text: &str, user_id: &str) -> bool {
    let tag = format!("<@{}>", user_id.to_ascii_uppercase());
    mentions(text).iter().any(|m| m == &tag)
}

pub fn mentions_member(text: &str, user_id: &str, groups: &BTreeSet<String>) -> bool {
    mentions_user(text, user_id)
        || mentions(text).iter().any(|tag| {
            tag.strip_prefix("<!subteam^")
                .and_then(|s| s.strip_suffix('>'))
                .is_some_and(|id| groups.contains(id))
        })
}

/// A bare `@name…` names the user without notifying them: a hint, never proof.
fn names_user(mention: &str) -> bool {
    let name = crate::user::name().to_lowercase();
    !name.contains(' ') && mention.to_lowercase().starts_with(&format!("@{name}"))
}

/// Who `event` is for; `users_thread` supplies ownership absent from inbox posts.
pub fn classify(
    event: &InboundEvent,
    thread: &[&InboundEvent],
    user_id: &str,
    users_thread: bool,
    groups: &BTreeSet<String>,
) -> Verdict {
    if event.kind == EventKind::Dm {
        return Verdict {
            addressee: Addressee::User,
            why: "a DM".into(),
        };
    }
    if mentions_member(&event.text, user_id, groups) {
        return Verdict {
            addressee: Addressee::User,
            why: crate::user::fill("mentions {user}"),
        };
    }
    if users_thread
        || thread
            .iter()
            .any(|e| e.kind == EventKind::Dm || mentions_member(&e.text, user_id, groups))
    {
        return Verdict {
            addressee: Addressee::User,
            why: crate::user::fill("in {user}'s thread"),
        };
    }
    let (named_user, others): (Vec<String>, Vec<String>) = mentions(&event.text)
        .into_iter()
        .partition(|m| names_user(m));
    if others.is_empty() {
        return Verdict {
            addressee: Addressee::Unclear,
            why: if named_user.is_empty() {
                "no mention".into()
            } else {
                format!(
                    "a bare @{}, not a real mention",
                    crate::user::name().to_lowercase()
                )
            },
        };
    }
    let tags = format!("tags {}", list(&others));
    if !named_user.is_empty() {
        return Verdict {
            addressee: Addressee::Unclear,
            why: format!("{tags} and a bare @{}", crate::user::name().to_lowercase()),
        };
    }
    Verdict {
        addressee: Addressee::Other,
        why: format!("{tags}, not {user}", user = crate::user::name()),
    }
}

fn list(mentions: &[String]) -> String {
    const MAX: usize = 3;
    let mut shown: Vec<&str> = mentions.iter().take(MAX).map(String::as_str).collect();
    let more = mentions.len().saturating_sub(MAX);
    let tail;
    if more > 0 {
        tail = format!("+{more} more");
        shown.push(&tail);
    }
    shown.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const KARL: &str = "U000KARL001";
    const COWORKER: &str = "U000COWORK1";

    fn event(kind: EventKind, thread: Option<&str>, text: &str) -> InboundEvent {
        InboundEvent {
            channel_kind: "slack".into(),
            conversation: "C000AGENT01".into(),
            ts: "1791300000.000002".into(),
            thread_ts: thread.map(str::to_owned),
            user: "UBOT".into(),
            user_name: "sams-agent".into(),
            text: text.into(),
            kind,
            permalink: None,
        }
    }

    fn judge(text: &str) -> Verdict {
        classify(
            &event(EventKind::AgentComms, None, text),
            &[],
            KARL,
            false,
            &BTreeSet::new(),
        )
    }

    #[test]
    fn mentions_are_ids_groups_and_bare_names_without_broadcasts() {
        let text = "<@U000COWORK1|Sam> and <@u000karl001>: @search-bot, @Alex. \
                    <!here> <!channel> @channel <!subteam^S123|@eng> mail karl@example.com \
                    <@U000COWORK1> again";
        assert_eq!(
            mentions(text),
            vec![
                "<@U000COWORK1>",
                "<@U000KARL001>",
                "@search-bot",
                "@alex",
                "<!subteam^S123>",
            ]
        );
        assert!(mentions_user("ping <@u000karl001|karl>", KARL));
        assert!(!mentions_user("ping @karl", KARL));
    }

    #[test]
    fn someone_else_tagged_is_not_for_karl() {
        let v = judge("<@U000COWORK1> can your search agent reindex wheat?");
        assert_eq!(v.addressee, Addressee::Other);
        assert_eq!(v.why, "tags <@U000COWORK1>, not Karl");
        let v = judge("@search-bot please rerun the eval");
        assert_eq!(v.addressee, Addressee::Other);
        assert_eq!(v.why, "tags @search-bot, not Karl");
        assert_eq!(
            v.annotation(),
            " — addressee: not Karl (tags @search-bot, not Karl)"
        );
    }

    #[test]
    fn karl_tagged_is_for_karl() {
        let v = judge("<@U000KARL001> is api#694 ready?");
        assert_eq!(v.addressee, Addressee::User);
        assert_eq!(v.annotation(), " — addressee: Karl (mentions Karl)");
        let v = classify(
            &event(EventKind::Dm, None, "hey"),
            &[],
            KARL,
            false,
            &BTreeSet::new(),
        );
        assert_eq!(v.addressee, Addressee::User);
        assert_eq!(v.why, "a DM");
    }

    #[test]
    fn both_tagged_is_for_karl() {
        let v = judge("<@U000COWORK1> <@U000KARL001> can you two sync on the wheat preload?");
        assert_eq!(v.addressee, Addressee::User);
        let v = judge("@search-agent cc <@u000karl001>");
        assert_eq!(v.addressee, Addressee::User);
    }

    #[test]
    fn reply_in_karls_thread_tagging_another_agent_is_judged_by_context() {
        let text = format!("<@{COWORKER}> your agent should take this one");
        let reply = event(EventKind::ThreadReply, Some("1791300000.000001"), &text);
        let app_knows = classify(&reply, &[], KARL, true, &BTreeSet::new());
        assert_eq!(app_knows.addressee, Addressee::User);
        assert_eq!(app_knows.why, "in Karl's thread");
        let root = event(
            EventKind::Mention,
            None,
            "<@U000KARL001> who owns the wheat preload?",
        );
        let inbox_knows = classify(&reply, &[&root], KARL, false, &BTreeSet::new());
        assert_eq!(inbox_knows.addressee, Addressee::User);
        let strangers = classify(
            &reply,
            &[&event(EventKind::AgentComms, None, "hi")],
            KARL,
            false,
            &BTreeSet::new(),
        );
        assert_eq!(strangers.addressee, Addressee::Other);
    }

    #[test]
    fn no_mention_stays_a_judgement_call() {
        let v = judge("merged the recs rollout, deploying now");
        assert_eq!(v.addressee, Addressee::Unclear);
        assert_eq!(v.annotation(), " — addressee: unclear (no mention)");
        let v = judge("<!here> standup in 5");
        assert_eq!(v.why, "no mention");
    }

    #[test]
    fn a_bare_at_karl_is_a_hint_not_a_mention() {
        let v = judge("@karl is this yours?");
        assert_eq!(v.addressee, Addressee::Unclear);
        assert_eq!(v.why, "a bare @karl, not a real mention");
        let v = judge("<@U000COWORK1> and @Karl please look");
        assert_eq!(v.addressee, Addressee::Unclear);
        assert_eq!(v.why, "tags <@U000COWORK1> and a bare @karl");
    }

    #[test]
    fn many_tags_are_abbreviated() {
        let v = judge("<@U1> <@U2> <@U3> <@U4> <@U5> all hands");
        assert_eq!(v.why, "tags <@U1>, <@U2>, <@U3>, +2 more, not Karl");
    }
}

//! Who an inbound message is for: Karl, someone else, or a call for the Envoy's judgement.
//!
//! A Slack mention is `<@U…>` (or `<@U…|name>`); a bare `@name` notifies nobody but still names
//! an addressee, which is how coworkers refer to their agents in text. `<!here>`, `<!channel>`
//! and `<!everyone>` are broadcasts, not addressees. Karl is known by his member id only.

use super::channel::{EventKind, InboundEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressee {
    /// A DM to Karl, or a message that mentions his id.
    Karl,
    /// Someone else is tagged and Karl is not; not for Karl unless the Envoy has a strong reason.
    Other,
    /// Nobody is tagged, or someone else is tagged in a thread Karl's side owns.
    Unclear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub addressee: Addressee,
    pub why: String,
}

impl Verdict {
    /// ` — addressee: Karl (a DM)`, appended to an event line.
    pub fn annotation(&self) -> String {
        let who = match self.addressee {
            Addressee::Karl => "Karl",
            Addressee::Other => "not Karl",
            Addressee::Unclear => "unclear",
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

/// Whether `text` mentions Karl's member id.
pub fn mentions_karl(text: &str, karl: &str) -> bool {
    let karl = format!("<@{}>", karl.to_ascii_uppercase());
    mentions(text).iter().any(|m| m == &karl)
}

/// A bare `@karl…` names Karl without notifying him: a hint, never proof.
fn names_karl(mention: &str) -> bool {
    mention.starts_with("@karl")
}

/// Who `event` is for. `thread` holds the other inbox events in its thread (Karl's own posts
/// never reach the inbox, so a thread he started is known only when its root mentioned him).
/// `karls_thread` says the app knows Karl's side owns the thread: a dwarf's `reach_out` thread,
/// a watched thread, or one the Envoy replied in.
pub fn classify(
    event: &InboundEvent,
    thread: &[&InboundEvent],
    karl: &str,
    karls_thread: bool,
) -> Verdict {
    if event.kind == EventKind::Dm {
        return Verdict {
            addressee: Addressee::Karl,
            why: "a DM".into(),
        };
    }
    if mentions_karl(&event.text, karl) {
        return Verdict {
            addressee: Addressee::Karl,
            why: "mentions Karl".into(),
        };
    }
    let (named_karl, others): (Vec<String>, Vec<String>) = mentions(&event.text)
        .into_iter()
        .partition(|m| names_karl(m));
    if others.is_empty() {
        return Verdict {
            addressee: Addressee::Unclear,
            why: if named_karl.is_empty() {
                "no mention".into()
            } else {
                "a bare @karl, not a real mention".into()
            },
        };
    }
    let tags = format!("tags {}", list(&others));
    if !named_karl.is_empty() {
        return Verdict {
            addressee: Addressee::Unclear,
            why: format!("{tags} and a bare @karl"),
        };
    }
    let in_karls_thread = karls_thread
        || thread
            .iter()
            .any(|e| e.kind == EventKind::Dm || mentions_karl(&e.text, karl));
    if in_karls_thread {
        Verdict {
            addressee: Addressee::Unclear,
            why: format!("{tags} in Karl's thread"),
        }
    } else {
        Verdict {
            addressee: Addressee::Other,
            why: format!("{tags}, not Karl"),
        }
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

    const KARL: &str = "U0AMMLGSFL0";
    const DANIEL: &str = "U0177NR3GG4";

    fn event(kind: EventKind, thread: Option<&str>, text: &str) -> InboundEvent {
        InboundEvent {
            channel_kind: "slack".into(),
            conversation: "C0BHZLG3X63".into(),
            ts: "1791300000.000002".into(),
            thread_ts: thread.map(str::to_owned),
            user: "UBOT".into(),
            user_name: "daniels-agent".into(),
            text: text.into(),
            kind,
            permalink: None,
        }
    }

    fn judge(text: &str) -> Verdict {
        classify(&event(EventKind::AgentComms, None, text), &[], KARL, false)
    }

    #[test]
    fn mentions_are_ids_groups_and_bare_names_without_broadcasts() {
        let text = "<@U0177NR3GG4|Daniel> and <@u0ammlgsfl0>: @depict-search-agent, @Albin. \
                    <!here> <!channel> @channel <!subteam^S123|@eng> mail karl@depict.ai \
                    <@U0177NR3GG4> again";
        assert_eq!(
            mentions(text),
            vec![
                "<@U0177NR3GG4>",
                "<@U0AMMLGSFL0>",
                "@depict-search-agent",
                "@albin",
                "<!subteam^S123>",
            ]
        );
        assert!(mentions_karl("ping <@u0ammlgsfl0|karl>", KARL));
        assert!(!mentions_karl("ping @karl", KARL));
    }

    #[test]
    fn someone_else_tagged_is_not_for_karl() {
        let v = judge("<@U0177NR3GG4> can your search agent reindex wheat?");
        assert_eq!(v.addressee, Addressee::Other);
        assert_eq!(v.why, "tags <@U0177NR3GG4>, not Karl");
        let v = judge("@depict-search-agent please rerun the eval");
        assert_eq!(v.addressee, Addressee::Other);
        assert_eq!(v.why, "tags @depict-search-agent, not Karl");
        assert_eq!(
            v.annotation(),
            " — addressee: not Karl (tags @depict-search-agent, not Karl)"
        );
    }

    #[test]
    fn karl_tagged_is_for_karl() {
        let v = judge("<@U0AMMLGSFL0> is realtime#694 ready?");
        assert_eq!(v.addressee, Addressee::Karl);
        assert_eq!(v.annotation(), " — addressee: Karl (mentions Karl)");
        let v = classify(&event(EventKind::Dm, None, "hey"), &[], KARL, false);
        assert_eq!(v.addressee, Addressee::Karl);
        assert_eq!(v.why, "a DM");
    }

    #[test]
    fn both_tagged_is_for_karl() {
        let v = judge("<@U0177NR3GG4> <@U0AMMLGSFL0> can you two sync on the wheat preload?");
        assert_eq!(v.addressee, Addressee::Karl);
        let v = judge("@search-agent cc <@u0ammlgsfl0>");
        assert_eq!(v.addressee, Addressee::Karl);
    }

    #[test]
    fn reply_in_karls_thread_tagging_another_agent_is_judged_by_context() {
        let text = format!("<@{DANIEL}> your agent should take this one");
        let reply = event(EventKind::ThreadReply, Some("1791300000.000001"), &text);
        let app_knows = classify(&reply, &[], KARL, true);
        assert_eq!(app_knows.addressee, Addressee::Unclear);
        assert_eq!(app_knows.why, "tags <@U0177NR3GG4> in Karl's thread");
        let root = event(
            EventKind::Mention,
            None,
            "<@U0AMMLGSFL0> who owns the wheat preload?",
        );
        let inbox_knows = classify(&reply, &[&root], KARL, false);
        assert_eq!(inbox_knows.addressee, Addressee::Unclear);
        let strangers = classify(
            &reply,
            &[&event(EventKind::AgentComms, None, "hi")],
            KARL,
            false,
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
        let v = judge("<@U0177NR3GG4> and @Karl please look");
        assert_eq!(v.addressee, Addressee::Unclear);
        assert_eq!(v.why, "tags <@U0177NR3GG4> and a bare @karl");
    }

    #[test]
    fn many_tags_are_abbreviated() {
        let v = judge("<@U1> <@U2> <@U3> <@U4> <@U5> all hands");
        assert_eq!(v.why, "tags <@U1>, <@U2>, <@U3>, +2 more, not Karl");
    }
}

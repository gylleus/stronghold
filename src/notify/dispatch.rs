//! The central dispatcher every notification goes through, whatever raised it: an agent's
//! `notify` tool call, a node entering NeedsInput, a whip escalation, a PR marked ready.
//!
//! [`NotificationCenter::dispatch`] drops repeats (a per-target cooldown for Myrstack's own
//! notifications), collapses an agent's identical notifications into one inbox entry, rate
//! limits each agent, records what passes in the [`History`] and decides from the
//! [`NotifySettings`] how it is delivered (macOS pop-up, in-app pop-up, sound). It is pure
//! state; the app feeds it and carries out the [`Delivery`].

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::history::{Entry, History};
use super::settings::{Delivery, NotifySettings, Severity};
use crate::review::PrRef;

/// Myrstack's own notifications about one target (needs input, stalled, PR ready) are sent at
/// most once in this window.
pub const PER_TARGET_COOLDOWN_MS: i64 = 10 * 60_000;
/// Window of the per-agent limit.
pub const AGENT_WINDOW_MS: i64 = 10 * 60_000;
/// At most this many notifications per agent per [`AGENT_WINDOW_MS`].
pub const AGENT_MAX: usize = 5;
/// An agent's notification identical to one it sent this recently is collapsed into it.
pub const DUPLICATE_WINDOW_MS: i64 = 30 * 60_000;

/// What raised a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// An Envoy draft or channel-health transition.
    Envoy,
    /// An agent's `notify` tool call (or `myrstack notify-karl`, the control API's `notify`).
    Agent,
    /// A node entered NeedsInput, or the whip saw it wait too long.
    NeedsInput,
    /// The whip escalated a stall.
    Escalation,
    /// A PR was marked ready for review.
    Review,
    /// The Settings window's "Send test".
    Test,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Agent => "agent",
            Source::Envoy => "envoy",
            Source::NeedsInput => "needs_input",
            Source::Escalation => "escalation",
            Source::Review => "review",
            Source::Test => "test",
        }
    }

    /// Myrstack's own notifications, which repeat per target and use the cooldown.
    fn has_cooldown(self) -> bool {
        matches!(
            self,
            Source::NeedsInput | Source::Escalation | Source::Review
        )
    }
}

/// A notification someone asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub source: Source,
    pub severity: Severity,
    pub title: String,
    pub message: String,
    /// A link to open when Karl clicks it.
    pub url: Option<String>,
    /// What a click selects: a node id, terminal handle or PR URL; the cooldown keys on it.
    pub target: Option<String>,
    /// Who raised it (an agent's terminal handle or pane key): the per-agent limit and
    /// duplicates key on it.
    pub sender: Option<String>,
    /// The agent's display name.
    pub agent_label: Option<String>,
}

impl Incoming {
    pub fn new(source: Source, severity: Severity, title: &str, message: &str) -> Self {
        Self {
            source,
            severity,
            title: title.to_owned(),
            message: message.to_owned(),
            url: None,
            target: None,
            sender: None,
            agent_label: None,
        }
    }

    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// The per-agent limit's key.
    fn sender_key(&self) -> &str {
        self.sender
            .as_deref()
            .or(self.target.as_deref())
            .unwrap_or("anonymous")
    }
}

/// What became of a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Recorded as inbox entry `id` and delivered as `delivery` (which may be silent: below
    /// the thresholds or quiet mode).
    Delivered { id: u64, delivery: Delivery },
    /// Identical to entry `id` sent recently: that entry now counts `count` and is unread
    /// again, and nothing pops up or plays.
    Collapsed { id: u64, count: u32 },
    /// An agent's handover about a PR Myrstack just announced as ready for review: its text
    /// replaced that entry's (`id`), which is unread again, and nothing pops up or plays.
    Folded { id: u64 },
    /// The sender hit the per-agent limit; nothing was recorded.
    RateLimited { retry_after_ms: i64 },
    /// The same target was notified within the cooldown; nothing was recorded.
    Cooldown,
}

impl Outcome {
    /// The inbox entry it ended up in, if any.
    pub fn id(&self) -> Option<u64> {
        match self {
            Outcome::Delivered { id, .. }
            | Outcome::Collapsed { id, .. }
            | Outcome::Folded { id } => Some(*id),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Outcome::Delivered { .. } => "delivered",
            Outcome::Collapsed { .. } => "collapsed",
            Outcome::Folded { .. } => "folded",
            Outcome::RateLimited { .. } => "rate_limited",
            Outcome::Cooldown => "cooldown",
        }
    }
}

/// The dispatcher and the inbox it records into.
#[derive(Debug, Default)]
pub struct NotificationCenter {
    pub history: History,
    /// When each target last passed the cooldown.
    cooldowns: HashMap<String, i64>,
    /// Accepted times per sender inside [`AGENT_WINDOW_MS`].
    sent_by: HashMap<String, VecDeque<i64>>,
}

impl NotificationCenter {
    pub fn new(history: History) -> Self {
        Self {
            history,
            ..Self::default()
        }
    }

    /// Runs `n` through the cooldown, duplicates and the per-agent limit, records it and
    /// decides its delivery.
    pub fn dispatch(&mut self, n: Incoming, settings: &NotifySettings, now_ms: i64) -> Outcome {
        // One notification per PR handover: Myrstack's "PR ready for review" and the agent's
        // own notify about the same PR become one entry, whichever comes first.
        if n.source == Source::Review
            && self
                .recent_about_pr(Source::Agent, n.target.as_deref(), now_ms)
                .is_some()
        {
            return Outcome::Cooldown;
        }
        if n.source == Source::Agent
            && let Some(id) = self.recent_about_pr(Source::Review, n.url.as_deref(), now_ms)
        {
            let entry = self.history.get_mut(id).expect("just found");
            entry.severity = entry.severity.max(n.severity);
            entry.title = n.title;
            entry.message = n.message;
            entry.url = n.url;
            entry.sender = n.sender;
            if n.agent_label.is_some() {
                entry.agent_label = n.agent_label;
            }
            entry.count += 1;
            entry.at_ms = now_ms;
            entry.read = false;
            return Outcome::Folded { id };
        }
        if n.source.has_cooldown() {
            let key = n.target.clone().unwrap_or_else(|| n.title.clone());
            if let Some(&last) = self.cooldowns.get(&key)
                && now_ms - last < PER_TARGET_COOLDOWN_MS
            {
                return Outcome::Cooldown;
            }
            self.cooldowns.insert(key, now_ms);
            self.cooldowns
                .retain(|_, &mut at| now_ms - at < PER_TARGET_COOLDOWN_MS);
        }
        if n.source == Source::Agent {
            if let Some(id) = self.duplicate_of(&n, now_ms) {
                let entry = self.history.get_mut(id).expect("just found");
                entry.count += 1;
                entry.at_ms = now_ms;
                entry.read = false;
                return Outcome::Collapsed {
                    id,
                    count: entry.count,
                };
            }
            let sent = self.sent_by.entry(n.sender_key().to_owned()).or_default();
            while sent.front().is_some_and(|&t| now_ms - t >= AGENT_WINDOW_MS) {
                sent.pop_front();
            }
            if sent.len() >= AGENT_MAX {
                let oldest = sent.front().copied().unwrap_or(now_ms);
                return Outcome::RateLimited {
                    retry_after_ms: (oldest + AGENT_WINDOW_MS - now_ms).max(1),
                };
            }
            sent.push_back(now_ms);
            self.sent_by.retain(|_, times| !times.is_empty());
        }
        let delivery = settings.delivery(n.severity);
        let id = self.history.push(Entry {
            id: 0,
            at_ms: now_ms,
            first_at_ms: now_ms,
            source: n.source,
            severity: n.severity,
            title: n.title,
            message: n.message,
            url: n.url,
            target: n.target,
            sender: n.sender,
            agent_label: n.agent_label,
            count: 1,
            read: false,
        });
        Outcome::Delivered { id, delivery }
    }

    /// Starts the cooldown of `target` as if Myrstack had just notified it, so the
    /// needs-input notification that follows an agent's own question doesn't repeat it.
    pub fn hold_cooldown(&mut self, target: &str, now_ms: i64) {
        self.cooldowns.insert(target.to_owned(), now_ms);
    }

    /// A `source` entry from the last [`PER_TARGET_COOLDOWN_MS`] about the PR `link` names
    /// (a Review entry's target, an Agent entry's url).
    fn recent_about_pr(&self, source: Source, link: Option<&str>, now_ms: i64) -> Option<u64> {
        let pr = PrRef::parse(link?)?;
        self.history
            .entries()
            .iter()
            .rev()
            .take_while(|e| now_ms - e.at_ms < PER_TARGET_COOLDOWN_MS)
            .find(|e| {
                let their = match source {
                    Source::Review => e.target.as_deref(),
                    _ => e.url.as_deref(),
                };
                e.source == source && their.and_then(PrRef::parse).as_ref() == Some(&pr)
            })
            .map(|e| e.id)
    }

    /// A recent entry from the same sender with the same severity, title, message and link.
    fn duplicate_of(&self, n: &Incoming, now_ms: i64) -> Option<u64> {
        self.history
            .entries()
            .iter()
            .rev()
            .take_while(|e| now_ms - e.at_ms < DUPLICATE_WINDOW_MS)
            .find(|e| {
                e.source == n.source
                    && e.sender.as_deref() == Some(n.sender_key())
                    && e.severity == n.severity
                    && e.title == n.title
                    && e.message == n.message
                    && e.url == n.url
            })
            .map(|e| e.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::settings::Sound;

    const MIN: i64 = 60_000;

    fn agent(sender: &str, title: &str, severity: Severity) -> Incoming {
        Incoming {
            sender: Some(sender.into()),
            target: Some(sender.into()),
            agent_label: Some(format!("agent {sender}")),
            ..Incoming::new(Source::Agent, severity, title, "details")
        }
    }

    fn delivered(outcome: &Outcome) -> &Delivery {
        match outcome {
            Outcome::Delivered { delivery, .. } => delivery,
            other => panic!("not delivered: {other:?}"),
        }
    }

    #[test]
    fn a_delivered_notification_is_recorded_unread() {
        let mut c = NotificationCenter::default();
        let mut n = agent("term_a", "Blocked on a decision", Severity::Warn);
        n.url = Some("https://github.com/o/r/pull/1".into());
        let out = c.dispatch(n, &NotifySettings::default(), 0);
        let d = delivered(&out);
        assert!(d.native && d.in_app);
        assert_eq!(d.sound, Some(Sound::System("Glass".into())));
        let e = c.history.get(out.id().unwrap()).unwrap();
        assert_eq!(e.title, "Blocked on a decision");
        assert_eq!(e.agent_label.as_deref(), Some("agent term_a"));
        assert_eq!(e.url.as_deref(), Some("https://github.com/o/r/pull/1"));
        assert!(!e.read);
        assert_eq!(c.history.unread(), 1);
    }

    #[test]
    fn severity_thresholds_route_to_channels() {
        let mut c = NotificationCenter::default();
        let settings = NotifySettings {
            popup_min: Severity::Warn,
            sound_min: Severity::Urgent,
            native_popups: false,
            ..NotifySettings::default()
        };
        let info = c.dispatch(agent("a", "i", Severity::Info), &settings, 0);
        assert!(delivered(&info).is_silent(), "below both thresholds");
        let warn = c.dispatch(agent("a", "w", Severity::Warn), &settings, 0);
        assert_eq!(
            delivered(&warn),
            &Delivery {
                native: false,
                in_app: true,
                sound: None
            }
        );
        let urgent = c.dispatch(agent("a", "u", Severity::Urgent), &settings, 0);
        assert!(delivered(&urgent).sound.is_some());
        assert_eq!(
            c.history.entries().len(),
            3,
            "all recorded, even the silent one"
        );
    }

    #[test]
    fn quiet_mode_records_but_only_urgent_gets_through() {
        let mut c = NotificationCenter::default();
        let quiet = NotifySettings {
            quiet: true,
            ..NotifySettings::default()
        };
        let warn = c.dispatch(agent("a", "w", Severity::Warn), &quiet, 0);
        assert!(delivered(&warn).is_silent());
        let urgent = c.dispatch(agent("a", "u", Severity::Urgent), &quiet, 0);
        assert!(delivered(&urgent).native);
        assert_eq!(c.history.unread(), 2);
    }

    #[test]
    fn identical_notifications_collapse() {
        let mut c = NotificationCenter::default();
        let s = NotifySettings::default();
        let first = c.dispatch(agent("a", "Done", Severity::Info), &s, 0);
        let id = first.id().unwrap();
        c.history.mark_read(id);
        let again = c.dispatch(agent("a", "Done", Severity::Info), &s, MIN);
        assert_eq!(again, Outcome::Collapsed { id, count: 2 });
        let e = c.history.get(id).unwrap();
        assert!(!e.read, "a repeat makes it unread again");
        assert_eq!((e.at_ms, e.first_at_ms), (MIN, 0));
        // A different message, sender or severity is a new notification.
        let mut other = agent("a", "Done", Severity::Info);
        other.message = "other".into();
        assert!(matches!(
            c.dispatch(other, &s, MIN),
            Outcome::Delivered { .. }
        ));
        assert!(matches!(
            c.dispatch(agent("b", "Done", Severity::Info), &s, MIN),
            Outcome::Delivered { .. }
        ));
        assert!(matches!(
            c.dispatch(agent("a", "Done", Severity::Warn), &s, MIN),
            Outcome::Delivered { .. }
        ));
        // Past the window it is news again.
        assert!(matches!(
            c.dispatch(
                agent("a", "Done", Severity::Info),
                &s,
                MIN + DUPLICATE_WINDOW_MS
            ),
            Outcome::Delivered { .. }
        ));
    }

    #[test]
    fn duplicates_do_not_use_up_the_limit() {
        let mut c = NotificationCenter::default();
        let s = NotifySettings::default();
        for _ in 0..20 {
            c.dispatch(agent("a", "same", Severity::Info), &s, 0);
        }
        for i in 1..AGENT_MAX {
            let out = c.dispatch(agent("a", &format!("n{i}"), Severity::Info), &s, 0);
            assert!(matches!(out, Outcome::Delivered { .. }), "{i}: {out:?}");
        }
        assert_eq!(c.history.entries().len(), AGENT_MAX);
    }

    #[test]
    fn each_agent_is_rate_limited() {
        let mut c = NotificationCenter::default();
        let s = NotifySettings::default();
        for i in 0..AGENT_MAX {
            let out = c.dispatch(
                agent("a", &format!("n{i}"), Severity::Urgent),
                &s,
                i as i64 * MIN,
            );
            assert!(matches!(out, Outcome::Delivered { .. }));
        }
        let limited = c.dispatch(agent("a", "one more", Severity::Urgent), &s, 5 * MIN);
        assert_eq!(
            limited,
            Outcome::RateLimited {
                retry_after_ms: AGENT_WINDOW_MS - 5 * MIN
            }
        );
        assert_eq!(c.history.entries().len(), AGENT_MAX, "nothing recorded");
        // Another agent is not affected.
        assert!(matches!(
            c.dispatch(agent("b", "hi", Severity::Info), &s, 5 * MIN),
            Outcome::Delivered { .. }
        ));
        // The first one leaves the window at 10 min.
        assert!(matches!(
            c.dispatch(agent("a", "later", Severity::Info), &s, AGENT_WINDOW_MS),
            Outcome::Delivered { .. }
        ));
    }

    #[test]
    fn an_agents_question_holds_back_the_needs_input_notification() {
        let mut c = NotificationCenter::default();
        let s = NotifySettings::default();
        assert!(matches!(
            c.dispatch(agent("t1", "Question: merge?", Severity::Warn), &s, 0),
            Outcome::Delivered { .. }
        ));
        c.hold_cooldown("t1", 0);
        let input =
            Incoming::new(Source::NeedsInput, Severity::Warn, "needs input", "").with_target("t1");
        assert_eq!(c.dispatch(input, &s, MIN), Outcome::Cooldown);
    }

    #[test]
    fn own_notifications_have_a_per_target_cooldown() {
        let mut c = NotificationCenter::default();
        let s = NotifySettings::default();
        let input = |t: &str| {
            Incoming::new(Source::NeedsInput, Severity::Warn, "needs input", "").with_target(t)
        };
        assert!(matches!(
            c.dispatch(input("t1"), &s, 0),
            Outcome::Delivered { .. }
        ));
        assert_eq!(c.dispatch(input("t1"), &s, 9 * MIN), Outcome::Cooldown);
        assert!(matches!(
            c.dispatch(input("t2"), &s, 9 * MIN),
            Outcome::Delivered { .. }
        ));
        assert!(matches!(
            c.dispatch(input("t1"), &s, PER_TARGET_COOLDOWN_MS),
            Outcome::Delivered { .. }
        ));
        // The cooldown is Myrstack's own: an agent on the same terminal still gets through.
        assert!(matches!(
            c.dispatch(
                agent("t1", "blocked", Severity::Warn),
                &s,
                PER_TARGET_COOLDOWN_MS
            ),
            Outcome::Delivered { .. }
        ));
    }

    #[test]
    fn tests_are_never_limited() {
        let mut c = NotificationCenter::default();
        let s = NotifySettings::default();
        for _ in 0..(AGENT_MAX + 3) {
            let out = c.dispatch(Incoming::new(Source::Test, Severity::Info, "t", ""), &s, 0);
            assert!(matches!(out, Outcome::Delivered { .. }));
        }
    }

    #[test]
    fn a_pr_handover_and_the_review_notification_are_one() {
        let mut c = NotificationCenter::default();
        let s = NotifySettings::default();
        let pr = "https://github.com/o/r/pull/7";
        let review = || {
            Incoming::new(
                Source::Review,
                Severity::Info,
                "PR ready for review: o/r#7",
                "",
            )
            .with_target(pr)
        };
        let handover = || {
            let mut n = agent("term_a", "PR #7 ready", Severity::Info);
            n.url = Some(format!("{pr}/files"));
            n.message = "Review it; merging deploys lite".into();
            n
        };
        // Review first, then the agent's handover folds into it.
        let first = c.dispatch(review(), &s, 0);
        let id = first.id().unwrap();
        c.history.mark_read(id);
        assert_eq!(c.dispatch(handover(), &s, MIN), Outcome::Folded { id });
        let e = c.history.get(id).unwrap();
        assert_eq!(
            (e.title.as_str(), e.count, e.read),
            ("PR #7 ready", 2, false)
        );
        assert_eq!(e.message, "Review it; merging deploys lite");
        assert_eq!(c.history.entries().len(), 1);

        // Handover first: the review notification that follows is dropped.
        let mut c = NotificationCenter::default();
        assert!(matches!(
            c.dispatch(handover(), &s, 0),
            Outcome::Delivered { .. }
        ));
        assert_eq!(c.dispatch(review(), &s, MIN), Outcome::Cooldown);
        assert_eq!(c.history.entries().len(), 1);

        // Another PR, or a non-PR link, is its own notification.
        let mut other = handover();
        other.url = Some("https://github.com/o/r/pull/8".into());
        assert!(matches!(
            c.dispatch(other, &s, MIN),
            Outcome::Delivered { .. }
        ));
    }
}

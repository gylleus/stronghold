//! A short "pop" on an agent that just changed state, shared by the farm:
//! a green burst when it is done, a softer one when it goes idle, an amber one when it starts
//! waiting on the user (whose lasting pulse the views already draw).
//!
//! [`Transitions`] finds the changes between polls, [`Pops`] keeps the pops on screen and
//! [`Pops::frames`] says how far along each is; the views draw them.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use eframe::egui::Color32;

use crate::model::{Node, NodeId, NodeKind, NodeState};

/// How long a pop lasts.
pub const POP_S: f64 = 0.8;
/// How long a new state must hold before it pops: a flap back is no change.
pub const SETTLE_MS: i64 = 2_000;
/// Pops starting this close together share one beat.
const MERGE_S: f64 = 0.25;
/// Up to this many pops at once each get all their sparkles; more are thinned to match.
const CROWD: usize = 4;
/// Share of a pop spent on the hop.
const HOP_SHARE: f32 = 0.45;
/// Share of a pop the flash lasts.
const FLASH_SHARE: f32 = 0.35;
/// Redraw interval while a pop runs.
pub const FRAME: Duration = Duration::from_millis(16);

/// Which pop a change gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cue {
    /// Working → done: a celebratory green pop.
    Done,
    /// Working → idle: a softer, smaller pop.
    Idle,
    /// → needs input: an amber attention pop.
    NeedsInput,
}

impl Cue {
    /// The cue for a settled change from `from` to `to`, if it gets one.
    pub fn of(from: NodeState, to: NodeState) -> Option<Self> {
        let busy = matches!(from, NodeState::Working | NodeState::Stalled);
        match to {
            NodeState::Done if busy => Some(Cue::Done),
            NodeState::Idle if busy => Some(Cue::Idle),
            NodeState::NeedsInput if from != NodeState::NeedsInput => Some(Cue::NeedsInput),
            _ => None,
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            Cue::Done => Color32::from_rgb(0x6c, 0xe0, 0x8c),
            Cue::Idle => Color32::from_rgb(0xb4, 0xc4, 0xd8),
            Cue::NeedsInput => Color32::from_rgb(0xfb, 0xbf, 0x24),
        }
    }

    /// How big the pop is, 0 to 1.
    pub fn strength(self) -> f32 {
        match self {
            Cue::Done => 1.0,
            Cue::Idle => 0.55,
            Cue::NeedsInput => 0.8,
        }
    }

    /// Sparkles in the burst before thinning.
    fn sparkles(self) -> usize {
        match self {
            Cue::Done => 10,
            Cue::Idle => 5,
            Cue::NeedsInput => 7,
        }
    }

    /// The macOS alert sound played with the pop when sounds are on; `None` for a quiet one.
    pub fn sound(self) -> Option<&'static str> {
        match self {
            Cue::Done => Some("Pop"),
            Cue::NeedsInput => Some("Tink"),
            Cue::Idle => None,
        }
    }
}

/// Finds the state changes worth a pop between polls: a node's new state must hold for
/// [`SETTLE_MS`] first. Nodes seen for the first time (all of them on the first poll), the
/// King, subagents, parked agents and released ones never pop.
#[derive(Debug, Default)]
pub struct Transitions {
    /// Each tracked node's last settled state.
    settled: HashMap<NodeId, NodeState>,
    /// A different state a node is in, and since when.
    pending: HashMap<NodeId, (NodeState, i64)>,
}

impl Transitions {
    /// Takes the nodes of the latest snapshot at `now_ms`; returns the changes that settled.
    pub fn observe<'a>(
        &mut self,
        nodes: impl IntoIterator<Item = &'a Node>,
        now_ms: i64,
    ) -> Vec<(NodeId, Cue)> {
        let mut seen = HashSet::with_capacity(self.settled.len());
        let mut cues = Vec::new();
        for node in nodes {
            let tracked = !matches!(node.kind, NodeKind::King | NodeKind::Subagent)
                && !node.is_parked()
                && node.released_at_ms.is_none();
            if !tracked {
                continue;
            }
            let state = node.state;
            seen.insert(node.id.clone());
            let Some(&settled) = self.settled.get(&node.id) else {
                self.settled.insert(node.id.clone(), state);
                continue;
            };
            if state == settled {
                self.pending.remove(&node.id);
                continue;
            }
            match self.pending.get(&node.id) {
                Some(&(held, since)) if held == state => {
                    if now_ms - since >= SETTLE_MS {
                        self.pending.remove(&node.id);
                        self.settled.insert(node.id.clone(), state);
                        cues.extend(Cue::of(settled, state).map(|cue| (node.id.clone(), cue)));
                    }
                }
                _ => {
                    self.pending.insert(node.id.clone(), (state, now_ms));
                }
            }
        }
        self.settled.retain(|id, _| seen.contains(id));
        self.pending.retain(|id, _| seen.contains(id));
        cues
    }

    /// How long until the next pending change could settle.
    pub fn next_due(&self, now_ms: i64) -> Option<Duration> {
        self.pending
            .values()
            .map(|&(_, since)| (since + SETTLE_MS - now_ms).max(0) as u64)
            .min()
            .map(Duration::from_millis)
    }
}

/// A pop on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Pop {
    pub node: NodeId,
    pub cue: Cue,
    pub start_s: f64,
    /// Reduce motion: a highlight that fades, nothing moves.
    pub still: bool,
}

/// The pops on screen.
#[derive(Debug, Clone, Default)]
pub struct Pops {
    live: Vec<Pop>,
}

impl Pops {
    /// Pops `node` with `cue` at `now_s` (egui time), joining the beat of pops started just
    /// before; a pop already running on `node` is replaced.
    pub fn push(&mut self, node: NodeId, cue: Cue, now_s: f64, still: bool) {
        self.live
            .retain(|p| p.node != node && now_s - p.start_s < POP_S);
        let start_s = self
            .live
            .iter()
            .map(|p| p.start_s)
            .filter(|&s| (0.0..MERGE_S).contains(&(now_s - s)))
            .fold(now_s, f64::min);
        self.live.push(Pop {
            node,
            cue,
            start_s,
            still,
        });
    }

    /// Drops finished pops; returns how far along each running one is at `now_s`.
    pub fn frames(&mut self, now_s: f64) -> Vec<(NodeId, PopFrame)> {
        self.live.retain(|p| now_s - p.start_s < POP_S);
        let crowd = self.live.len();
        self.live
            .iter()
            .filter_map(|p| Some((p.node.clone(), PopFrame::at(p, now_s, crowd)?)))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

/// One frame of a pop, as the views draw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopFrame {
    pub cue: Cue,
    /// Progress, 0 to 1.
    pub t: f32,
    /// How high the node hops, as a share of its size (scaled by the cue's strength).
    pub lift: f32,
    /// The flash on the node, 0 to 1.
    pub flash: f32,
    /// How far the ring and sparkles have spread, 0 to 1, eased out.
    pub spread: f32,
    /// Opacity of the ring and sparkles.
    pub fade: f32,
    /// Sparkles in the burst, thinned when many pop at once.
    pub sparkles: usize,
    /// Reduce motion: a static ring at this opacity instead of all the above.
    pub highlight: f32,
}

impl PopFrame {
    /// `pop` at `now_s` among `crowd` running pops; `None` before it starts or once it ends.
    pub fn at(pop: &Pop, now_s: f64, crowd: usize) -> Option<Self> {
        let elapsed = now_s - pop.start_s;
        if !(0.0..POP_S).contains(&elapsed) {
            return None;
        }
        let t = (elapsed / POP_S) as f32;
        let strength = pop.cue.strength();
        if pop.still {
            return Some(Self {
                cue: pop.cue,
                t,
                lift: 0.0,
                flash: 0.0,
                spread: 0.0,
                fade: 0.0,
                sparkles: 0,
                highlight: (1.0 - t) * strength.max(0.7),
            });
        }
        let hop = t / HOP_SHARE;
        let sparkles = if crowd > CROWD {
            (pop.cue.sparkles() * CROWD / crowd).max(2)
        } else {
            pop.cue.sparkles()
        };
        Some(Self {
            cue: pop.cue,
            t,
            lift: if hop < 1.0 {
                0.3 * strength * (hop * std::f32::consts::PI).sin()
            } else {
                0.0
            },
            flash: strength * (1.0 - t / FLASH_SHARE).max(0.0),
            spread: 1.0 - (1.0 - t).powi(2),
            fade: strength * (1.0 - t),
            sparkles,
            highlight: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_790_000_000_000;

    fn node(id: &str, kind: NodeKind, state: NodeState) -> Node {
        Node {
            id: NodeId::terminal(id),
            kind,
            label: id.into(),
            terminal_handle: Some(id.into()),
            live: true,
            run_id: None,
            task_id: None,
            dispatch_id: None,
            agent: None,
            worktree_path: None,
            branch: None,
            last_output_ms: None,
            preview: None,
            state,
            worker_state: None,
            dispatch_status: None,
            worker_terminal_state: None,
            released_at_ms: None,
            parked_at_ms: None,
        }
    }

    fn term(state: NodeState) -> Vec<Node> {
        vec![node("a", NodeKind::Terminal, state)]
    }

    fn cues(t: &mut Transitions, nodes: &[Node], at: i64) -> Vec<Cue> {
        t.observe(nodes, at).into_iter().map(|(_, c)| c).collect()
    }

    #[test]
    fn first_poll_and_new_nodes_never_pop() {
        let mut t = Transitions::default();
        assert!(cues(&mut t, &term(NodeState::Done), T0).is_empty());
        assert!(cues(&mut t, &term(NodeState::Done), T0 + 5_000).is_empty());
        let mut two = term(NodeState::Done);
        two.push(node("b", NodeKind::Terminal, NodeState::NeedsInput));
        assert!(cues(&mut t, &two, T0 + 6_000).is_empty());
        assert!(cues(&mut t, &two, T0 + 9_000).is_empty());
    }

    #[test]
    fn a_held_change_pops_once_after_settling() {
        let mut t = Transitions::default();
        cues(&mut t, &term(NodeState::Working), T0);
        let done = term(NodeState::Done);
        assert!(cues(&mut t, &done, T0 + 1_000).is_empty());
        assert_eq!(t.next_due(T0 + 1_500), Some(Duration::from_millis(1_500)));
        assert!(cues(&mut t, &done, T0 + 2_999).is_empty());
        assert_eq!(cues(&mut t, &done, T0 + 3_000), vec![Cue::Done]);
        assert!(cues(&mut t, &done, T0 + 4_000).is_empty());
        assert!(cues(&mut t, &done, T0 + 60_000).is_empty());
        assert_eq!(t.next_due(T0 + 60_000), None);
    }

    #[test]
    fn a_flap_back_does_not_pop() {
        let mut t = Transitions::default();
        cues(&mut t, &term(NodeState::Working), T0);
        cues(&mut t, &term(NodeState::Idle), T0 + 1_000);
        assert!(cues(&mut t, &term(NodeState::Working), T0 + 2_000).is_empty());
        assert!(cues(&mut t, &term(NodeState::Idle), T0 + 2_500).is_empty());
        // The clock restarted at the second idle.
        assert!(cues(&mut t, &term(NodeState::Idle), T0 + 4_000).is_empty());
        assert_eq!(
            cues(&mut t, &term(NodeState::Idle), T0 + 4_500),
            vec![Cue::Idle]
        );
    }

    #[test]
    fn each_cue_and_the_changes_that_stay_quiet() {
        use NodeState::*;
        assert_eq!(Cue::of(Working, Done), Some(Cue::Done));
        assert_eq!(Cue::of(Stalled, Done), Some(Cue::Done));
        assert_eq!(Cue::of(Working, Idle), Some(Cue::Idle));
        assert_eq!(Cue::of(Idle, NeedsInput), Some(Cue::NeedsInput));
        assert_eq!(Cue::of(Working, NeedsInput), Some(Cue::NeedsInput));
        assert_eq!(Cue::of(Idle, Working), None);
        assert_eq!(Cue::of(NeedsInput, Working), None);
        assert_eq!(Cue::of(Idle, Done), None);
        assert_eq!(Cue::of(Working, Failed), None);
    }

    #[test]
    fn subagents_the_king_and_parked_agents_never_pop() {
        let mut t = Transitions::default();
        let clan = |state| {
            let mut parked = node("p", NodeKind::Terminal, state);
            parked.parked_at_ms = Some(T0);
            vec![
                node("s", NodeKind::Subagent, state),
                node("q", NodeKind::King, state),
                parked,
            ]
        };
        cues(&mut t, &clan(NodeState::Working), T0);
        cues(&mut t, &clan(NodeState::Done), T0 + 1_000);
        assert!(cues(&mut t, &clan(NodeState::Done), T0 + 5_000).is_empty());
        assert_eq!(t.next_due(T0 + 5_000), None);
    }

    #[test]
    fn a_node_that_leaves_and_comes_back_starts_fresh() {
        let mut t = Transitions::default();
        cues(&mut t, &term(NodeState::Working), T0);
        cues(&mut t, &[], T0 + 1_000);
        cues(&mut t, &term(NodeState::Done), T0 + 2_000);
        assert!(cues(&mut t, &term(NodeState::Done), T0 + 9_000).is_empty());
    }

    #[test]
    fn a_pop_is_short_and_moves() {
        let mut pops = Pops::default();
        pops.push(NodeId::terminal("a"), Cue::Done, 10.0, false);
        let (_, mid) = pops.frames(10.15).pop().unwrap();
        assert!(mid.lift > 0.0 && mid.flash > 0.0 && mid.fade > 0.0);
        assert_eq!(mid.highlight, 0.0);
        assert_eq!(mid.sparkles, Cue::Done.sparkles());
        let (_, late) = pops.frames(10.7).pop().unwrap();
        assert_eq!(late.lift, 0.0);
        assert!(late.fade < mid.fade && late.spread > mid.spread);
        assert!(pops.frames(10.0 + POP_S).is_empty());
        assert!(pops.is_empty());
    }

    #[test]
    fn reduced_motion_is_a_fading_highlight() {
        let mut pops = Pops::default();
        pops.push(NodeId::terminal("a"), Cue::Idle, 10.0, true);
        let mut last = f32::MAX;
        for step in 0..8 {
            let (_, f) = pops.frames(10.0 + step as f64 * 0.1).pop().unwrap();
            assert_eq!((f.lift, f.flash, f.spread, f.sparkles), (0.0, 0.0, 0.0, 0));
            assert!(f.highlight > 0.0 && f.highlight < last);
            last = f.highlight;
        }
        assert!(pops.frames(10.0 + POP_S).is_empty());
    }

    #[test]
    fn many_pops_at_once_share_a_beat_and_thin_out() {
        let mut pops = Pops::default();
        for (i, at) in [10.0, 10.03, 10.06, 10.09, 10.12, 10.15, 10.18, 10.21]
            .into_iter()
            .enumerate()
        {
            pops.push(NodeId::terminal(&i.to_string()), Cue::Done, at, false);
        }
        let frames = pops.frames(10.3);
        assert_eq!(frames.len(), 8);
        assert!(frames.iter().all(|(_, f)| f.t == frames[0].1.t));
        assert!(frames.iter().all(|(_, f)| f.sparkles == 5));
        // A pop well after the beat starts its own.
        pops.push(NodeId::terminal("late"), Cue::Idle, 10.6, false);
        let late = pops.frames(10.6);
        assert_eq!(late.last().unwrap().1.t, 0.0);
    }

    #[test]
    fn a_repeat_pop_replaces_the_running_one() {
        let mut pops = Pops::default();
        pops.push(NodeId::terminal("a"), Cue::Idle, 10.0, false);
        pops.push(NodeId::terminal("a"), Cue::NeedsInput, 10.5, false);
        let frames = pops.frames(10.5);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].1.cue, Cue::NeedsInput);
    }
}

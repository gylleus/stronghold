use super::*;
use crate::model::{ClanSnapshot, NodeKind, NodeState};
use crate::orca::TerminalStatus;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect};

fn dwarf() -> Node {
    Node {
        id: NodeId("term:worker".into()),
        kind: NodeKind::Terminal,
        label: "Worker".into(),
        terminal_handle: Some("worker".into()),
        live: true,
        run_id: None,
        task_id: None,
        dispatch_id: None,
        agent: None,
        worktree_path: None,
        branch: None,
        last_output_ms: None,
        preview: None,
        state: NodeState::Idle,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
        parked_at_ms: None,
    }
}

fn terminal() -> Terminal {
    Terminal {
        handle: "worker".into(),
        connected: true,
        writable: true,
        ..Default::default()
    }
}

#[test]
fn only_live_writable_unparked_terminal_inputs_are_eligible() {
    let node = dwarf();
    let terminal = terminal();
    let input = Some(ui::terminal::input_id("worker"));
    assert_eq!(terminal_input(&node, Some(&terminal), None), input);
    for excluded in [
        Node {
            kind: NodeKind::Subagent,
            ..node.clone()
        },
        Node {
            live: false,
            ..node.clone()
        },
        Node {
            terminal_handle: None,
            ..node.clone()
        },
        Node {
            parked_at_ms: Some(1),
            ..node.clone()
        },
    ] {
        assert_eq!(terminal_input(&excluded, Some(&terminal), None), None);
    }
    for excluded in [
        None,
        Some(Terminal {
            connected: false,
            ..terminal.clone()
        }),
        Some(Terminal {
            writable: false,
            ..terminal.clone()
        }),
        Some(Terminal {
            handle: "other".into(),
            ..terminal.clone()
        }),
    ] {
        assert_eq!(terminal_input(&node, excluded.as_ref(), None), None);
    }
    let mut screen = ScreenUpdate {
        handle: "worker".into(),
        lines: vec![],
        status: Some(TerminalStatus::Exited),
        read_at_ms: 0,
        error: None,
    };
    assert_eq!(terminal_input(&node, Some(&terminal), Some(&screen)), None);
    screen.handle = "previous selection".into();
    assert_eq!(terminal_input(&node, Some(&terminal), Some(&screen)), input);
}

#[test]
fn delayed_focus_is_cancelled_when_selection_or_input_changes() {
    let ctx = egui::Context::default();
    let previous = egui::Id::new("previous field");
    let node = dwarf();
    let input = ui::terminal::input_id("worker");
    for (selected, available) in [
        (None, Some(input)),
        (Some(NodeId::king()), Some(input)),
        (Some(node.id.clone()), None),
        (
            Some(node.id.clone()),
            Some(ui::terminal::input_id("replacement")),
        ),
    ] {
        ctx.memory_mut(|m| m.request_focus(previous));
        Request::from_click(&node.id, Modifiers::NONE, Some(input))
            .unwrap()
            .apply(&ctx, selected.as_ref(), available);
        assert_eq!(ctx.memory(|m| m.focused()), Some(previous));
    }
}

/// Drive the real hold and conversation widgets, including both dock drawing orders.
struct Harness {
    ctx: egui::Context,
    snapshot: ClanSnapshot,
    clan: ui::clan_view::ClanView,
    farm: ui::farm::FarmView,
    pending: Option<Request>,
    selected: Option<NodeId>,
    terminal: Terminal,
    input: ui::terminal::InputPanelState,
    chat: ui::chat::ChatPanelState,
    conversation_first: bool,
    positions: Vec<(NodeId, Pos2)>,
    time: f64,
}

impl Harness {
    fn new(conversation_first: bool) -> Self {
        let mut snapshot = ClanSnapshot::default();
        snapshot.nodes.push(dwarf());
        snapshot.add_king();
        let mut clan = ui::clan_view::ClanView::new();
        clan.sync(&snapshot);
        Self {
            ctx: Default::default(),
            snapshot,
            clan,
            farm: ui::farm::FarmView::new(),
            pending: None,
            selected: None,
            terminal: terminal(),
            input: Default::default(),
            chat: Default::default(),
            conversation_first,
            positions: vec![],
            time: 0.0,
        }
    }

    fn available(&self) -> Option<egui::Id> {
        if self.clan.selection().len() > 1 {
            return None;
        }
        let id = self.selected.as_ref()?;
        if *id == NodeId::king() {
            return Some(ui::chat::input_id());
        }
        terminal_input(self.snapshot.node(id)?, Some(&self.terminal), None)
    }

    fn conversation(&mut self, ui: &mut egui::Ui) {
        if self.selected.as_ref() == Some(&NodeId::king()) {
            ui::chat::chat_ui(
                ui,
                &mut self.chat,
                ui::chat::ChatView {
                    transcript: &Default::default(),
                    state: &crate::chat::SessionState::Ready,
                    is_node: &|_| false,
                    auto_mode: false,
                },
            );
        } else if let Some(node) = self.selected.as_ref().and_then(|id| self.snapshot.node(id)) {
            ui::terminal::terminal_panel(
                ui,
                node,
                None,
                ui::terminal::PanelInput {
                    state: &mut self.input,
                    terminal: Some(&self.terminal),
                    conversation: None,
                    is_node: &|_| false,
                    review_rows: &[],
                },
                0,
            );
        }
        if let Some(request) = self.pending.take() {
            request.apply(ui.ctx(), self.selected.as_ref(), self.available());
        }
    }

    fn hold(&mut self, ui: &mut egui::Ui) {
        let scene = ui::farm::layout::layout(
            &ui::farm::layout::FarmInput {
                snapshot: &self.snapshot,
                hidden: &Default::default(),
                pr_ready: &Default::default(),
                building: &Default::default(),
                now_ms: 0,
            },
            ui.available_rect_before_wrap(),
        );
        self.positions = scene
            .dwarves
            .iter()
            .map(|d| (d.id.clone(), d.pos))
            .collect();
        let response = self.farm.show(ui, &self.snapshot, &mut self.clan);
        if let Some(id) = response.clicked {
            let modifiers = ui.input(|i| i.modifiers);
            self.clan
                .click_node(&id, ui::clan_view::is_additive(modifiers));
            self.selected = self.clan.selected();
            self.pending = Request::from_click(&id, modifiers, self.available());
        }
    }

    fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) {
        self.time += 1.0 / 60.0;
        let ctx = self.ctx.clone();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 900.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ui| {
                if let Some(request) = &self.pending {
                    request.apply(ui.ctx(), self.selected.as_ref(), self.available());
                }
                assert!(ui::keys::take_commands(ui.ctx()).is_empty());
                ui.input_mut(|i| i.modifiers = modifiers);
                ui.columns(2, |columns| {
                    if self.conversation_first {
                        self.conversation(&mut columns[1]);
                    }
                    self.hold(&mut columns[0]);
                    if !self.conversation_first {
                        self.conversation(&mut columns[1]);
                    }
                });
            },
        );
        output.textures_delta.clear();
    }

    fn gesture(&mut self, id: &NodeId, modifiers: Modifiers, button: PointerButton, drag: bool) {
        self.frame(vec![], Modifiers::NONE);
        let at = self
            .positions
            .iter()
            .find(|(node, _)| node == id)
            .unwrap()
            .1;
        let end = if drag {
            at + egui::vec2(80.0, 50.0)
        } else {
            at
        };
        self.frame(vec![Event::PointerMoved(at)], modifiers);
        self.frame(
            vec![Event::PointerButton {
                pos: at,
                button,
                pressed: true,
                modifiers,
            }],
            modifiers,
        );
        if drag {
            self.frame(vec![Event::PointerMoved(end)], modifiers);
        }
        self.frame(
            vec![Event::PointerButton {
                pos: end,
                button,
                pressed: false,
                modifiers,
            }],
            modifiers,
        );
    }
}

#[test]
fn hold_click_types_into_dwarf_and_king_in_either_pane_order() {
    for conversation_first in [false, true] {
        let mut h = Harness::new(conversation_first);
        for id in [dwarf().id, NodeId::king(), dwarf().id] {
            h.gesture(&id, Modifiers::NONE, PointerButton::Primary, false);
            assert_eq!(h.selected.as_ref(), Some(&id));
            h.frame(
                vec![Event::Text("j".into()), Event::Text("hello".into())],
                Modifiers::NONE,
            );
            assert_eq!(h.ctx.memory(|m| m.focused()), h.available());
            let draft = if id == NodeId::king() {
                &h.chat.draft
            } else {
                h.input.draft_mut("worker")
            };
            assert!(
                draft.ends_with("jhello"),
                "typing reaches the clicked input"
            );
            // Reclicking an already selected dwarf restores focus too.
            h.ctx
                .memory_mut(|m| m.surrender_focus(h.available().unwrap()));
            h.gesture(&id, Modifiers::NONE, PointerButton::Primary, false);
            h.frame(vec![], Modifiers::NONE);
            assert_eq!(h.ctx.memory(|m| m.focused()), h.available());
        }
    }
}

#[test]
fn modifiers_drags_and_non_click_selection_do_not_focus_input() {
    for modifiers in [
        Modifiers::SHIFT,
        Modifiers::COMMAND,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::MAC_CMD,
    ] {
        let mut h = Harness::new(false);
        h.gesture(&dwarf().id, modifiers, PointerButton::Primary, false);
        assert_ne!(
            h.ctx.memory(|m| m.focused()),
            Some(ui::terminal::input_id("worker"))
        );
        assert!(h.pending.is_none());
    }
    for button in [
        PointerButton::Primary,
        PointerButton::Secondary,
        PointerButton::Middle,
    ] {
        let mut h = Harness::new(false);
        h.gesture(&dwarf().id, Modifiers::NONE, button, true);
        assert!(h.pending.is_none());
        assert_ne!(
            h.ctx.memory(|m| m.focused()),
            Some(ui::terminal::input_id("worker"))
        );
    }
    let mut h = Harness::new(false);
    h.clan.set_selected(Some(&dwarf().id));
    h.selected = Some(dwarf().id);
    h.frame(vec![], Modifiers::NONE);
    assert!(h.pending.is_none());
    assert_ne!(h.ctx.memory(|m| m.focused()), h.available());
}

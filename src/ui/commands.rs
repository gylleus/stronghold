//! The command palette's actions: titles, keywords, shortcut labels, when each one can run,
//! what it dispatches to, and the recently run ones.

use std::collections::VecDeque;

use crate::model::NodeState;
use crate::ui::keys::KeyCommand;

/// A palette action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    OpenChat,
    Dictate,
    TalkToKing,
    StopSpeaking,
    ToggleSpeakReplies,
    StatusReport,
    NewSession,
    WhipSelected,
    WhipStalled,
    CloseSelected,
    Broadcast,
    ReviewQueue,
    WhipSettings,
    Settings,
    ToggleDryRun,
    FitFarm,
    ResetPanes,
    ToggleNotifications,
    ClearFilters,
    ShowNeedsInput,
    ShowStalled,
    ShowWorking,
    ToggleLoose,
    ToggleReleased,
    NextRepo,
    PrevRepo,
    NextRun,
    PrevRun,
    NextNode,
    PrevNode,
    HistoryBack,
    HistoryForward,
    OpenInOrca,
    TypeIntoTerminal,
    KeyboardHelp,
    OpenLog,
}

/// An action's fixed description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub id: ActionId,
    /// Stable name, saved in the recents list.
    pub key: &'static str,
    pub title: &'static str,
    /// Extra words the search matches.
    pub keywords: &'static str,
    /// Shortcut label shown right-aligned, e.g. "⌘J".
    pub shortcut: Option<&'static str>,
}

const fn action(
    id: ActionId,
    key: &'static str,
    title: &'static str,
    keywords: &'static str,
    shortcut: Option<&'static str>,
) -> Action {
    Action {
        id,
        key,
        title,
        keywords,
        shortcut,
    }
}

/// Every action, in the order `>` lists them.
pub const ACTIONS: &[Action] = &[
    action(
        ActionId::OpenChat,
        "open_chat",
        "Open King chat",
        "king chat talk ask message",
        Some("⌘J"),
    ),
    action(
        ActionId::Dictate,
        "dictate",
        "Dictate (hold ⌘E)",
        "voice speech talk microphone dictation transcribe speak",
        Some("⌘E"),
    ),
    action(
        ActionId::TalkToKing,
        "talk_to_king",
        "Talk to the King",
        "voice speak ask microphone hold hotkey conversation",
        None,
    ),
    action(
        ActionId::StatusReport,
        "status_report",
        "Status report",
        "king chat ask summary clan overview",
        None,
    ),
    action(
        ActionId::NewSession,
        "new_session",
        "New dwarf…",
        "dwarf session spawn start launch agent claude codex worktree worker prompt",
        None,
    ),
    action(
        ActionId::WhipSelected,
        "whip_selected",
        "Whip selected",
        "nudge poke manual whip agent",
        Some("w"),
    ),
    action(
        ActionId::WhipStalled,
        "whip_stalled",
        "Whip all stalled now",
        "nudge stalled stuck pass tick",
        None,
    ),
    action(
        ActionId::CloseSelected,
        "close_selected",
        "Close selected…",
        "kill session terminal end stop",
        Some("x ⌫"),
    ),
    action(
        ActionId::Broadcast,
        "broadcast",
        "Broadcast…",
        "message coordinators send all king",
        None,
    ),
    action(
        ActionId::ReviewQueue,
        "review_queue",
        "Review queue",
        "pr pull request ready review github merge ankra queue",
        None,
    ),
    action(
        ActionId::WhipSettings,
        "whip_settings",
        "Whip settings…",
        "configure rules thresholds preferences options",
        None,
    ),
    action(
        ActionId::Settings,
        "settings",
        "Settings…",
        "preferences options configure king model effort voice whip data dir socket version",
        Some("⌘,"),
    ),
    action(
        ActionId::ToggleDryRun,
        "toggle_dry_run",
        "Toggle whip dry-run",
        "whip dry run live mode",
        None,
    ),
    action(
        ActionId::StopSpeaking,
        "stop_speaking",
        "Stop speaking",
        "voice quiet silence mute reply king audio",
        None,
    ),
    action(
        ActionId::ToggleSpeakReplies,
        "toggle_speak_replies",
        "Speak replies on/off",
        "voice read aloud tts king audio",
        None,
    ),
    action(
        ActionId::FitFarm,
        "fit_farm",
        "Fit hold",
        "zoom screen view all center",
        Some("f"),
    ),
    action(
        ActionId::ResetPanes,
        "reset_panes",
        "Reset pane layout",
        "dock panes tabs columns sidebar details conversation arrangement default restore",
        None,
    ),
    action(
        ActionId::ToggleNotifications,
        "toggle_notifications",
        "Show/hide notifications",
        "notifications inbox bell panel history alerts unread collapse",
        None,
    ),
    action(
        ActionId::ClearFilters,
        "clear_filters",
        "Clear filters",
        "reset filter show all",
        None,
    ),
    action(
        ActionId::ShowNeedsInput,
        "show_needs_input",
        "Show only: needs input",
        "filter state waiting prompt",
        None,
    ),
    action(
        ActionId::ShowStalled,
        "show_stalled",
        "Show only: stalled",
        "filter state stuck",
        None,
    ),
    action(
        ActionId::ShowWorking,
        "show_working",
        "Show only: working",
        "filter state busy active",
        None,
    ),
    action(
        ActionId::ToggleLoose,
        "toggle_loose",
        "Toggle loose terminals",
        "filter show hide loose terminals",
        None,
    ),
    action(
        ActionId::ToggleReleased,
        "toggle_released",
        "Toggle recently released",
        "filter show hide released workers",
        None,
    ),
    action(
        ActionId::NextRepo,
        "next_repo",
        "Next repo",
        "navigate go repository",
        Some("]"),
    ),
    action(
        ActionId::PrevRepo,
        "prev_repo",
        "Previous repo",
        "navigate go back repository",
        Some("["),
    ),
    action(
        ActionId::NextRun,
        "next_run",
        "Next run",
        "navigate go",
        Some("J"),
    ),
    action(
        ActionId::PrevRun,
        "prev_run",
        "Previous run",
        "navigate go back",
        Some("K"),
    ),
    action(
        ActionId::NextNode,
        "next_node",
        "Next node",
        "navigate go",
        Some("j"),
    ),
    action(
        ActionId::PrevNode,
        "prev_node",
        "Previous node",
        "navigate go back",
        Some("k"),
    ),
    action(
        ActionId::HistoryBack,
        "history_back",
        "Back",
        "history previous last return selection navigate go",
        Some("⌘["),
    ),
    action(
        ActionId::HistoryForward,
        "history_forward",
        "Forward",
        "history next selection navigate go",
        Some("⌘]"),
    ),
    action(
        ActionId::OpenInOrca,
        "open_in_orca",
        "Open in Orca",
        "terminal switch tab show",
        Some("o"),
    ),
    action(
        ActionId::TypeIntoTerminal,
        "type_into_terminal",
        "Type into terminal",
        "input box focus send",
        Some("i"),
    ),
    action(
        ActionId::KeyboardHelp,
        "keyboard_help",
        "Keyboard help",
        "shortcuts keys bindings",
        Some("?"),
    ),
    action(
        ActionId::OpenLog,
        "open_log",
        "Open log",
        "log file console debug errors warnings diagnostics troubleshoot",
        None,
    ),
];

impl ActionId {
    pub fn def(self) -> &'static Action {
        ACTIONS
            .iter()
            .find(|a| a.id == self)
            .expect("every action is in ACTIONS")
    }

    /// The action saved under `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        ACTIONS.iter().find(|a| a.key == key).map(|a| a.id)
    }
}

/// App state the actions' titles and enabled checks read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionContext {
    /// The first poll has arrived.
    pub has_data: bool,
    /// Nodes a whip or close of the selection would cover.
    pub whip_targets: usize,
    pub close_targets: usize,
    /// The selected node has a terminal (to open in Orca or type into).
    pub selected_terminal: bool,
    pub dry_run: bool,
    /// Any sidebar filter differs from the default.
    pub filters_active: bool,
    pub show_loose: bool,
    pub show_released: bool,
    /// The King's chat can take a message now.
    pub chat_ready: bool,
    /// The talk-to-the-King hotkey's label, e.g. `⌘⇧E`.
    pub talk_hotkey: String,
    /// The King is speaking.
    pub speaking: bool,
    /// Every reply is spoken.
    pub speak_replies: bool,
    /// The selection history has an entry to go back or forward to.
    pub can_back: bool,
    pub can_forward: bool,
}

impl Default for ActionContext {
    fn default() -> Self {
        Self {
            has_data: false,
            whip_targets: 0,
            close_targets: 0,
            selected_terminal: false,
            dry_run: true,
            filters_active: false,
            show_loose: true,
            show_released: true,
            chat_ready: false,
            talk_hotkey: "⌘⇧E".into(),
            speaking: false,
            speak_replies: false,
            can_back: false,
            can_forward: false,
        }
    }
}

/// The action's title, with the current state where the action flips it.
pub fn title(id: ActionId, cx: &ActionContext) -> String {
    let on_off = |on: bool| if on { "on" } else { "off" };
    match id {
        ActionId::ToggleDryRun => format!("Toggle whip dry-run (now {})", on_off(cx.dry_run)),
        ActionId::ToggleLoose => format!(
            "{} loose terminals",
            if cx.show_loose { "Hide" } else { "Show" }
        ),
        ActionId::ToggleReleased => format!(
            "{} recently released",
            if cx.show_released { "Hide" } else { "Show" }
        ),
        ActionId::TalkToKing => format!("Talk to the King (hold {})", cx.talk_hotkey),
        ActionId::ToggleSpeakReplies => {
            format!("Speak replies on/off (now {})", on_off(cx.speak_replies))
        }
        _ => id.def().title.to_string(),
    }
}

/// Why the action cannot run now; `None` when it can.
pub fn disabled_reason(id: ActionId, cx: &ActionContext) -> Option<&'static str> {
    use ActionId::*;
    let reason = match id {
        WhipSelected if cx.whip_targets == 0 => "Select a node first",
        CloseSelected if cx.close_targets == 0 => "Select a node other than the King first",
        OpenInOrca | TypeIntoTerminal if !cx.selected_terminal => {
            "Select a node with a terminal first"
        }
        StatusReport if !cx.chat_ready => "The King's chat is busy or not ready",
        StopSpeaking if !cx.speaking => "The King is not speaking",
        ClearFilters if !cx.filters_active => "No filters are set",
        HistoryBack if !cx.can_back => "No earlier selection",
        HistoryForward if !cx.can_forward => "No later selection",
        WhipSelected | CloseSelected | WhipStalled | NextRepo | PrevRepo | NextRun | PrevRun
        | NextNode | PrevNode | FitFarm | KeyboardHelp
            if !cx.has_data =>
        {
            "Waiting for the first clan poll"
        }
        _ => return None,
    };
    Some(reason)
}

/// What running an action does; keyboard-shortcut actions go through the same code path as
/// their key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    Key(KeyCommand),
    /// Select the King and focus his chat (unlike ⌘J, never closes it).
    OpenChat,
    /// Record until ⌘E or Stop, then put the text into the King's chat.
    Dictate,
    /// Record until the talk hotkey or Send, then send it to the King as a voice message.
    TalkToKing,
    StopSpeaking,
    ToggleSpeakReplies,
    /// Open the chat and send the Status report prompt, like its button.
    StatusReport,
    WhipStalled,
    /// Select the King and open his broadcast section.
    Broadcast,
    /// Select the King and open his "Ready for review" section.
    ReviewQueue,
    /// Open the Settings window on its Whip section.
    WhipSettings,
    /// Open the Settings window (⌘,).
    OpenSettings,
    ToggleDryRun,
    /// Put the dock's panes back in their default arrangement.
    ResetPanes,
    /// Open the notifications panel, or collapse it when it is in front.
    ToggleNotifications,
    ClearFilters,
    ShowOnly(NodeState),
    ToggleLoose,
    ToggleReleased,
    /// Open the "New dwarf…" dialog.
    NewSession,
    /// Open the log file in Console.
    OpenLog,
}

pub fn dispatch(id: ActionId) -> Dispatch {
    use ActionId as A;
    match id {
        A::OpenChat => Dispatch::OpenChat,
        A::Dictate => Dispatch::Dictate,
        A::TalkToKing => Dispatch::TalkToKing,
        A::StopSpeaking => Dispatch::StopSpeaking,
        A::ToggleSpeakReplies => Dispatch::ToggleSpeakReplies,
        A::StatusReport => Dispatch::StatusReport,
        A::NewSession => Dispatch::NewSession,
        A::WhipSelected => Dispatch::Key(KeyCommand::Whip),
        A::WhipStalled => Dispatch::WhipStalled,
        A::CloseSelected => Dispatch::Key(KeyCommand::CloseSession),
        A::Broadcast => Dispatch::Broadcast,
        A::ReviewQueue => Dispatch::ReviewQueue,
        A::WhipSettings => Dispatch::WhipSettings,
        A::Settings => Dispatch::OpenSettings,
        A::ToggleDryRun => Dispatch::ToggleDryRun,
        A::FitFarm => Dispatch::Key(KeyCommand::Fit),
        A::ResetPanes => Dispatch::ResetPanes,
        A::ToggleNotifications => Dispatch::ToggleNotifications,
        A::ClearFilters => Dispatch::ClearFilters,
        A::ShowNeedsInput => Dispatch::ShowOnly(NodeState::NeedsInput),
        A::ShowStalled => Dispatch::ShowOnly(NodeState::Stalled),
        A::ShowWorking => Dispatch::ShowOnly(NodeState::Working),
        A::ToggleLoose => Dispatch::ToggleLoose,
        A::ToggleReleased => Dispatch::ToggleReleased,
        A::NextRepo => Dispatch::Key(KeyCommand::NextRepo),
        A::PrevRepo => Dispatch::Key(KeyCommand::PrevRepo),
        A::NextRun => Dispatch::Key(KeyCommand::NextRun),
        A::PrevRun => Dispatch::Key(KeyCommand::PrevRun),
        A::NextNode => Dispatch::Key(KeyCommand::NextNode),
        A::PrevNode => Dispatch::Key(KeyCommand::PrevNode),
        A::HistoryBack => Dispatch::Key(KeyCommand::HistoryBack),
        A::HistoryForward => Dispatch::Key(KeyCommand::HistoryForward),
        A::OpenInOrca => Dispatch::Key(KeyCommand::OpenInOrca),
        A::TypeIntoTerminal => Dispatch::Key(KeyCommand::FocusInput),
        A::KeyboardHelp => Dispatch::Key(KeyCommand::ToggleHelp),
        A::OpenLog => Dispatch::OpenLog,
    }
}

/// How many recently run actions the palette remembers.
pub const MAX_RECENTS: usize = 5;

/// Recently run actions, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recents(VecDeque<ActionId>);

impl Recents {
    /// Moves `id` to the front, dropping the oldest beyond [`MAX_RECENTS`].
    pub fn push(&mut self, id: ActionId) {
        self.0.retain(|a| *a != id);
        self.0.push_front(id);
        self.0.truncate(MAX_RECENTS);
    }

    pub fn ids(&self) -> impl Iterator<Item = ActionId> + '_ {
        self.0.iter().copied()
    }

    /// Saved keys, newest first.
    pub fn keys(&self) -> Vec<String> {
        self.ids().map(|id| id.def().key.to_string()).collect()
    }

    /// Restores saved keys, skipping unknown ones and duplicates.
    pub fn from_keys<S: AsRef<str>>(keys: &[S]) -> Self {
        let mut recents = Self::default();
        for id in keys
            .iter()
            .rev()
            .filter_map(|k| ActionId::from_key(k.as_ref()))
        {
            recents.push(id);
        }
        recents
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn registry_has_unique_ids_and_keys() {
        let ids: HashSet<ActionId> = ACTIONS.iter().map(|a| a.id).collect();
        let keys: HashSet<&str> = ACTIONS.iter().map(|a| a.key).collect();
        assert_eq!(ids.len(), ACTIONS.len());
        assert_eq!(keys.len(), ACTIONS.len());
        for a in ACTIONS {
            assert_eq!(a.id.def(), a);
            assert_eq!(ActionId::from_key(a.key), Some(a.id));
        }
    }

    /// Every keyboard command except the ones that only make sense on the canvas has an action
    /// running it, and that action shows a shortcut label.
    #[test]
    fn every_shortcut_has_a_palette_action() {
        // Esc and Enter act on the palette itself while it is open.
        let palette_keys = [KeyCommand::Escape, KeyCommand::OpenPanel];
        for command in KeyCommand::ALL {
            if palette_keys.contains(&command) {
                continue;
            }
            let action = ACTIONS.iter().find(|a| match dispatch(a.id) {
                Dispatch::Key(c) => c == command,
                Dispatch::OpenChat => command == KeyCommand::ToggleChat,
                Dispatch::OpenSettings => command == KeyCommand::OpenSettings,
                _ => false,
            });
            let action = action.unwrap_or_else(|| panic!("no palette action for {command:?}"));
            assert!(
                action.shortcut.is_some(),
                "{:?} shows its shortcut",
                action.id
            );
        }
    }

    #[test]
    fn shortcut_labels_match_the_key_bindings() {
        use crate::ui::keys::command_for_event;
        use eframe::egui::{Event, Key, Modifiers};
        for a in ACTIONS {
            let (Some(label), Dispatch::Key(command)) = (a.shortcut, dispatch(a.id)) else {
                continue;
            };
            let key = label.split_whitespace().next().unwrap();
            let event = match key.strip_prefix('⌘') {
                Some(name) => Event::Key {
                    key: Key::from_name(name).unwrap_or_else(|| panic!("{label}")),
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::COMMAND,
                },
                None => Event::Text(key.to_string()),
            };
            assert_eq!(command_for_event(&event, false), Some(command), "{label}");
        }
    }

    #[test]
    fn dispatch_mapping() {
        use ActionId as A;
        let cases = [
            (A::OpenChat, Dispatch::OpenChat),
            (A::Dictate, Dispatch::Dictate),
            (A::TalkToKing, Dispatch::TalkToKing),
            (A::StopSpeaking, Dispatch::StopSpeaking),
            (A::ToggleSpeakReplies, Dispatch::ToggleSpeakReplies),
            (A::StatusReport, Dispatch::StatusReport),
            (A::NewSession, Dispatch::NewSession),
            (A::WhipSelected, Dispatch::Key(KeyCommand::Whip)),
            (A::WhipStalled, Dispatch::WhipStalled),
            (A::CloseSelected, Dispatch::Key(KeyCommand::CloseSession)),
            (A::Broadcast, Dispatch::Broadcast),
            (A::ReviewQueue, Dispatch::ReviewQueue),
            (A::WhipSettings, Dispatch::WhipSettings),
            (A::Settings, Dispatch::OpenSettings),
            (A::ToggleDryRun, Dispatch::ToggleDryRun),
            (A::FitFarm, Dispatch::Key(KeyCommand::Fit)),
            (A::ResetPanes, Dispatch::ResetPanes),
            (A::ToggleNotifications, Dispatch::ToggleNotifications),
            (A::ClearFilters, Dispatch::ClearFilters),
            (A::ShowNeedsInput, Dispatch::ShowOnly(NodeState::NeedsInput)),
            (A::ShowStalled, Dispatch::ShowOnly(NodeState::Stalled)),
            (A::ShowWorking, Dispatch::ShowOnly(NodeState::Working)),
            (A::ToggleLoose, Dispatch::ToggleLoose),
            (A::ToggleReleased, Dispatch::ToggleReleased),
            (A::NextRepo, Dispatch::Key(KeyCommand::NextRepo)),
            (A::PrevRepo, Dispatch::Key(KeyCommand::PrevRepo)),
            (A::NextRun, Dispatch::Key(KeyCommand::NextRun)),
            (A::PrevRun, Dispatch::Key(KeyCommand::PrevRun)),
            (A::NextNode, Dispatch::Key(KeyCommand::NextNode)),
            (A::PrevNode, Dispatch::Key(KeyCommand::PrevNode)),
            (A::HistoryBack, Dispatch::Key(KeyCommand::HistoryBack)),
            (A::HistoryForward, Dispatch::Key(KeyCommand::HistoryForward)),
            (A::OpenInOrca, Dispatch::Key(KeyCommand::OpenInOrca)),
            (A::TypeIntoTerminal, Dispatch::Key(KeyCommand::FocusInput)),
            (A::KeyboardHelp, Dispatch::Key(KeyCommand::ToggleHelp)),
            (A::OpenLog, Dispatch::OpenLog),
        ];
        assert_eq!(cases.len(), ACTIONS.len(), "every action is covered");
        for (id, expected) in cases {
            assert_eq!(dispatch(id), expected, "{id:?}");
        }
    }

    #[test]
    fn enabled_checks_give_reasons() {
        let mut cx = ActionContext {
            has_data: true,
            ..ActionContext::default()
        };
        assert_eq!(
            disabled_reason(ActionId::WhipSelected, &cx),
            Some("Select a node first")
        );
        assert!(disabled_reason(ActionId::CloseSelected, &cx).is_some());
        assert!(disabled_reason(ActionId::OpenInOrca, &cx).is_some());
        assert!(disabled_reason(ActionId::StatusReport, &cx).is_some());
        assert!(disabled_reason(ActionId::ClearFilters, &cx).is_some());
        assert_eq!(disabled_reason(ActionId::OpenChat, &cx), None);
        assert_eq!(
            disabled_reason(ActionId::StopSpeaking, &cx),
            Some("The King is not speaking")
        );
        assert_eq!(disabled_reason(ActionId::TalkToKing, &cx), None);
        assert_eq!(
            title(ActionId::TalkToKing, &cx),
            "Talk to the King (hold ⌘⇧E)"
        );
        assert_eq!(
            title(ActionId::ToggleSpeakReplies, &cx),
            "Speak replies on/off (now off)"
        );
        assert_eq!(disabled_reason(ActionId::Broadcast, &cx), None);

        cx.whip_targets = 1;
        cx.close_targets = 1;
        cx.selected_terminal = true;
        cx.chat_ready = true;
        cx.filters_active = true;
        assert_eq!(
            disabled_reason(ActionId::HistoryBack, &cx),
            Some("No earlier selection")
        );
        cx.can_back = true;
        cx.can_forward = true;
        for id in [
            ActionId::HistoryBack,
            ActionId::HistoryForward,
            ActionId::WhipSelected,
            ActionId::CloseSelected,
            ActionId::OpenInOrca,
            ActionId::StatusReport,
            ActionId::ClearFilters,
        ] {
            assert_eq!(disabled_reason(id, &cx), None, "{id:?}");
        }
        cx.has_data = false;
        assert_eq!(
            disabled_reason(ActionId::WhipStalled, &cx),
            Some("Waiting for the first clan poll")
        );
    }

    #[test]
    fn titles_show_current_state() {
        let mut cx = ActionContext::default();
        assert_eq!(
            title(ActionId::ToggleDryRun, &cx),
            "Toggle whip dry-run (now on)"
        );
        assert_eq!(title(ActionId::ToggleLoose, &cx), "Hide loose terminals");
        cx.dry_run = false;
        cx.show_loose = false;
        cx.show_released = false;
        assert_eq!(
            title(ActionId::ToggleDryRun, &cx),
            "Toggle whip dry-run (now off)"
        );
        assert_eq!(title(ActionId::ToggleLoose, &cx), "Show loose terminals");
        assert_eq!(
            title(ActionId::ToggleReleased, &cx),
            "Show recently released"
        );
        assert_eq!(title(ActionId::FitFarm, &cx), "Fit hold");
    }

    #[test]
    fn recents_are_newest_first_unique_and_capped() {
        let mut r = Recents::default();
        for id in [
            ActionId::FitFarm,
            ActionId::OpenChat,
            ActionId::WhipStalled,
            ActionId::FitFarm,
        ] {
            r.push(id);
        }
        let ids: Vec<_> = r.ids().collect();
        assert_eq!(
            ids,
            [ActionId::FitFarm, ActionId::WhipStalled, ActionId::OpenChat]
        );
        for a in ACTIONS {
            r.push(a.id);
        }
        assert_eq!(r.ids().count(), MAX_RECENTS);
        assert_eq!(r.ids().next(), Some(ACTIONS.last().unwrap().id));

        let restored = Recents::from_keys(&r.keys());
        assert_eq!(restored, r);
        let partial = Recents::from_keys(&["fit_farm", "gone", "open_chat", "fit_farm"]);
        let ids: Vec<_> = partial.ids().collect();
        assert_eq!(ids, [ActionId::FitFarm, ActionId::OpenChat]);
    }
}

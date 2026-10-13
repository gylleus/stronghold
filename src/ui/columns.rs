//! What the conversation pane shows (the King's chat, or the selected agent's screen and
//! input; the King's chat when nothing is selected, unless ⌘J hid it) and where the top bar's
//! King quick actions sit above it. The panes themselves are dock tabs, see [`super::dock`].

/// What the selection is, for the conversation column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    Nothing,
    King,
    /// One agent, or several nodes.
    Other,
}

/// Whether the conversation column shows the King's chat: when he is selected, or when
/// nothing is and the user has not hidden it.
pub fn shows_chat(picked: Picked, chat_hidden: bool) -> bool {
    match picked {
        Picked::King => true,
        Picked::Nothing => !chat_hidden,
        Picked::Other => false,
    }
}

/// Where the top bar's King quick actions (`width` wide) start: above the conversation
/// column's content when it is open, else at the bar's right end; always inside the bar.
pub fn quick_actions_x(
    bar_left: f32,
    bar_right: f32,
    width: f32,
    conversation: Option<f32>,
) -> f32 {
    let latest = (bar_right - width).max(bar_left);
    conversation.map_or(latest, |x| x.clamp(bar_left, latest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_shows_by_default_until_hidden() {
        assert!(shows_chat(Picked::Nothing, false));
        assert!(!shows_chat(Picked::Nothing, true));
        assert!(
            shows_chat(Picked::King, true),
            "selecting him always shows it"
        );
        assert!(!shows_chat(Picked::Other, false));
    }

    #[test]
    fn quick_actions_sit_above_the_conversation_or_at_the_right() {
        assert_eq!(quick_actions_x(0.0, 1000.0, 200.0, Some(600.0)), 600.0);
        assert_eq!(quick_actions_x(0.0, 1000.0, 200.0, None), 800.0);
        // A narrow column still leaves the buttons room inside the bar.
        assert_eq!(quick_actions_x(0.0, 1000.0, 200.0, Some(900.0)), 800.0);
        assert_eq!(quick_actions_x(0.0, 150.0, 200.0, None), 0.0);
    }
}

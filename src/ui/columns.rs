//! The window's side columns: the conversation column on the right (the Queen's chat, or the
//! selected agent's screen and input; the Queen's chat when nothing is selected, unless ⌘J hid
//! it) and, just left of it, the details column (the selected node's attributes and actions),
//! which collapses to a strip.

use serde::{Deserialize, Serialize};

/// Below this window width (points) the details column starts collapsed, leaving the graph
/// and the conversation their room.
pub const NARROW_WIDTH: f32 = 1400.0;
/// Width of the collapsed details column's strip.
pub const STRIP_WIDTH: f32 = 28.0;
pub const DETAILS_DEFAULT_WIDTH: f32 = 270.0;
pub const DETAILS_MIN_WIDTH: f32 = 190.0;
pub const DETAILS_MAX_WIDTH: f32 = 480.0;
/// The conversation column's first-launch width, as a share of the window.
pub const CONVERSATION_SHARE: f32 = 0.45;
pub const CONVERSATION_MIN_WIDTH: f32 = 300.0;

/// How the user last left the details column; persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DetailsPref {
    /// Open unless the window is narrow.
    #[default]
    Auto,
    Open,
    Collapsed,
}

/// How the details column is drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsColumn {
    /// Nothing selected that has details.
    Hidden,
    /// A strip with a button to open it.
    Collapsed,
    Open,
}

/// The details column for `pref`, given whether the selection has details and the window's
/// width.
pub fn details_column(pref: DetailsPref, has_details: bool, window_width: f32) -> DetailsColumn {
    if !has_details {
        return DetailsColumn::Hidden;
    }
    match pref {
        DetailsPref::Open => DetailsColumn::Open,
        DetailsPref::Collapsed => DetailsColumn::Collapsed,
        DetailsPref::Auto if window_width < NARROW_WIDTH => DetailsColumn::Collapsed,
        DetailsPref::Auto => DetailsColumn::Open,
    }
}

/// The preference after the user clicks the column's collapse or expand button.
pub fn toggled(shown: DetailsColumn) -> DetailsPref {
    match shown {
        DetailsColumn::Open => DetailsPref::Collapsed,
        DetailsColumn::Collapsed | DetailsColumn::Hidden => DetailsPref::Open,
    }
}

/// What the selection is, for the conversation column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    Nothing,
    Queen,
    /// One agent, or several nodes.
    Other,
}

/// Whether the conversation column shows the Queen's chat: when she is selected, or when
/// nothing is and the user has not hidden it.
pub fn shows_chat(picked: Picked, chat_hidden: bool) -> bool {
    match picked {
        Picked::Queen => true,
        Picked::Nothing => !chat_hidden,
        Picked::Other => false,
    }
}

/// Where the top bar's Queen quick actions (`width` wide) start: above the conversation
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

    const WIDE: f32 = 1728.0;
    const NARROW: f32 = 1280.0;

    #[test]
    fn hidden_without_details_whatever_the_preference() {
        for pref in [DetailsPref::Auto, DetailsPref::Open, DetailsPref::Collapsed] {
            assert_eq!(details_column(pref, false, WIDE), DetailsColumn::Hidden);
        }
    }

    #[test]
    fn auto_collapses_on_narrow_windows() {
        assert_eq!(
            details_column(DetailsPref::Auto, true, WIDE),
            DetailsColumn::Open
        );
        assert_eq!(
            details_column(DetailsPref::Auto, true, NARROW),
            DetailsColumn::Collapsed
        );
    }

    #[test]
    fn explicit_choice_wins_over_width() {
        assert_eq!(
            details_column(DetailsPref::Open, true, NARROW),
            DetailsColumn::Open
        );
        assert_eq!(
            details_column(DetailsPref::Collapsed, true, WIDE),
            DetailsColumn::Collapsed
        );
    }

    #[test]
    fn toggling_flips_what_is_shown() {
        let collapsed = details_column(DetailsPref::Auto, true, NARROW);
        let pref = toggled(collapsed);
        assert_eq!(details_column(pref, true, NARROW), DetailsColumn::Open);
        let pref = toggled(details_column(pref, true, NARROW));
        assert_eq!(details_column(pref, true, WIDE), DetailsColumn::Collapsed);
    }

    #[test]
    fn chat_shows_by_default_until_hidden() {
        assert!(shows_chat(Picked::Nothing, false));
        assert!(!shows_chat(Picked::Nothing, true));
        assert!(
            shows_chat(Picked::Queen, true),
            "selecting her always shows it"
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

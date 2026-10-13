//! System-wide hotkeys: one brings the Stronghold window to the front, the other (optional,
//! configurable) is held to talk to the King ([`TalkKey`] press and release).
//!
//! The hotkey fires while another app has focus. Its event handler forwards the event
//! to a channel and wakes egui, so [`Hotkey::poll`] runs even when the app is idle,
//! hidden or minimized (eframe keeps calling `App::logic` for invisible windows).

use std::time::Duration;

use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, ViewportCommand};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// Modifiers of the show-window hotkey: Cmd+Shift on macOS, Ctrl+Shift elsewhere.
#[cfg(target_os = "macos")]
pub const HOTKEY_MODIFIERS: Modifiers = Modifiers::SUPER.union(Modifiers::SHIFT);
#[cfg(not(target_os = "macos"))]
pub const HOTKEY_MODIFIERS: Modifiers = Modifiers::CONTROL.union(Modifiers::SHIFT);

/// Key of the show-window hotkey.
pub const HOTKEY_CODE: Code = Code::KeyM;

/// How many frames to keep re-sending `Focus` after the hotkey fires. Un-minimizing
/// animates on macOS, and winit ignores `Focus` while the window is still minimized.
const FOCUS_RETRIES: u32 = 20;

/// Delay between focus retries.
const FOCUS_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// What a hotkey event asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    ShowWindow,
    Ignore,
}

/// The talk-to-the-King hotkey went down or up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TalkKey {
    Press,
    Release,
}

/// Maps a hotkey event to a talk key event, if it is the talk hotkey's.
pub fn talk_key_for(event_id: u32, state: HotKeyState, talk_id: Option<u32>) -> Option<TalkKey> {
    (Some(event_id) == talk_id).then_some(match state {
        HotKeyState::Pressed => TalkKey::Press,
        HotKeyState::Released => TalkKey::Release,
    })
}

/// Maps a hotkey event to an action. Only the press of our own hotkey shows the window.
pub fn action_for(event_id: u32, state: HotKeyState, our_id: u32) -> HotkeyAction {
    if event_id == our_id && state == HotKeyState::Pressed {
        HotkeyAction::ShowWindow
    } else {
        HotkeyAction::Ignore
    }
}

/// The default show-window hotkey.
pub fn default_hotkey() -> HotKey {
    HotKey::new(Some(HOTKEY_MODIFIERS), HOTKEY_CODE)
}

/// Viewport commands for one frame, and the retries left afterwards.
///
/// `fired` is whether the hotkey was pressed since the last frame, `focused` whether the
/// window currently has focus.
pub fn commands_for_frame(
    fired: bool,
    focused: bool,
    retries_left: u32,
) -> (Vec<ViewportCommand>, u32) {
    if fired {
        let commands = vec![
            ViewportCommand::Minimized(false),
            ViewportCommand::Visible(true),
            ViewportCommand::Focus,
        ];
        return (commands, FOCUS_RETRIES);
    }
    if focused || retries_left == 0 {
        return (Vec::new(), 0);
    }
    (vec![ViewportCommand::Focus], retries_left - 1)
}

/// A registered global hotkey. Unregisters and clears the event handler on drop.
pub struct Hotkey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
    events: Receiver<GlobalHotKeyEvent>,
    focus_retries_left: u32,
    /// The registered talk hotkey.
    talk: Option<HotKey>,
    /// Talk key events not yet taken.
    talk_keys: Vec<TalkKey>,
}

impl Hotkey {
    /// Registers the default hotkey. Must be called on the main thread (macOS requirement);
    /// `ctx` is woken whenever a hotkey event arrives.
    pub fn register(ctx: &egui::Context) -> Result<Self> {
        let manager = GlobalHotKeyManager::new().context("creating global hotkey manager")?;
        let hotkey = default_hotkey();
        manager
            .register(hotkey)
            .with_context(|| format!("registering global hotkey {hotkey}"))?;

        let (tx, events): (Sender<GlobalHotKeyEvent>, _) = crossbeam_channel::unbounded();
        let ctx = ctx.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let _ = tx.send(event);
            ctx.request_repaint();
        }));

        log::info!("registered global hotkey {hotkey}");
        Ok(Self {
            manager,
            hotkey,
            events,
            focus_retries_left: 0,
            talk: None,
            talk_keys: Vec::new(),
        })
    }

    /// Drains hotkey events and, if the hotkey fired, shows, un-minimizes and focuses the
    /// root window. Call once per frame from `App::logic`.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let our_id = self.hotkey.id();
        let talk_id = self.talk.map(|h| h.id());
        let mut fired = false;
        for event in self.events.try_iter() {
            fired |= action_for(event.id(), event.state(), our_id) == HotkeyAction::ShowWindow;
            if let Some(key) = talk_key_for(event.id(), event.state(), talk_id) {
                self.talk_keys.push(key);
            }
        }
        if fired {
            log::debug!("global hotkey fired, showing window");
        }

        let focused = ctx.input(|i| i.viewport().focused == Some(true));
        let (commands, retries_left) = commands_for_frame(fired, focused, self.focus_retries_left);
        self.focus_retries_left = retries_left;
        for command in commands {
            ctx.send_viewport_cmd(command);
        }
        if retries_left > 0 {
            ctx.request_repaint_after(FOCUS_RETRY_INTERVAL);
        }
    }
}

impl Hotkey {
    /// Registers `hotkey` as the talk hotkey, replacing the previous one (`None` removes it).
    /// On failure (e.g. another app holds the combination) no talk hotkey is registered.
    pub fn set_talk_hotkey(&mut self, hotkey: Option<HotKey>) -> Result<()> {
        if hotkey.map(|h| h.id()) == self.talk.map(|h| h.id()) {
            return Ok(());
        }
        if let Some(old) = self.talk.take()
            && let Err(err) = self.manager.unregister(old)
        {
            log::warn!("failed to unregister talk hotkey {old}: {err}");
        }
        self.talk_keys.clear();
        let Some(hotkey) = hotkey else {
            return Ok(());
        };
        self.manager
            .register(hotkey)
            .with_context(|| format!("registering talk hotkey {hotkey}"))?;
        log::info!("registered talk hotkey {hotkey}");
        self.talk = Some(hotkey);
        Ok(())
    }

    /// Talk key events since the last call (filled by [`poll`](Self::poll)).
    pub fn take_talk_keys(&mut self) -> Vec<TalkKey> {
        std::mem::take(&mut self.talk_keys)
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        GlobalHotKeyEvent::set_event_handler(None::<fn(GlobalHotKeyEvent)>);
        if let Some(talk) = self.talk
            && let Err(err) = self.manager.unregister(talk)
        {
            log::warn!("failed to unregister talk hotkey: {err}");
        }
        if let Err(err) = self.manager.unregister(self.hotkey) {
            log::warn!("failed to unregister global hotkey: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_of_our_hotkey_shows_window() {
        let id = default_hotkey().id();
        assert_eq!(
            action_for(id, HotKeyState::Pressed, id),
            HotkeyAction::ShowWindow
        );
    }

    #[test]
    fn release_is_ignored() {
        let id = default_hotkey().id();
        assert_eq!(
            action_for(id, HotKeyState::Released, id),
            HotkeyAction::Ignore
        );
    }

    #[test]
    fn other_hotkey_is_ignored() {
        let id = default_hotkey().id();
        let other = HotKey::new(Some(HOTKEY_MODIFIERS), Code::KeyK).id();
        assert_eq!(
            action_for(other, HotKeyState::Pressed, id),
            HotkeyAction::Ignore
        );
    }

    #[test]
    fn default_hotkey_is_shift_m_with_platform_modifier() {
        #[cfg(target_os = "macos")]
        assert_eq!(default_hotkey().to_string(), "shift+super+KeyM");
        assert!(default_hotkey().matches(HOTKEY_MODIFIERS, Code::KeyM));
    }

    #[test]
    fn talk_hotkey_maps_press_and_release() {
        let talk: HotKey = crate::voice::settings::DEFAULT_TALK_HOTKEY.parse().unwrap();
        let talk_id = Some(talk.id());
        assert_eq!(
            talk_key_for(talk.id(), HotKeyState::Pressed, talk_id),
            Some(TalkKey::Press)
        );
        assert_eq!(
            talk_key_for(talk.id(), HotKeyState::Released, talk_id),
            Some(TalkKey::Release)
        );
        let show = default_hotkey().id();
        assert_eq!(talk_key_for(show, HotKeyState::Pressed, talk_id), None);
        assert_eq!(talk_key_for(talk.id(), HotKeyState::Pressed, None), None);
        // The talk hotkey never shows the window.
        assert_eq!(
            action_for(talk.id(), HotKeyState::Pressed, show),
            HotkeyAction::Ignore
        );
        assert!(talk.matches(HOTKEY_MODIFIERS, Code::KeyE));
    }

    #[test]
    fn firing_shows_unminimizes_and_focuses() {
        let (commands, retries) = commands_for_frame(true, false, 0);
        assert_eq!(
            commands,
            vec![
                ViewportCommand::Minimized(false),
                ViewportCommand::Visible(true),
                ViewportCommand::Focus,
            ]
        );
        assert_eq!(retries, FOCUS_RETRIES);
    }

    #[test]
    fn retries_focus_until_focused() {
        let (commands, retries) = commands_for_frame(false, false, 3);
        assert_eq!(commands, vec![ViewportCommand::Focus]);
        assert_eq!(retries, 2);

        let (commands, retries) = commands_for_frame(false, true, 2);
        assert!(commands.is_empty());
        assert_eq!(retries, 0);
    }

    #[test]
    fn retries_run_out() {
        let (commands, retries) = commands_for_frame(false, false, 0);
        assert!(commands.is_empty());
        assert_eq!(retries, 0);
    }

    #[test]
    fn idle_frame_does_nothing() {
        let (commands, retries) = commands_for_frame(false, true, 0);
        assert!(commands.is_empty());
        assert_eq!(retries, 0);
    }
}

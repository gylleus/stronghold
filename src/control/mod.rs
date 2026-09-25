//! Local control API: a Unix socket in the data dir that serves the fleet status and accepts
//! dashboard and agent actions. Protocol and methods: `docs/control-api.md`.

pub mod client;
pub mod commands;
pub mod protocol;
pub mod server;
#[cfg(test)]
mod tests;
pub mod view;

use std::path::PathBuf;

pub use client::ControlClient;
pub use commands::{
    AppCommands, CommandHandler, ControlCommand, FilterChange, PendingCommand, Reply, TerminalKey,
};
pub use protocol::{ControlError, ErrorCode, PROTOCOL_VERSION, Request, Response};
pub use server::{Backend, ControlServer, ScreenRead, ScreenReader};
pub use view::{SharedStatus, StatusView, ViewInputs};

/// File name of the socket inside the data dir.
pub const SOCKET_FILE: &str = "control.sock";

/// `<data dir>/control.sock`; the data dir honours `$MYRSTACK_DATA_DIR`.
pub fn socket_path() -> PathBuf {
    crate::claude::hook::data_dir().join(SOCKET_FILE)
}

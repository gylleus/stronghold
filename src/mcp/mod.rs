//! `myrstack mcp`: an MCP server on stdin/stdout that proxies to the running app's control API,
//! so a chat client (Claude Code etc.) can read the fleet and drive the dashboard.
//!
//! It also serves the fleet playbook (prompts, resources, tools) straight from disk.
//!
//! Hand-rolled rather than built on `rmcp`: the server only needs `initialize`, `ping`,
//! `tools/*`, `prompts/*` and `resources/*` over newline-delimited JSON-RPC, and the app is synchronous, so
//! an async runtime would add weight without buying anything. Tools and setup: `docs/mcp.md`.

pub mod backend;
pub mod lifecycle;
pub mod notify;
pub mod playbook;
pub mod review;
pub mod server;
#[cfg(test)]
mod tests;
pub mod tools;

use std::io;

use anyhow::Result;

pub use backend::{CallError, Control, SocketControl};
pub use server::{McpServer, PROTOCOL_VERSIONS};

/// `myrstack mcp`: serves MCP on stdio until stdin closes; `None` for other subcommands.
pub fn run_cli(args: &[String]) -> Option<Result<()>> {
    let (cmd, _) = args.split_first()?;
    (cmd == "mcp").then(run_stdio)
}

fn run_stdio() -> Result<()> {
    // stdout carries the protocol, so logs go to stderr only (env_logger's default target).
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .try_init();
    let path = crate::control::socket_path();
    log::info!("myrstack mcp: proxying to {}", path.display());
    let mut server = McpServer::new(SocketControl::new(path));
    server.serve(io::BufReader::new(io::stdin()), io::stdout().lock())?;
    Ok(())
}

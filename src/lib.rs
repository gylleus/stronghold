//! Myrstack: a live farm view of the Orca agent clan.

// Stub modules are filled in by later milestones.
#![allow(dead_code)]

pub mod actions;
pub mod app;
pub mod ask;
pub mod broadcast;
pub mod chat;
pub mod clan;
pub mod claude;
pub mod close;
pub mod codex;
pub mod control;
pub mod detect;
pub mod envoy;
pub mod finish;
pub mod health;
pub mod hotkey;
pub mod logfile;
pub mod mcp;
pub mod model;
pub mod notify;
pub mod orca;
pub mod park;
pub mod persist;
pub mod playbook;
pub mod poller;
pub mod queen_inbox;
pub mod quota;
pub mod review;
pub mod spawn;
pub mod stale;
pub mod traffic;
pub mod ui;
pub mod voice;
pub mod whip;
pub mod worktrees;

pub use app::MyrstackApp;

/// Window and application title.
pub const APP_NAME: &str = "Myrstack";

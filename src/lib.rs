//! Myrstack: a live graph view of the Orca agent fleet.

// Stub modules are filled in by later milestones.
#![allow(dead_code)]

pub mod actions;
pub mod app;
pub mod broadcast;
pub mod chat;
pub mod claude;
pub mod close;
pub mod codex;
pub mod control;
pub mod detect;
pub mod fleet;
pub mod hotkey;
pub mod mcp;
pub mod model;
pub mod notify;
pub mod orca;
pub mod persist;
pub mod playbook;
pub mod poller;
pub mod quota;
pub mod ui;
pub mod whip;

pub use app::MyrstackApp;

/// Window and application title.
pub const APP_NAME: &str = "Myrstack";

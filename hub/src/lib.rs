//! herdr-hub library: a standalone gateway that connects to running herdr
//! servers and exposes one push-based client protocol for non-terminal
//! clients. See SPEC.md and protocol/hub-protocol.md.

pub mod actions;
pub mod app;
pub mod clients;
pub mod config;
pub mod hubproto;
pub mod screen;
pub mod serverconn;
pub mod service;
pub mod state;
pub mod wire;

//! syki-core: shared contracts used by every Syki Lite crate.
//!
//! This crate owns the JSON-RPC message shapes exchanged between the CLI,
//! the GUI shell and the native runtime, plus the runtime environment
//! resolution (instance-scoped data directory, mutex name and WebView2
//! user-data folder). It intentionally has no UI or OS-binding code.

pub mod ipc;
pub mod policy;
pub mod rpc;
pub mod search;
pub mod semindex;
pub mod storage;

/// Version reported by the CLI, the GUI banner and the `info` RPC.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

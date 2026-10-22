//! Shared collectors, state, and presentation; no desktop window handles.
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod monitor;
pub mod platform;
pub mod presentation;
pub mod bridge;
pub mod state;

#[cfg(windows)]
pub mod updater;
#[cfg(windows)]
pub mod windows_update;

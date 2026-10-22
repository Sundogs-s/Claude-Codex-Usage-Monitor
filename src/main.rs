#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

pub use usage_monitor::state;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
#[path = "windows.rs"]
mod windows_app;
#[cfg(windows)]
mod windows_compat;

#[cfg(windows)]
fn main() -> ::windows::core::Result<()> {
    if usage_monitor::updater::dispatch_worker() || usage_monitor::windows_update::dispatch_helper() { return Ok(()); }
    windows_app::run()
}

#[cfg(not(windows))]
fn main() {
    if let Err(error) = usage_monitor::bridge::run() {
        eprintln!("Usage Monitor core: {error}");
        std::process::exit(1);
    }
}

/// Background monitor thread with dynamic refresh interval.
use crate::{claude, codex, state::SharedState};
use log::{error, info};
use reqwest::Client;
use std::{sync::Arc, sync::atomic::{AtomicU64, Ordering}, time::Duration};
use tokio::{sync::Notify, time::sleep};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

// ─── Global refresh control ───────────────────────────────────────────────────

pub static CLAUDE_INTERVAL: AtomicU64 = AtomicU64::new(60);
pub static CODEX_INTERVAL:  AtomicU64 = AtomicU64::new(65);
pub static WAKE_POLLERS: Notify = Notify::const_new();

pub fn set_refresh_secs(secs: u64) {
    CLAUDE_INTERVAL.store(secs, Ordering::Relaxed);
    CODEX_INTERVAL.store(secs + 5, Ordering::Relaxed);
    WAKE_POLLERS.notify_waiters();
}

/// Immediately wake both pollers without changing the refresh interval.
pub fn wake_pollers() {
    WAKE_POLLERS.notify_waiters();
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn notify_repaint(hwnd: HWND) {
    unsafe { let _ = PostMessageW(hwnd, WM_APP, None, None); }
}

// ─── Spawn ───────────────────────────────────────────────────────────────────

pub fn spawn(state: SharedState, hwnd: HWND) {
    let hwnd_usize = hwnd.0 as usize;
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async move {
            let hwnd = HWND(hwnd_usize as *mut _);
            let client = Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("HTTP client");
            tokio::join!(
                poll_claude(Arc::clone(&state), hwnd, client.clone()),
                poll_codex(Arc::clone(&state),  hwnd, client),
            );
        });
    });
}

// ─── Pollers ─────────────────────────────────────────────────────────────────

async fn poll_claude(state: SharedState, hwnd: HWND, client: Client) {
    loop {
        info!("[monitor] polling Claude…");
        let mut changed = false;
        match claude::fetch(&client).await {
            Ok(usage) => {
                let mut s = state.lock();
                if s.claude != usage {
                    s.claude = usage;
                    changed = true;
                }
                if !s.claude_error.is_empty() {
                    s.claude_error = String::new();
                    changed = true;
                }
            }
            Err(e) => {
                error!("[monitor] Claude: {}", e);
                let mut s = state.lock();
                let next_err = format!("Claude: {}", e);
                if s.claude_error != next_err {
                    s.claude_error = next_err;
                    changed = true;
                }
            }
        }
        if changed {
            notify_repaint(hwnd);
        }
        let secs = CLAUDE_INTERVAL.load(Ordering::Relaxed);
        tokio::select! {
            _ = sleep(Duration::from_secs(secs)) => {}
            _ = WAKE_POLLERS.notified() => {}
        }
    }
}

async fn poll_codex(state: SharedState, hwnd: HWND, client: Client) {
    sleep(Duration::from_secs(5)).await;
    loop {
        info!("[monitor] polling Codex…");
        let mut changed = false;
        match codex::fetch(&client).await {
            Ok(usage) => {
                let mut s = state.lock();
                if s.codex != usage {
                    s.codex = usage;
                    changed = true;
                }
                if !s.codex_error.is_empty() {
                    s.codex_error = String::new();
                    changed = true;
                }
            }
            Err(e) => {
                error!("[monitor] Codex: {}", e);
                let mut s = state.lock();
                let next_err = format!("Codex: {}", e);
                if s.codex_error != next_err {
                    s.codex_error = next_err;
                    changed = true;
                }
            }
        }
        if changed {
            notify_repaint(hwnd);
        }
        let secs = CODEX_INTERVAL.load(Ordering::Relaxed);
        tokio::select! {
            _ = sleep(Duration::from_secs(secs)) => {}
            _ = WAKE_POLLERS.notified() => {}
        }
    }
}

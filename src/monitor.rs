/// Background monitor thread with dynamic refresh interval.
use crate::{claude, codex, cursor, state::SharedState};
use log::{error, info};
use reqwest::Client;
use std::{sync::Arc, sync::atomic::{AtomicU64, Ordering}, time::Duration};
use tokio::{sync::Notify, time::sleep};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

// ─── Global refresh control ───────────────────────────────────────────────────

pub static CLAUDE_INTERVAL:  AtomicU64 = AtomicU64::new(60);
pub static CODEX_INTERVAL:   AtomicU64 = AtomicU64::new(65);
pub static CURSOR_INTERVAL:  AtomicU64 = AtomicU64::new(70);
pub static WAKE_POLLERS: Notify = Notify::const_new();

pub fn set_refresh_secs(secs: u64) {
    CLAUDE_INTERVAL.store(secs,       Ordering::Relaxed);
    CODEX_INTERVAL.store(secs + 5,   Ordering::Relaxed);
    CURSOR_INTERVAL.store(secs + 10, Ordering::Relaxed);
    WAKE_POLLERS.notify_waiters();
}

/// Immediately wake all pollers without changing the refresh interval.
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
                poll_codex(Arc::clone(&state),  hwnd, client.clone()),
                poll_cursor(Arc::clone(&state), hwnd, client),
            );
        });
    });
}

// ─── Pollers ─────────────────────────────────────────────────────────────────

/// The Claude usage endpoint rate-limits aggressively: never poll it faster than this.
/// Measured 2026-09-03 (Max 5x): a request 60 s after a success got 429, 120 s got 200.
const CLAUDE_MIN_INTERVAL: u64 = 120;
/// First back-off step after a 429, and the cap for doubling.
const CLAUDE_BACKOFF_START: u64 = 240;
const CLAUDE_BACKOFF_MAX:   u64 = 900;

async fn poll_claude(state: SharedState, hwnd: HWND, client: Client) {
    let mut backoff: u64 = 0;
    loop {
        info!("[monitor] polling Claude…");
        let mut changed = false;
        match claude::fetch(&client).await {
            Ok(usage) => {
                backoff = 0;
                let mut s = state.lock();
                if s.claude != usage {
                    s.claude = usage;
                    changed = true;
                }
                if s.claude_stale || s.claude_next_retry.is_some() {
                    s.claude_stale = false;
                    s.claude_next_retry = None;
                    changed = true;
                }
                if !s.claude_error.is_empty() {
                    s.claude_error = String::new();
                    changed = true;
                }
            }
            Err(claude::FetchError::RateLimited) => {
                backoff = if backoff == 0 {
                    CLAUDE_BACKOFF_START.max(CLAUDE_INTERVAL.load(Ordering::Relaxed))
                } else {
                    (backoff * 2).min(CLAUDE_BACKOFF_MAX)
                };
                let retry_at = chrono::Utc::now() + chrono::Duration::seconds(backoff as i64);
                info!("[monitor] Claude usage API rate-limited — keeping last data, retry in {}s", backoff);
                let mut s = state.lock();
                if s.claude.rows.is_empty() {
                    s.claude_error = "Claude: usage API rate-limited".to_string();
                } else {
                    s.claude_stale = true;
                    s.claude_error = String::new();
                }
                s.claude_next_retry = Some(retry_at);
                changed = true;
            }
            Err(e) => {
                backoff = 0;
                error!("[monitor] Claude: {}", e);
                let mut s = state.lock();
                if matches!(e, claude::FetchError::Auth(_)) && !s.claude.rows.is_empty() {
                    // Credentials are gone: the old numbers are meaningless, blank them.
                    s.claude.rows.clear();
                    changed = true;
                }
                s.claude_stale = false;
                s.claude_next_retry = None;
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
        let secs = if backoff > 0 {
            backoff
        } else {
            CLAUDE_INTERVAL.load(Ordering::Relaxed).max(CLAUDE_MIN_INTERVAL)
        };
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

async fn poll_cursor(state: SharedState, hwnd: HWND, client: Client) {
    // Stagger start by 10 s so all three pollers don't hit the network simultaneously.
    sleep(Duration::from_secs(10)).await;
    loop {
        info!("[monitor] polling Cursor…");
        let mut changed = false;

        // Only hit the API when the section is visible.
        let show_cursor = state.lock().settings.show_cursor;
        if show_cursor {
            match cursor::fetch(&client).await {
                Ok(usage) => {
                    let mut s = state.lock();
                    if s.cursor != usage {
                        s.cursor = usage;
                        changed = true;
                    }
                    if !s.cursor_error.is_empty() {
                        s.cursor_error = String::new();
                        changed = true;
                    }
                }
                Err(e) => {
                    error!("[monitor] Cursor: {}", e);
                    let mut s = state.lock();
                    let next_err = e.to_string();
                    if s.cursor_error != next_err {
                        s.cursor_error = next_err;
                        changed = true;
                    }
                }
            }
        }

        if changed { notify_repaint(hwnd); }
        let secs = CURSOR_INTERVAL.load(Ordering::Relaxed);
        tokio::select! {
            _ = sleep(Duration::from_secs(secs)) => {}
            _ = WAKE_POLLERS.notified() => {}
        }
    }
}

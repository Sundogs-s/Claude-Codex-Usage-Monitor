/// Shared application state — updated by background monitor thread,
/// read by the Win32 WM_PAINT handler.
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ─── Usage data ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowUsage {
    pub utilization_5h: Option<f32>,
    pub utilization_7d: Option<f32>,
    pub reset_5h:       Option<DateTime<Utc>>,
    pub reset_7d:       Option<DateTime<Utc>>,
}

/// Cursor personal-plan usage snapshot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CursorUsage {
    /// Auto (cheaper) model budget used, 0.0–1.0.  From autoPercentUsed / 100.
    pub auto_usage_pct: Option<f32>,
    /// Named-model (API/premium) budget used, 0.0–1.0.  From apiPercentUsed / 100.
    pub api_usage_pct:  Option<f32>,
    /// Billing cycle end date.
    pub reset_date:     Option<DateTime<Utc>>,
    /// User email / name (best-effort).
    pub user_email:     Option<String>,
}

// ─── Settings ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DisplayMode {
    /// Draggable, resizable semi-transparent overlay (always on top)
    Floating,
    /// Compact bar floating above the taskbar (full monitor width)
    CompactBar,
    /// Embedded in the Windows taskbar via AppBar API
    AppBar,
}
impl Default for DisplayMode { fn default() -> Self { Self::Floating } }

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RefreshRate { Secs5, Min1, Min5 }
impl Default for RefreshRate { fn default() -> Self { Self::Min1 } }
impl RefreshRate {
    pub fn secs(self) -> u64 {
        match self { Self::Secs5 => 5, Self::Min1 => 60, Self::Min5 => 300 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub display_mode:    DisplayMode,
    /// Index into the monitor list (0 = primary).
    #[serde(default)]
    pub monitor_idx:     usize,
    #[serde(default)]
    pub refresh_rate:    RefreshRate,
    #[serde(default)]
    pub auto_start:      bool,
    /// Whether the floating window shows a taskbar button.
    #[serde(default)]
    pub show_in_taskbar: bool,
    #[serde(default)]
    pub hover_auto_hide: bool,
    /// Whether to show Claude section (right-click menu toggle).
    #[serde(default = "default_true")]
    pub show_claude:     bool,
    /// Whether to show Codex section (right-click menu toggle).
    #[serde(default = "default_true")]
    pub show_codex:      bool,
    /// Whether to show Cursor section (right-click menu toggle).
    #[serde(default = "default_true")]
    pub show_cursor:     bool,
    /// Saved position for floating mode (-1 = auto-position).
    #[serde(default = "default_neg")]
    pub win_x: i32,
    #[serde(default = "default_neg")]
    pub win_y: i32,
    /// Saved size — width is fixed at DEFAULT_W; height auto-computed
    /// when any section toggle changes (win_h = -1 signals auto mode).
    #[serde(default = "default_win_w")]
    pub win_w: i32,
    #[serde(default = "default_neg")]
    pub win_h: i32,
}

fn default_true()  -> bool { true }
fn default_neg()   -> i32  { -1 }
fn default_win_w() -> i32  { 320 }

impl Default for Settings {
    fn default() -> Self {
        Self {
            display_mode:    DisplayMode::Floating,
            monitor_idx:     0,
            refresh_rate:    RefreshRate::Min1,
            auto_start:      false,
            show_in_taskbar: false,
            hover_auto_hide: false,
            show_claude:     true,
            show_codex:      true,
            show_cursor:     true,
            win_x: -1, win_y: -1,
            win_w: 320, win_h: -1,   // -1 → auto-compute on first paint
        }
    }
}

// ─── Window height auto-sizing ────────────────────────────────────────────────
//
// Layout spec (floating mode):
//   header:            28 px
//   per-section:       80 px  (label row + 2 progress rows + padding)
//   divider gap:        8 px  (between sections; N-1 dividers for N sections)
//   bottom padding:    12 px
//
// Minimum height: 132 px (single section, matches MIN_H in main.rs)

pub const SECTION_H:    i32 = 80;
pub const HEADER_H:     i32 = 28;
pub const DIVIDER_GAP:  i32 = 8;
pub const BOTTOM_PAD:   i32 = 12;

/// Compute the preferred floating-window height for the currently enabled sections.
/// Returns at least 132 (MIN_H) so the window is never empty.
pub fn calc_window_height(settings: &Settings) -> i32 {
    let n = [settings.show_claude, settings.show_codex, settings.show_cursor]
        .iter()
        .filter(|&&v| v)
        .count() as i32;

    if n == 0 {
        return 132;
    }

    let h = HEADER_H + n * SECTION_H + (n - 1) * DIVIDER_GAP + BOTTOM_PAD;
    h.max(132)
}

// ─── Monitor info ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MonitorInfo {
    /// Full monitor rect (pixels).
    pub rect:      (i32, i32, i32, i32),  // left, top, right, bottom
    /// Work area (excluding taskbar).
    pub work_rect: (i32, i32, i32, i32),
}

// ─── App state ────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct AppState {
    pub claude:        WindowUsage,
    pub codex:         WindowUsage,
    pub cursor:        CursorUsage,
    pub claude_error:  String,
    pub codex_error:   String,
    pub cursor_error:  String,
    pub floating_hover_hidden: bool,
    pub settings:      Settings,
    pub monitors:      Vec<MonitorInfo>,
}

pub type SharedState = Arc<Mutex<AppState>>;

pub fn new_shared() -> SharedState {
    Arc::new(Mutex::new(AppState::default()))
}

// ─── Settings persistence ─────────────────────────────────────────────────────

pub fn settings_path() -> std::path::PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(appdata)
        .join("UsageMonitor")
        .join("settings.json")
}

pub fn load_settings() -> Settings {
    let path = settings_path();
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(s) = serde_json::from_str::<Settings>(&data) {
            return s;
        }
    }
    Settings::default()
}

pub fn save_settings(s: &Settings) {
    let path = settings_path();
    if let Some(p) = path.parent() { let _ = std::fs::create_dir_all(p); }
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(&path, json);
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

pub fn secs_until(reset: Option<DateTime<Utc>>) -> i64 {
    reset.map(|t| (t - Utc::now()).num_seconds().max(0)).unwrap_or(0)
}

pub fn fmt_countdown(secs: i64) -> String {
    if secs <= 0  { return "—".to_string(); }
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86400, (secs % 86400) / 3600)
    }
}

pub fn fmt_pct(u: Option<f32>) -> String {
    match u {
        Some(v) => format!("{:.0}%", v * 100.0),
        None => "–".to_string(),
    }
}

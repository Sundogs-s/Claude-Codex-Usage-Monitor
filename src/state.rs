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

/// One rate-limit window row (e.g. "5h", "7d", "Fable").
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRow {
    pub label:       String,
    /// 0.0–1.0
    pub utilization: Option<f32>,
    pub reset:       Option<DateTime<Utc>>,
}

/// Claude plan usage: a dynamic list of windows (5h, 7d, per-model weekly, extra usage).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeUsage {
    pub rows:       Vec<UsageRow>,
    /// When `rows` was last fetched successfully (None = never).
    pub fetched_at: Option<DateTime<Utc>>,
    /// Plan label for the heading meta text, e.g. "Max 5x".
    pub tier:       Option<String>,
}

impl ClaudeUsage {
    pub fn util(&self, label: &str) -> Option<f32> {
        self.rows.iter().find(|r| r.label == label).and_then(|r| r.utilization)
    }
    /// Seconds since the last successful fetch (None = never fetched).
    pub fn age_secs(&self) -> Option<i64> {
        self.fetched_at.map(|t| (Utc::now() - t).num_seconds().max(0))
    }
}

/// Human-readable age: "now", "3m ago", "2h ago".
pub fn fmt_age(secs: Option<i64>) -> String {
    match secs {
        None => "–".to_string(),
        Some(s) if s < 60 => "now".to_string(),
        Some(s) if s < 3600 => format!("{}m ago", s / 60),
        Some(s) if s < 86400 => format!("{}h ago", s / 3600),
        Some(s) => format!("{}d ago", s / 86400),
    }
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

/// Visual theme (switchable from the tray menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    /// v0.4 look: rounded dark panel, gradient bars, Segoe UI headings.
    Classic,
    /// v0.5 "Neon HUD": 8px pixel grid, segmented glowing bars, cyan structure lines.
    Neon,
}
impl Default for Theme { fn default() -> Self { Self::Neon } }
impl Theme {
    /// Default floating-window width for the theme (used when `win_w` is -1 = auto).
    pub fn default_width(self) -> i32 {
        match self { Theme::Classic => 320, Theme::Neon => 240 }
    }
}

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
    #[serde(default)]
    pub theme:           Theme,
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
    /// Saved size — width -1 = theme default (see `effective_win_w`); height auto-computed
    /// when any section toggle changes (win_h = -1 signals auto mode).
    #[serde(default = "default_win_w")]
    pub win_w: i32,
    #[serde(default = "default_neg")]
    pub win_h: i32,
}

fn default_true()  -> bool { true }
fn default_neg()   -> i32  { -1 }
fn default_win_w() -> i32  { -1 }

/// Floating-window width: the saved value, or the theme default when unset (-1).
pub fn effective_win_w(s: &Settings) -> i32 {
    if s.win_w < 0 { s.theme.default_width() } else { s.win_w }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            display_mode:    DisplayMode::Floating,
            theme:           Theme::Neon,
            monitor_idx:     0,
            refresh_rate:    RefreshRate::Min1,
            auto_start:      false,
            show_in_taskbar: false,
            hover_auto_hide: false,
            show_claude:     true,
            show_codex:      true,
            show_cursor:     true,
            win_x: -1, win_y: -1,
            win_w: -1, win_h: -1,    // -1 → theme default width / auto height
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
/// Extra height per data row beyond the 2-row baseline (row 22px + gap 5px).
pub const ROW_PITCH:    i32 = 27;
pub const HEADER_H:     i32 = 28;
pub const DIVIDER_GAP:  i32 = 8;
pub const BOTTOM_PAD:   i32 = 12;
/// Bottom banner (stale / error line) height when shown.
pub const BANNER_H:     i32 = 16;

/// Height of a section holding `rows` data rows (2 rows = SECTION_H). Classic theme.
pub fn section_height(rows: usize) -> i32 {
    SECTION_H + (rows.max(2) as i32 - 2) * ROW_PITCH
}

// ─── Neon HUD metrics — everything is a multiple of the 8px pixel grid ────────
//
//   header 24 | section: pad 8 + head 16 + rows×16 + pad 8 | divider gap 8 | banner 16 | foot 8
//   window 240 wide: pad 16 | label 32 | bar 80 | pct 32 | eta 40 | pad 16 (gaps 8)
pub const NEON_HEADER_H:    i32 = 24;
pub const NEON_SEC_PAD:     i32 = 8;
pub const NEON_HEAD_H:      i32 = 16;
pub const NEON_ROW_H:       i32 = 16;
pub const NEON_DIVIDER_GAP: i32 = 8;
pub const NEON_BANNER_H:    i32 = 16;
pub const NEON_BOTTOM_PAD:  i32 = 8;

pub fn neon_section_height(rows: usize) -> i32 {
    NEON_SEC_PAD * 2 + NEON_HEAD_H + rows.max(2) as i32 * NEON_ROW_H
}

/// Layout inputs derived from live state (not settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayoutInfo {
    /// Number of rows in the Claude section (>= 2).
    pub claude_rows: usize,
    /// Whether a bottom banner (stale / error text) is shown.
    pub banner:      bool,
}

/// AppBar column widths at 96 dpi for the visible sections.
/// Classic: Claude 200 (three rows), others 180. Neon: every column 176 (stripe 4 + 9 + rows 155 + 8).
pub const APPBAR_COL_W:        i32 = 180;
pub const APPBAR_CLAUDE_COL_W: i32 = 200;
pub const NEON_APPBAR_COL_W:   i32 = 176;

pub fn appbar_col_widths(settings: &Settings) -> Vec<i32> {
    let (claude_w, other_w) = match settings.theme {
        Theme::Classic => (APPBAR_CLAUDE_COL_W, APPBAR_COL_W),
        Theme::Neon    => (NEON_APPBAR_COL_W, NEON_APPBAR_COL_W),
    };
    let mut v = Vec::new();
    if settings.show_claude { v.push(claude_w); }
    if settings.show_codex  { v.push(other_w); }
    if settings.show_cursor { v.push(other_w); }
    v
}

/// Compute the preferred floating-window height for the currently enabled sections.
/// Returns at least 132 (MIN_H) so the window is never empty.
pub fn calc_window_height(settings: &Settings, layout: LayoutInfo) -> i32 {
    let neon = settings.theme == Theme::Neon;
    let sec_h = |rows: usize| if neon { neon_section_height(rows) } else { section_height(rows) };

    let mut heights: Vec<i32> = Vec::new();
    if settings.show_claude { heights.push(sec_h(layout.claude_rows)); }
    if settings.show_codex  { heights.push(sec_h(2)); }
    if settings.show_cursor { heights.push(sec_h(2)); }

    let n = heights.len() as i32;
    if n == 0 {
        return 132;
    }

    let (header, gap, banner_h, foot) = if neon {
        (NEON_HEADER_H, NEON_DIVIDER_GAP, NEON_BANNER_H, NEON_BOTTOM_PAD)
    } else {
        (HEADER_H, DIVIDER_GAP, BANNER_H, BOTTOM_PAD)
    };
    let banner = if layout.banner { banner_h } else { 0 };
    let h = header + heights.iter().sum::<i32>() + (n - 1) * gap + banner + foot;
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
    pub claude:        ClaudeUsage,
    pub codex:         WindowUsage,
    pub cursor:        CursorUsage,
    /// Claude rows are the last-known values; the usage API is currently rate-limited.
    pub claude_stale:  bool,
    /// Next Claude retry time while backing off (shown in the stale banner).
    pub claude_next_retry: Option<DateTime<Utc>>,
    pub claude_error:  String,
    pub codex_error:   String,
    pub cursor_error:  String,
    pub floating_hover_hidden: bool,
    pub settings:      Settings,
    pub monitors:      Vec<MonitorInfo>,
}

impl AppState {
    /// Layout inputs for the floating window derived from the live data.
    pub fn layout(&self) -> LayoutInfo {
        let claude_rows = self.claude.rows.len().max(2);
        let claude_banner = self.settings.show_claude
            && (self.claude_stale || !self.claude_error.is_empty());
        let other_banner = (self.settings.show_codex && !self.codex_error.is_empty())
            || (self.settings.show_cursor && !self.cursor_error.is_empty());
        LayoutInfo { claude_rows, banner: claude_banner || other_banner }
    }
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

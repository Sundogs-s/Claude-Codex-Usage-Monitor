// usage-monitor v2 — Windows system-tray + floating overlay for Claude + Codex usage
//
// Architecture:
//   • System tray icon (Shell_NotifyIconW) — dynamic dual progress-bar icon
//   • Left-click tray  → show / hide overlay
//   • Right-click tray → context menu with show/hide per service + mode select
//   • Floating mode:    draggable (HTCAPTION anywhere), resizable (edge NCHITTEST)
//   • AppBar mode:      300×taskbar-height bar embedded via SHAppBarMessage
//   • WM_TIMER (1 s)  → countdown repaint
//   • WM_APP          → monitor thread data-ready repaint + tray icon refresh

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod claude;
mod codex;
mod cursor;
mod monitor;
mod overlay;
mod state;

use log::{info, LevelFilter};
use monitor::set_refresh_secs;
use simplelog::{CombinedLogger, Config, SharedLogger};
#[cfg(debug_assertions)]
use simplelog::{ColorChoice, TermLogger, TerminalMode};
use state::{
    appbar_col_widths, calc_window_height, load_settings, new_shared, save_settings,
    DisplayMode, LayoutInfo, MonitorInfo, RefreshRate, Settings, SharedState, APPBAR_COL_W,
};
use std::fs::OpenOptions;
use std::path::PathBuf;
use windows::{
    core::{w, Result, PCWSTR},
    Win32::{
        Foundation::{BOOL, COLORREF, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            CreateSolidBrush, EnumDisplayMonitors, GetMonitorInfoW, HDC, InvalidateRect,
            SetWindowRgn, HMONITOR, MONITORINFO,
        },
        System::{
            DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalLock, GlobalUnlock},
            Registry::{
                RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
                HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
            },
            Threading::{GetCurrentProcess, TerminateProcess},
        },
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NIM_MODIFY, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::*,
        },
    },
};

// ─── Constants ────────────────────────────────────────────────────────────────
const DEF_W: i32      = 320;
const MIN_W: i32      = 240;
const MIN_H: i32      = 132;   // single-section minimum
/// System DPI scale (1.0 at 96 dpi). The taskbar band height and the dock fonts scale
/// with DPI, so the column widths must too or the labels get clipped.
fn system_scale() -> f32 {
    unsafe {
        use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
        let hdc = GetDC(None);
        if hdc.0.is_null() { return 1.0; }
        let dpi = GetDeviceCaps(hdc, LOGPIXELSX);
        let _ = ReleaseDC(None, hdc);
        if dpi <= 0 { 1.0 } else { (dpi as f32 / 96.0).clamp(1.0, 4.0) }
    }
}

/// Compute the AppBar width from current settings (sum of visible column widths at 96 dpi:
/// Claude 200px to fit its three rows, others 180px), scaled by the system DPI.
fn appbar_width(settings: &Settings) -> i32 {
    let base = appbar_col_widths(settings).iter().sum::<i32>().max(APPBAR_COL_W);
    (base as f32 * system_scale()).round() as i32
}

/// Layout key of the last WM_APP repaint (claude rows << 1 | banner) — the floating
/// window is resized only when this changes, never on every data refresh.
static LAST_LAYOUT_KEY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);
const TIMER_ID: usize = 1;
const HOVER_TIMER_ID: usize = 2;
const TRAY_UID: u32   = 1;
const WM_TRAY: u32    = WM_USER + 1;
const WM_APPBAR: u32  = WM_USER + 2;

// Menu item IDs
const IDM_MODE_FLOAT:      u32 = 101;
const IDM_MODE_APPBAR:     u32 = 103;
const IDM_SHOW_TASKBAR:    u32 = 104;
const IDM_HOVER_AUTOHIDE:  u32 = 105;
const IDM_SHOW_CLAUDE:     u32 = 110;
const IDM_SHOW_CODEX:      u32 = 111;
const IDM_SHOW_CURSOR:     u32 = 112;
const IDM_CURSOR_COOKIE:   u32 = 113;
const IDM_CURSOR_CLR_COOKIE: u32 = 114;
const IDM_REFRESH_5S:      u32 = 201;
const IDM_REFRESH_1M:      u32 = 202;
const IDM_REFRESH_5M:      u32 = 203;
const IDM_MANUAL_REFRESH:  u32 = 150;
const IDM_AUTOSTART:       u32 = 301;
const IDM_EXIT:           u32 = 400;
const IDM_MONITOR_BASE:   u32 = 500; // +index per monitor

// ─── Logger ───────────────────────────────────────────────────────────────────
//
// Size-rotated file logger: usage-monitor.log is capped at LOG_MAX_BYTES; when it
// overflows it becomes usage-monitor.1.log (one generation kept). An oversized file
// found at startup (the pre-0.4 log grew to several GB) is discarded instead of kept.
// File level is Info; set USAGE_MONITOR_DEBUG=1 for Debug (per-request dumps).
const LOG_MAX_BYTES:     u64 = 5 * 1024 * 1024;
const LOG_DISCARD_BYTES: u64 = 100 * 1024 * 1024;

struct RotatingFile {
    path: PathBuf,
    file: Option<std::fs::File>,
    size: u64,
}

impl RotatingFile {
    fn open(path: PathBuf) -> Self {
        let mut rf = RotatingFile { path, file: None, size: 0 };
        rf.size = std::fs::metadata(&rf.path).map(|m| m.len()).unwrap_or(0);
        if rf.size > LOG_MAX_BYTES {
            rf.rotate();
        } else {
            rf.reopen();
        }
        rf
    }

    fn backup_path(&self) -> PathBuf {
        self.path.with_file_name("usage-monitor.1.log")
    }

    fn reopen(&mut self) {
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
    }

    fn rotate(&mut self) {
        self.file = None;
        let bak = self.backup_path();
        let _ = std::fs::remove_file(&bak);
        if self.size > LOG_DISCARD_BYTES {
            let _ = std::fs::remove_file(&self.path);
        } else {
            let _ = std::fs::rename(&self.path, &bak);
        }
        self.size = 0;
        self.reopen();
    }

    fn write_line(&mut self, line: &str) {
        if self.size > LOG_MAX_BYTES {
            self.rotate();
        }
        if let Some(f) = self.file.as_mut() {
            use std::io::Write;
            if f.write_all(line.as_bytes()).is_ok() {
                self.size += line.len() as u64;
            }
        }
    }
}

struct RotatingFileLogger {
    level: LevelFilter,
    inner: parking_lot::Mutex<RotatingFile>,
}

impl log::Log for RotatingFileLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.level
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) { return; }
        let line = if r.level() >= log::Level::Debug {
            format!("{} [{}] {}: {}\n", chrono::Local::now().format("%H:%M:%S"), r.level(), r.target(), r.args())
        } else {
            format!("{} [{}] {}\n", chrono::Local::now().format("%H:%M:%S"), r.level(), r.args())
        };
        self.inner.lock().write_line(&line);
    }
    fn flush(&self) {}
}

impl SharedLogger for RotatingFileLogger {
    fn level(&self) -> LevelFilter { self.level }
    fn config(&self) -> Option<&Config> { None }
    fn as_log(self: Box<Self>) -> Box<dyn log::Log> { self }
}

fn init_logger() {
    let log_base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let log_dir = log_base.join("UsageMonitor").join("_runtime_logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("usage-monitor.log");
    let file_level = if std::env::var("USAGE_MONITOR_DEBUG").map(|v| v == "1").unwrap_or(false) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    let mut loggers: Vec<Box<dyn SharedLogger>> = Vec::new();
    loggers.push(Box::new(RotatingFileLogger {
        level: file_level,
        inner: parking_lot::Mutex::new(RotatingFile::open(log_path.clone())),
    }));
    #[cfg(debug_assertions)]
    {
        loggers.push(TermLogger::new(
            LevelFilter::Debug,
            Config::default(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        ));
    }
    if !loggers.is_empty() {
        CombinedLogger::init(loggers).ok();
    }
    info!(
        "usage-monitor v{} starting — log: {}",
        env!("CARGO_PKG_VERSION"),
        log_path.display()
    );
}

// ─── Monitor enumeration ──────────────────────────────────────────────────────
extern "system" fn monitor_enum_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    unsafe {
        let list = &mut *(data.0 as *mut Vec<(HMONITOR, MONITORINFO)>);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(hmon, &mut mi);
        list.push((hmon, mi));
        BOOL(1)
    }
}

fn enum_monitors() -> Vec<MonitorInfo> {
    let mut raw: Vec<(HMONITOR, MONITORINFO)> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_enum_cb),
            LPARAM(&mut raw as *mut _ as isize),
        );
    }
    raw.into_iter()
        .map(|(_, mi)| MonitorInfo {
            rect: (mi.rcMonitor.left, mi.rcMonitor.top, mi.rcMonitor.right, mi.rcMonitor.bottom),
            work_rect: (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom),
        })
        .collect()
}

// ─── Dynamic tray icon ────────────────────────────────────────────────────────
fn update_tray_icon(hwnd: HWND, state: &SharedState) {
    let (c5h, x5h, c_err, x_err, cr_err, cr_pct) = {
        let s = state.lock();
        (
            s.claude.util("5h"),
            s.codex.utilization_5h,
            s.claude_error.clone(),
            s.codex_error.clone(),
            s.cursor_error.clone(),
            s.cursor.auto_usage_pct,
        )
    };

    unsafe {
        // Use system small-icon size for best fit
        let icon_size = GetSystemMetrics(SM_CXSMICON).max(16);
        let hicon = overlay::create_tray_icon_hicon(c5h, x5h, icon_size);
        if hicon.is_invalid() {
            return;
        }

        // Build tooltip: "Claude 82% · Codex 38% · Cursor 73%"
        let tip = {
            let c_str  = c5h.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "–".to_string());
            let x_str  = x5h.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "–".to_string());
            let cr_str = cr_pct.map(|v| format!(" · Cursor {:.0}%", v * 100.0)).unwrap_or_default();
            let err = if !c_err.is_empty() || !x_err.is_empty() || !cr_err.is_empty() {
                " ⚠".to_string()
            } else { String::new() };
            format!("Claude {} · Codex {}{}{}\0", c_str, x_str, cr_str, err)
        };

        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd:   hwnd,
            uID:    TRAY_UID,
            uFlags: NIF_ICON | NIF_TIP,
            hIcon:  hicon,
            ..Default::default()
        };
        let tip_w: Vec<u16> = tip.encode_utf16().collect();
        let n = tip_w.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip_w[..n]);
        Shell_NotifyIconW(NIM_MODIFY, &nid);
        DestroyIcon(hicon).ok();
    }
}

fn create_tray_icon(hwnd: HWND) {
    unsafe {
        // Initial icon: system default (will be replaced on first data update)
        let icon = LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd:   hwnd,
            uID:    TRAY_UID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon:  icon,
            ..Default::default()
        };
        let tip = "Usage Monitor\0";
        let tip_w: Vec<u16> = tip.encode_utf16().collect();
        let n = tip_w.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip_w[..n]);
        Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

fn remove_tray_icon(hwnd: HWND) {
    unsafe {
        let nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd:   hwnd,
            uID:    TRAY_UID,
            ..Default::default()
        };
        Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

// ─── Auto-start registry ──────────────────────────────────────────────────────
fn set_auto_start(enable: bool) {
    unsafe {
        let key_path = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let val_name = w!("UsageMonitor");
        let mut hkey = windows::Win32::System::Registry::HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_path,
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .is_ok()
        {
            if enable {
                if let Ok(exe) = std::env::current_exe() {
                    let s = format!("\"{}\"", exe.display());
                    let pw: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
                    let bytes = std::slice::from_raw_parts(
                        pw.as_ptr() as *const u8,
                        pw.len() * 2,
                    );
                    let _ = RegSetValueExW(hkey, val_name, 0, REG_SZ, Some(bytes));
                }
            } else {
                let _ = RegDeleteValueW(hkey, val_name);
            }
            RegCloseKey(hkey);
        }
    }
}

// ─── AppBar helpers ───────────────────────────────────────────────────────────

/// Register the window as a Shell AppBar and position it just left of the
/// system tray area. Returns the taskbar height actually used.
fn register_appbar(hwnd: HWND, appbar_w: i32) -> i32 {
    unsafe {
        // Embed into taskbar as child window (more reliable than SHAppBarMessage docking).
        let taskbar_hwnd = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null())
            .unwrap_or_default();

        let taskbar_h;
        let tray_left;
        let taskbar_bottom;
        let taskbar_top;
        let mut taskbar_rect = RECT::default();
        let mut has_taskbar = false;

        if taskbar_hwnd.0 != std::ptr::null_mut() {
            GetWindowRect(taskbar_hwnd, &mut taskbar_rect).ok();
            has_taskbar = true;
            taskbar_h      = taskbar_rect.bottom - taskbar_rect.top;
            taskbar_bottom = taskbar_rect.bottom;
            taskbar_top    = taskbar_rect.top;
            // Use TrayNotifyWnd (actual notification area) when available.
            let tray_hwnd = FindWindowExW(taskbar_hwnd, None, w!("TrayNotifyWnd"), PCWSTR::null())
                .unwrap_or_default();
            if tray_hwnd.0 != std::ptr::null_mut() {
                let mut tr = RECT::default();
                GetWindowRect(tray_hwnd, &mut tr).ok();
                tray_left = tr.left - appbar_w;
            } else {
                // Fallback to right edge of taskbar.
                tray_left = taskbar_rect.right - appbar_w;
            }

            // Convert popup to child and attach to taskbar when needed.
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let new_style = (style & !WS_POPUP.0) | WS_CHILD.0 | WS_CLIPSIBLINGS.0;
            if style != new_style {
                let _ = SetWindowLongW(hwnd, GWL_STYLE, new_style as i32);
            }
            if GetParent(hwnd).unwrap_or_default() != taskbar_hwnd {
                let _ = SetParent(hwnd, taskbar_hwnd);
            }
        } else {
            taskbar_h      = 40;
            taskbar_bottom = GetSystemMetrics(SM_CYSCREEN);
            taskbar_top    = taskbar_bottom - taskbar_h;
            tray_left      = GetSystemMetrics(SM_CXSCREEN) - appbar_w;
        }
        info!(
            "[appbar] pre-register taskbar_h={} top={} bottom={} tray_left={} appbar_w={}",
            taskbar_h, taskbar_top, taskbar_bottom, tray_left, appbar_w
        );

        // Position child window. Embedded coordinates are relative to taskbar origin.
        let h = taskbar_h.max(1);
        let mut x = tray_left;
        let mut y = taskbar_top;
        if has_taskbar {
            // Child-window coordinates must be relative to parent origin.
            // For bottom taskbar embedding, y should stay at 0 and height fills taskbar.
            x = tray_left - taskbar_rect.left;
            y = 0;
            info!(
                "[appbar-debug-v3] has_taskbar={} taskbar_rect=({},{})->({},{}) tray_left={} child_xy=({}, {}) h={}",
                has_taskbar,
                taskbar_rect.left,
                taskbar_rect.top,
                taskbar_rect.right,
                taskbar_rect.bottom,
                tray_left,
                x,
                y,
                h
            );
        }

        // Only move/resize when geometry truly changed to avoid resize repaint jitter.
        let mut wr = RECT::default();
        let _ = GetWindowRect(hwnd, &mut wr);
        let mut needs_move = true;
        if has_taskbar {
            let mut trc = RECT::default();
            let _ = GetWindowRect(taskbar_hwnd, &mut trc);
            let want_left = trc.left + x;
            let want_top = trc.top + y;
            let want_right = want_left + appbar_w;
            let want_bottom = want_top + h;
            needs_move = wr.left != want_left
                || wr.top != want_top
                || wr.right != want_right
                || wr.bottom != want_bottom;
        }
        if needs_move {
            let moved = MoveWindow(hwnd, x, y, appbar_w, h, true).is_ok();
            info!(
                "[appbar-debug-v3] MoveWindow applied={} child_xy=({}, {}) size=({},{})",
                moved, x, y, appbar_w, h
            );
            let _ = SetWindowPos(
                hwnd,
                HWND_TOP,
                x,
                y,
                appbar_w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            log_window_state("[appbar] after SetWindowPos", hwnd);
        }

        taskbar_h
    }
}

fn unregister_appbar(hwnd: HWND) {
    unsafe {
        let _ = SetParent(hwnd, None);
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let new_style = (style & !(WS_CHILD.0 | WS_CLIPSIBLINGS.0)) | WS_POPUP.0;
        let _ = SetWindowLongW(hwnd, GWL_STYLE, new_style as i32);
        let _ = SetWindowPos(
            hwnd,
            HWND_NOTOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        log_window_state("[appbar] detached to popup", hwnd);
    }
}

// ─── Window geometry ──────────────────────────────────────────────────────────
fn apply_window_layout(hwnd: HWND, state: &SharedState) {
    let s = state.lock();
    let monitors = s.monitors.clone();
    let settings = s.settings.clone();
    let layout = s.layout();
    drop(s);

    let mon_idx = settings.monitor_idx.min(monitors.len().saturating_sub(1));
    let mon = monitors.get(mon_idx).cloned().unwrap_or_default();
    let (ml, mt, mr, mb) = mon.work_rect;
    let mw = mr - ml;
    let mh = mb - mt;

    match settings.display_mode {
        DisplayMode::Floating => {
            let fw = settings.win_w.max(MIN_W);
            let fh = if settings.win_h < MIN_H { calc_window_height(&settings, layout) } else { settings.win_h };
            let fx = if settings.win_x < 0 { ml + mw - fw - 16 } else { settings.win_x };
            let fy = if settings.win_y < 0 { mt + mh - fh - 48 } else { settings.win_y };
            unsafe {
                // Restore TOPMOST + semi-transparent when leaving AppBar
                let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                let new_ex = if settings.show_in_taskbar {
                    (ex & !WS_EX_TOOLWINDOW.0) | WS_EX_TOPMOST.0 | WS_EX_APPWINDOW.0 | WS_EX_LAYERED.0
                } else {
                    (ex & !WS_EX_APPWINDOW.0) | WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0
                };
                SetWindowLongW(hwnd, GWL_EXSTYLE, new_ex as i32);
                SetLayeredWindowAttributes(hwnd, COLORREF(0), 220, LWA_ALPHA).ok();
                SetWindowPos(hwnd, HWND_TOPMOST, fx, fy, fw, fh,
                    SWP_NOACTIVATE | SWP_FRAMECHANGED).ok();
                overlay::apply_rounded_region(hwnd, fw, fh);
                InvalidateRect(hwnd, None, false).ok();
                log_window_state("[layout] floating applied", hwnd);
            }
        }
        DisplayMode::CompactBar => {
            // CompactBar mode removed: treat as Floating for backward-compatible settings.
            let fw = settings.win_w.max(MIN_W);
            let fh = if settings.win_h < MIN_H { calc_window_height(&settings, layout) } else { settings.win_h };
            let fx = if settings.win_x < 0 { ml + mw - fw - 16 } else { settings.win_x };
            let fy = if settings.win_y < 0 { mt + mh - fh - 48 } else { settings.win_y };
            unsafe {
                let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                let new_ex = if settings.show_in_taskbar {
                    (ex & !WS_EX_TOOLWINDOW.0) | WS_EX_TOPMOST.0 | WS_EX_APPWINDOW.0 | WS_EX_LAYERED.0
                } else {
                    (ex & !WS_EX_APPWINDOW.0) | WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0
                };
                SetWindowLongW(hwnd, GWL_EXSTYLE, new_ex as i32);
                SetLayeredWindowAttributes(hwnd, COLORREF(0), 220, LWA_ALPHA).ok();
                SetWindowPos(hwnd, HWND_TOPMOST, fx, fy, fw, fh,
                    SWP_NOACTIVATE | SWP_FRAMECHANGED).ok();
                overlay::apply_rounded_region(hwnd, fw, fh);
                InvalidateRect(hwnd, None, false).ok();
                log_window_state("[layout] compact->floating applied", hwnd);
            }
        }
        DisplayMode::AppBar => {
            unsafe {
                // AppBar must NOT be TOPMOST/TOOLWINDOW/LAYERED.
                // LAYERED + AppBar can end up reserving space but not rendering reliably.
                let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                let new_ex = ex & !(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0 | WS_EX_APPWINDOW.0);
                SetWindowLongW(hwnd, GWL_EXSTYLE, new_ex as i32);
                // Remove rounded region so AppBar fills the full taskbar band
                SetWindowRgn(hwnd, None, true);
                info!("[layout] switch->appbar exstyle={:#x} -> {:#x}", ex, new_ex);
            }
            let aw = appbar_width(&settings);
            register_appbar(hwnd, aw);
            unsafe {
                InvalidateRect(hwnd, None, false).ok();
                log_window_state("[layout] appbar applied", hwnd);
            }
        }
    }
}

fn log_window_state(tag: &str, hwnd: HWND) {
    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let mut wr = RECT::default();
        let mut cr = RECT::default();
        let _ = GetWindowRect(hwnd, &mut wr);
        let _ = GetClientRect(hwnd, &mut cr);
        info!(
            "{} hwnd={:?} visible={} style={:#x} exstyle={:#x} window=({},{})->({},{}) client={}x{}",
            tag,
            hwnd,
            IsWindowVisible(hwnd).as_bool(),
            style,
            ex,
            wr.left,
            wr.top,
            wr.right,
            wr.bottom,
            cr.right - cr.left,
            cr.bottom - cr.top
        );
    }
}

fn apply_ex_style(hwnd: HWND, show_in_taskbar: bool) {
    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let new_ex = if show_in_taskbar {
            (ex & !WS_EX_TOOLWINDOW.0) | WS_EX_APPWINDOW.0
        } else {
            (ex & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0
        };
        SetWindowLongW(hwnd, GWL_EXSTYLE, new_ex as i32);
    }
}

// ─── Context menu ─────────────────────────────────────────────────────────────
fn show_context_menu(hwnd: HWND, state: &SharedState) {
    let (settings, monitors) = {
        let s = state.lock();
        (s.settings.clone(), s.monitors.clone())
    };

    unsafe {
        let hmenu = CreatePopupMenu().unwrap_or_default();

        // ── Show Claude / Codex / Cursor toggles ──────────────────────────
        let fc = if settings.show_claude { MF_CHECKED } else { MF_UNCHECKED };
        let fx = if settings.show_codex  { MF_CHECKED } else { MF_UNCHECKED };
        let fcu = if settings.show_cursor { MF_CHECKED } else { MF_UNCHECKED };
        AppendMenuW(hmenu, MF_STRING | fc,  IDM_SHOW_CLAUDE  as usize, w!("显示 Claude")).ok();
        AppendMenuW(hmenu, MF_STRING | fx,  IDM_SHOW_CODEX   as usize, w!("显示 Codex")).ok();
        AppendMenuW(hmenu, MF_STRING | fcu, IDM_SHOW_CURSOR  as usize, w!("显示 Cursor")).ok();

        // Cursor submenu: Cookie management
        let h_cursor = CreatePopupMenu().unwrap_or_default();
        AppendMenuW(h_cursor, MF_STRING, IDM_CURSOR_COOKIE     as usize, w!("粘贴 Cookie…")).ok();
        AppendMenuW(h_cursor, MF_STRING, IDM_CURSOR_CLR_COOKIE as usize, w!("清除 Cookie")).ok();
        AppendMenuW(hmenu, MF_POPUP, h_cursor.0 as usize, w!("Cursor 认证")).ok();

        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).ok();

        // ── Display mode (radio-style) ─────────────────────────────────────
        let mf = |m: DisplayMode| {
            if settings.display_mode == m { MF_CHECKED } else { MF_UNCHECKED }
        };
        AppendMenuW(hmenu, MF_STRING | mf(DisplayMode::Floating),
            IDM_MODE_FLOAT  as usize, w!("浮动悬窗")).ok();
        AppendMenuW(hmenu, MF_STRING | mf(DisplayMode::AppBar),
            IDM_MODE_APPBAR as usize, w!("嵌入任务栏 (AppBar)")).ok();

        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).ok();

        // ── Manual refresh ─────────────────────────────────────────────────
        AppendMenuW(hmenu, MF_STRING, IDM_MANUAL_REFRESH as usize, w!("立即刷新")).ok();

        // ── Auto-start ─────────────────────────────────────────────────────
        let fas = if settings.auto_start { MF_CHECKED } else { MF_UNCHECKED };
        AppendMenuW(hmenu, MF_STRING | fas, IDM_AUTOSTART as usize, w!("开机自启")).ok();

        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).ok();

        // ── Advanced submenu ───────────────────────────────────────────────
        let h_adv = CreatePopupMenu().unwrap_or_default();

        // Show in taskbar toggle
        let ftb = if settings.show_in_taskbar { MF_CHECKED } else { MF_UNCHECKED };
        AppendMenuW(h_adv, MF_STRING | ftb,
            IDM_SHOW_TASKBAR as usize, w!("在任务栏显示按钮")).ok();

        let fhover = if settings.hover_auto_hide { MF_CHECKED } else { MF_UNCHECKED };
        AppendMenuW(h_adv, MF_STRING | fhover,
            IDM_HOVER_AUTOHIDE as usize, w!("Hover Auto Hide")).ok();

        // Monitor selection
        if monitors.len() > 1 {
            AppendMenuW(h_adv, MF_SEPARATOR, 0, PCWSTR::null()).ok();
            for (i, _) in monitors.iter().enumerate() {
                let lbl: Vec<u16> = format!("显示器 {}\0", i + 1).encode_utf16().collect();
                let fm = if i == settings.monitor_idx { MF_CHECKED } else { MF_UNCHECKED };
                AppendMenuW(h_adv, MF_STRING | fm,
                    (IDM_MONITOR_BASE + i as u32) as usize,
                    PCWSTR(lbl.as_ptr())).ok();
            }
        }

        // Refresh rate
        AppendMenuW(h_adv, MF_SEPARATOR, 0, PCWSTR::null()).ok();
        let h_ref = CreatePopupMenu().unwrap_or_default();
        let rf = |r: RefreshRate| {
            if settings.refresh_rate == r { MF_CHECKED } else { MF_UNCHECKED }
        };
        AppendMenuW(h_ref, MF_STRING | rf(RefreshRate::Secs5),
            IDM_REFRESH_5S as usize, w!("5 秒")).ok();
        AppendMenuW(h_ref, MF_STRING | rf(RefreshRate::Min1),
            IDM_REFRESH_1M as usize, w!("1 分钟")).ok();
        AppendMenuW(h_ref, MF_STRING | rf(RefreshRate::Min5),
            IDM_REFRESH_5M as usize, w!("5 分钟")).ok();
        AppendMenuW(h_adv, MF_POPUP, h_ref.0 as usize, w!("刷新频率")).ok();

        AppendMenuW(hmenu, MF_POPUP, h_adv.0 as usize, w!("高级设置")).ok();

        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).ok();
        AppendMenuW(hmenu, MF_STRING, IDM_EXIT as usize, w!("退出")).ok();

        let mut pt = POINT::default();
        GetCursorPos(&mut pt).ok();
        SetForegroundWindow(hwnd).ok();
        let cmd = TrackPopupMenu(
            hmenu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x, pt.y, 0, hwnd, None,
        );
        DestroyMenu(hmenu).ok();

        handle_menu(hwnd, cmd.0 as u32, state, &monitors);
    }
}

fn handle_menu(hwnd: HWND, cmd: u32, state: &SharedState, monitors: &[MonitorInfo]) {
    unsafe {
        info!("[menu] command id={}", cmd);
        match cmd {
            IDM_SHOW_CLAUDE => {
                let v = {
                    let mut s = state.lock();
                    s.settings.show_claude = !s.settings.show_claude;
                    let v = s.settings.show_claude;
                    save_settings(&s.settings);
                    v
                };
                info!("show_claude → {}", v);
                resize_to_content(hwnd, state);
                InvalidateRect(hwnd, None, false).ok();
            }

            IDM_SHOW_CODEX => {
                let v = {
                    let mut s = state.lock();
                    s.settings.show_codex = !s.settings.show_codex;
                    let v = s.settings.show_codex;
                    save_settings(&s.settings);
                    v
                };
                info!("show_codex → {}", v);
                resize_to_content(hwnd, state);
                InvalidateRect(hwnd, None, false).ok();
            }

            IDM_SHOW_CURSOR => {
                let v = {
                    let mut s = state.lock();
                    s.settings.show_cursor = !s.settings.show_cursor;
                    let v = s.settings.show_cursor;
                    save_settings(&s.settings);
                    v
                };
                info!("show_cursor → {}", v);
                // Immediately trigger a Cursor poll when enabling
                if v { monitor::wake_pollers(); }
                resize_to_content(hwnd, state);
                InvalidateRect(hwnd, None, false).ok();
            }

            IDM_CURSOR_COOKIE => {
                // Show an InputBox-style dialog via a simple Win32 message box prompt.
                // We use a custom dialog defined below.
                if let Some(cookie) = show_cookie_input_dialog(hwnd) {
                    match cursor::save_cookie(&cookie) {
                        Ok(()) => {
                            info!("[menu] Cursor cookie saved ({} chars)", cookie.len());
                            monitor::wake_pollers();
                        }
                        Err(e) => {
                            error_msgbox(hwnd, &format!("保存 Cookie 失败:\n{}", e));
                        }
                    }
                }
            }

            IDM_CURSOR_CLR_COOKIE => {
                cursor::clear_cookie();
                info!("[menu] Cursor cookie cleared");
                {
                    let mut s = state.lock();
                    s.cursor = crate::state::CursorUsage::default();
                    s.cursor_error = "Cookie 已清除".to_string();
                }
                InvalidateRect(hwnd, None, false).ok();
            }

            IDM_MODE_FLOAT => {
                let prev_mode = state.lock().settings.display_mode;
                if prev_mode == DisplayMode::AppBar {
                    unregister_appbar(hwnd);
                }
                state.lock().settings.display_mode = DisplayMode::Floating;
                save_settings(&state.lock().settings);
                apply_window_layout(hwnd, state);
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }

            IDM_MODE_APPBAR => {
                state.lock().settings.display_mode = DisplayMode::AppBar;
                save_settings(&state.lock().settings);
                apply_window_layout(hwnd, state);
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }

            IDM_SHOW_TASKBAR => {
                let v = {
                    let mut s = state.lock();
                    s.settings.show_in_taskbar = !s.settings.show_in_taskbar;
                    let v = s.settings.show_in_taskbar;
                    save_settings(&s.settings);
                    v
                };
                apply_ex_style(hwnd, v);
            }

            IDM_HOVER_AUTOHIDE => {
                let enable = {
                    let mut s = state.lock();
                    s.settings.hover_auto_hide = !s.settings.hover_auto_hide;
                    let v = s.settings.hover_auto_hide;
                    if !v && s.floating_hover_hidden {
                        s.floating_hover_hidden = false;
                    }
                    save_settings(&s.settings);
                    v
                };
                if !enable {
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }

            IDM_MANUAL_REFRESH => {
                monitor::wake_pollers();
                InvalidateRect(hwnd, None, false).ok();
            }

            IDM_REFRESH_5S => do_refresh(hwnd, state, RefreshRate::Secs5),
            IDM_REFRESH_1M => do_refresh(hwnd, state, RefreshRate::Min1),
            IDM_REFRESH_5M => do_refresh(hwnd, state, RefreshRate::Min5),

            IDM_AUTOSTART => {
                let v = {
                    let mut s = state.lock();
                    s.settings.auto_start = !s.settings.auto_start;
                    let v = s.settings.auto_start;
                    save_settings(&s.settings);
                    v
                };
                set_auto_start(v);
            }

            IDM_EXIT => {
                // Keep exit path minimal and non-blocking to avoid appbar teardown deadlocks.
                info!("[exit] fast-exit requested; deleting tray icon then TerminateProcess");
                remove_tray_icon(hwnd);
                let _ = TerminateProcess(GetCurrentProcess(), 0);
            }

            id if (IDM_MONITOR_BASE..IDM_MONITOR_BASE + 8).contains(&id) => {
                let idx = (id - IDM_MONITOR_BASE) as usize;
                if idx < monitors.len() {
                    state.lock().settings.monitor_idx = idx;
                    save_settings(&state.lock().settings);
                    apply_window_layout(hwnd, state);
                }
            }

            _ => {}
        }
    }
}

// ─── Floating window auto-resize ──────────────────────────────────────────────
/// Resize the floating window to the computed height for the currently-enabled
/// sections. Does nothing in AppBar mode (size is fixed by the taskbar band).
fn resize_to_content(hwnd: HWND, state: &SharedState) {
    let (mode, settings, layout) = {
        let s = state.lock();
        (s.settings.display_mode, s.settings.clone(), s.layout())
    };
    // AppBar: re-register with the new (possibly narrower/wider) width.
    if mode == DisplayMode::AppBar {
        register_appbar(hwnd, appbar_width(&settings));
        return;
    }
    if mode != DisplayMode::Floating { return; }

    let new_h = calc_window_height(&settings, layout);
    unsafe {
        let mut wr = RECT::default();
        GetWindowRect(hwnd, &mut wr).ok();
        let cur_h = wr.bottom - wr.top;
        let cur_w = wr.right - wr.left;
        if cur_h == new_h { return; }

        // Anchor to bottom-right: keep bottom + right edge fixed, grow/shrink upward
        let new_top = wr.bottom - new_h;
        SetWindowPos(hwnd, HWND_TOPMOST,
            wr.left, new_top, cur_w, new_h,
            SWP_NOACTIVATE).ok();
        overlay::apply_rounded_region(hwnd, cur_w, new_h);

        // Persist size
        let mut s = state.lock();
        s.settings.win_h = new_h;
        s.settings.win_y = new_top;
        save_settings(&s.settings);

        info!("[resize] floating h {} → {}", cur_h, new_h);
    }
}

// ─── Cursor Cookie input dialog ───────────────────────────────────────────────
/// Show a simple Win32 InputBox-style dialog to collect the Cursor cookie string.
/// Returns Some(cookie) if the user clicked OK with non-empty input, None otherwise.
///
/// Implementation: uses a small custom dialog window with an Edit control.
fn show_cookie_input_dialog(parent: HWND) -> Option<String> {
    // Build a simple dialog using TaskDialog-style MessageBox as a fallback.
    // For a real InputBox we'd use a custom WNDPROC; here we use the clipboard
    // approach: instruct the user to copy the cookie, then read it.
    //
    // We use a two-step approach:
    //  1. Show instructions message box.
    //  2. Read clipboard.
    unsafe {
        let msg = "请在浏览器中打开 cursor.com，\
            按 F12 → Network → 任意请求 → Headers → Cookie，\
            复制完整的 Cookie 字符串后，\
            \n\n点击「确定」从剪贴板粘贴。\
            \n（Cookie 仅保存在本机，不会上传）";
        let msg_w: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
        let title_w: Vec<u16> = "Cursor Cookie\0".encode_utf16().collect();

        let result = MessageBoxW(
            parent,
            PCWSTR(msg_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OKCANCEL | MB_ICONINFORMATION,
        );
        if result == IDCANCEL { return None; }

        // CF_UNICODETEXT = 13 (standard Windows clipboard format constant).
        // GetClipboardData returns HANDLE; GlobalLock/Unlock require HGLOBAL —
        // both are opaque isize wrappers, so we reinterpret via std::mem::transmute.
        const CF_UNICODE: u32 = 13;
        if OpenClipboard(parent).is_ok() {
            let h = GetClipboardData(CF_UNICODE);
            let cookie = if let Ok(handle) = h {
                // Reinterpret HANDLE as HGLOBAL (same underlying type: isize).
                let hglobal: HGLOBAL = std::mem::transmute(handle);
                let ptr = GlobalLock(hglobal) as *const u16;
                let cookie = if !ptr.is_null() {
                    let mut len = 0usize;
                    while *ptr.add(len) != 0 { len += 1; }
                    let slice = std::slice::from_raw_parts(ptr, len);
                    String::from_utf16_lossy(slice).trim().to_string()
                } else { String::new() };
                let _ = GlobalUnlock(hglobal);
                cookie
            } else { String::new() };
            CloseClipboard().ok();

            if !cookie.is_empty() && cookie.contains('=') {
                return Some(cookie);
            }
            // Cookie looks invalid or clipboard was empty
            let err_msg: Vec<u16> = "剪贴板内容为空或不像 Cookie 字符串（应包含 = 号）。\n请确认已复制正确内容后重试。\0"
                .encode_utf16().collect();
            MessageBoxW(parent, PCWSTR(err_msg.as_ptr()), PCWSTR(title_w.as_ptr()), MB_OK | MB_ICONWARNING);
        }
        None
    }
}

fn error_msgbox(parent: HWND, msg: &str) {
    unsafe {
        let msg_w: Vec<u16> = format!("{}\0", msg).encode_utf16().collect();
        let title_w: Vec<u16> = "Usage Monitor 错误\0".encode_utf16().collect();
        MessageBoxW(parent, PCWSTR(msg_w.as_ptr()), PCWSTR(title_w.as_ptr()), MB_OK | MB_ICONERROR);
    }
}

fn do_refresh(hwnd: HWND, state: &SharedState, rate: RefreshRate) {
    {
        let mut s = state.lock();
        s.settings.refresh_rate = rate;
        save_settings(&s.settings);
    }
    set_refresh_secs(rate.secs());
    unsafe { InvalidateRect(hwnd, None, false).ok(); }
}

// ─── Entry point ──────────────────────────────────────────────────────────────
fn main() -> Result<()> {
    init_logger();

    // Diagnostic mode: `usage-monitor --probe [--cli]` runs one Claude fetch, prints the
    // outcome to stdout and the log, and exits without creating any window.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--probe") {
        let force_cli = args.iter().any(|a| a == "--cli");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()
            .expect("tokio runtime");
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()
            .expect("HTTP client");
        let out = rt.block_on(claude::probe(&client, force_cli));
        info!("[probe] {}", out);
        println!("{}", out);
        return Ok(());
    }

    unsafe {
        let hinstance: HINSTANCE = std::mem::transmute(GetModuleHandleW(None)?);
        let class_name = w!("UsageMonitorOverlay");

        let wc = WNDCLASSEXW {
            cbSize:        std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc:   Some(wnd_proc),
            hInstance:     hinstance,
            hCursor:       LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: CreateSolidBrush(COLORREF(0x00171711)),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        // Load settings & enumerate monitors
        let mut settings = load_settings();
        if settings.display_mode == DisplayMode::CompactBar {
            settings.display_mode = DisplayMode::Floating;
            save_settings(&settings);
        }
        let monitors  = enum_monitors();
        info!("Found {} monitors", monitors.len());

        let shared = new_shared();
        {
            let mut s = shared.lock();
            s.settings = settings.clone();
            s.monitors  = monitors.clone();
        }

        // Initial window geometry
        let mon_idx = settings.monitor_idx.min(monitors.len().saturating_sub(1));
        let mon = monitors.get(mon_idx).cloned().unwrap_or_default();
        let (ml, mt, mr, mb) = mon.work_rect;
        let mw = mr - ml;
        let mh = mb - mt;

        let (wx, wy, ww, wh) = match settings.display_mode {
            DisplayMode::Floating => {
                let fw = settings.win_w.max(MIN_W);
                // Use calc_window_height when win_h is -1 (auto) or from settings.
                // No data yet at startup → 2 Claude rows; WM_APP resizes once rows arrive.
                let fh = if settings.win_h < MIN_H {
                    calc_window_height(&settings, LayoutInfo { claude_rows: 2, banner: false })
                } else {
                    settings.win_h
                };
                let fx = if settings.win_x < 0 { ml + mw - fw - 16 } else { settings.win_x };
                let fy = if settings.win_y < 0 { mt + mh - fh - 48 } else { settings.win_y };
                (fx, fy, fw, fh)
            }
            DisplayMode::CompactBar => {
                let fw = settings.win_w.max(MIN_W);
                let fh = if settings.win_h < MIN_H {
                    calc_window_height(&settings, LayoutInfo { claude_rows: 2, banner: false })
                } else {
                    settings.win_h
                };
                let fx = if settings.win_x < 0 { ml + mw - fw - 16 } else { settings.win_x };
                let fy = if settings.win_y < 0 { mt + mh - fh - 48 } else { settings.win_y };
                (fx, fy, fw, fh)
            }
            // AppBar: start with placeholder; register_appbar will reposition
            DisplayMode::AppBar => {
                let screen_w = GetSystemMetrics(SM_CXSCREEN);
                let screen_h = GetSystemMetrics(SM_CYSCREEN);
                let aw = appbar_width(&settings);
                (screen_w - aw, screen_h - 40, aw, 40)
            }
        };

        // AppBar windows must NOT carry WS_EX_TOPMOST or WS_EX_TOOLWINDOW —
        // the shell positions them inside the taskbar band; TOPMOST fights that
        // placement and TOOLWINDOW hides them from the shell's z-order management.
        let ex_style = match settings.display_mode {
            DisplayMode::AppBar => WINDOW_EX_STYLE(0),
            DisplayMode::Floating if !settings.show_in_taskbar =>
                WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            _ => WS_EX_TOPMOST | WS_EX_LAYERED,
        };

        let hwnd = CreateWindowExW(
            ex_style,
            class_name,
            w!("Usage Monitor"),
            WS_POPUP,
            wx, wy, ww, wh,
            None, None, hinstance, None,
        )?;

        // Floating uses alpha=220 via layered window.
        // AppBar is created as a normal opaque window (non-layered).
        if settings.display_mode != DisplayMode::AppBar {
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 220, LWA_ALPHA)?;
        }

        // Store SharedState in window USERDATA
        let ptr = Box::into_raw(Box::new(shared.clone()));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);

        // AppBar must have no rounded clip region — it fills the full taskbar band.
        // Other modes get the 14px rounded corners from the design spec.
        if settings.display_mode != DisplayMode::AppBar {
            overlay::apply_rounded_region(hwnd, ww, wh);
        }
        info!("Window: {:?}  pos=({},{})  size=({}×{})", hwnd, wx, wy, ww, wh);
        log_window_state("[startup] initial window", hwnd);

        // Tray icon
        create_tray_icon(hwnd);
        info!("Tray icon added");

        // AppBar registration (if starting in that mode)
        if settings.display_mode == DisplayMode::AppBar {
            register_appbar(hwnd, appbar_width(&settings));
        }

        // Background polling
        monitor::spawn(shared.clone(), hwnd);
        set_refresh_secs(settings.refresh_rate.secs());

        // Timers: 1s appbar relayout + 100ms floating hover auto-hide.
        SetTimer(hwnd, TIMER_ID, 1000, None);
        SetTimer(hwnd, HOVER_TIMER_ID, 100, None);

        // Show overlay on startup
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

// ─── Window procedure ─────────────────────────────────────────────────────────
extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let get_state = || -> Option<&SharedState> {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const SharedState;
            if ptr.is_null() { None } else { Some(&*ptr) }
        };

        match msg {
            // Avoid background erase flicker; we fully paint in WM_PAINT.
            WM_ERASEBKGND => LRESULT(1),

            // ── Paint ──────────────────────────────────────────────────────
            WM_PAINT => {
                if let Some(shared) = get_state() {
                    let s = shared.lock();
                    overlay::paint(hwnd, &s);
                }
                LRESULT(0)
            }

            // ── 1-second timer → countdown repaint ────────────────────────
            WM_TIMER if wparam.0 == TIMER_ID => {
                if let Some(sh) = get_state() {
                    let s = sh.lock();
                    if s.settings.display_mode == DisplayMode::AppBar {
                        // Keep hugging tray left edge when tray icon count/width changes.
                        let aw = appbar_width(&s.settings);
                        drop(s);
                        register_appbar(hwnd, aw);
                    }
                }
                LRESULT(0)
            }

            WM_TIMER if wparam.0 == HOVER_TIMER_ID => {
                if let Some(sh) = get_state() {
                    let mut s = sh.lock();
                    if s.settings.display_mode != DisplayMode::Floating || !s.settings.hover_auto_hide {
                        if s.floating_hover_hidden {
                            s.floating_hover_hidden = false;
                            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        }
                        return LRESULT(0);
                    }

                    let mut wr = RECT::default();
                    let mut pt = POINT::default();
                    let _ = GetWindowRect(hwnd, &mut wr);
                    let _ = GetCursorPos(&mut pt);
                    let inside = pt.x >= wr.left
                        && pt.x < wr.right
                        && pt.y >= wr.top
                        && pt.y < wr.bottom;

                    if inside && !s.floating_hover_hidden && IsWindowVisible(hwnd).as_bool() {
                        s.floating_hover_hidden = true;
                        ShowWindow(hwnd, SW_HIDE);
                    } else if !inside && s.floating_hover_hidden {
                        s.floating_hover_hidden = false;
                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                }
                LRESULT(0)
            }

            // ── Monitor posted WM_APP → data ready + refresh tray icon ─────
            WM_APP => {
                if let Some(sh) = get_state() {
                    update_tray_icon(hwnd, sh);
                    // Grow/shrink the floating window when the Claude row count or the
                    // banner line changes (e.g. the Fable row arriving after first fetch).
                    let key = {
                        let s = sh.lock();
                        let l = s.layout();
                        ((l.claude_rows as u32) << 1) | (l.banner as u32)
                    };
                    if LAST_LAYOUT_KEY.swap(key, std::sync::atomic::Ordering::Relaxed) != key {
                        resize_to_content(hwnd, sh);
                    }
                }
                InvalidateRect(hwnd, None, false).ok();
                LRESULT(0)
            }

            // ── AppBar shell notification ──────────────────────────────────
            WM_APPBAR => {
                // Re-register position if taskbar changes (e.g. auto-hide toggle)
                if let Some(sh) = get_state() {
                    let s = sh.lock();
                    if s.settings.display_mode == DisplayMode::AppBar {
                        info!("[appbar] WM_APPBAR received wparam={} lparam={}", wparam.0, lparam.0);
                        let aw = appbar_width(&s.settings);
                        drop(s);
                        register_appbar(hwnd, aw);
                        InvalidateRect(hwnd, None, false).ok();
                        log_window_state("[appbar] after WM_APPBAR relayout", hwnd);
                    }
                }
                LRESULT(0)
            }

            // ── System tray callback ───────────────────────────────────────
            WM_TRAY => {
                let event = (lparam.0 & 0xFFFF) as u32;
                if event == WM_LBUTTONUP {
                    if let Some(sh) = get_state() {
                        let mode = sh.lock().settings.display_mode;
                        if mode == DisplayMode::AppBar {
                            // AppBar stays pinned; left-click does nothing
                        } else if IsWindowVisible(hwnd).as_bool() {
                            ShowWindow(hwnd, SW_HIDE);
                        } else {
                            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            apply_window_layout(hwnd, sh);
                        }
                    }
                } else if event == WM_RBUTTONUP {
                    if let Some(sh) = get_state() {
                        show_context_menu(hwnd, sh);
                    }
                }
                LRESULT(0)
            }

            // ── Drag anywhere / resize edges (floating only) ───────────────
            WM_NCHITTEST => {
                let floating = get_state()
                    .map(|s| s.lock().settings.display_mode == DisplayMode::Floating)
                    .unwrap_or(true);

                if !floating {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }

                let sx = (lparam.0 & 0xFFFF) as i16 as i32;
                let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let mut wr = RECT::default();
                GetWindowRect(hwnd, &mut wr).ok();

                let edge = 7i32;
                let nl = sx - wr.left  < edge;
                let nr = wr.right  - sx < edge;
                let nt = sy - wr.top   < edge;
                let nb = wr.bottom - sy < edge;

                LRESULT(match (nl, nr, nt, nb) {
                    (true,  false, true,  false) => HTTOPLEFT     as isize,
                    (false, true,  true,  false) => HTTOPRIGHT    as isize,
                    (true,  false, false, true)  => HTBOTTOMLEFT  as isize,
                    (false, true,  false, true)  => HTBOTTOMRIGHT as isize,
                    (true,  false, _,     _)     => HTLEFT        as isize,
                    (false, true,  _,     _)     => HTRIGHT       as isize,
                    (_,     _,     true,  false) => HTTOP         as isize,
                    (_,     _,     false, true)  => HTBOTTOM      as isize,
                    _                            => HTCAPTION     as isize,
                })
            }

            // ── Minimum size ───────────────────────────────────────────────
            WM_GETMINMAXINFO => {
                let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
                mmi.ptMinTrackSize.x = MIN_W;
                mmi.ptMinTrackSize.y = MIN_H;
                LRESULT(0)
            }

            // ── Track resize / move → update region & save pos ─────────────
            WM_SIZE | WM_MOVE => {
                if let Some(sh) = get_state() {
                    let mode = sh.lock().settings.display_mode;
                    if mode == DisplayMode::Floating {
                        let mut wr = RECT::default();
                        GetWindowRect(hwnd, &mut wr).ok();
                        let nw = wr.right - wr.left;
                        let nh = wr.bottom - wr.top;
                        {
                            let mut s = sh.lock();
                            s.settings.win_x = wr.left;
                            s.settings.win_y = wr.top;
                            s.settings.win_w = nw;
                            s.settings.win_h = nh;
                        }
                        overlay::apply_rounded_region(hwnd, nw, nh);
                    }
                }
                InvalidateRect(hwnd, None, false).ok();
                LRESULT(0)
            }

            // ── Cleanup ───────────────────────────────────────────────────
            WM_DESTROY => {
                KillTimer(hwnd, TIMER_ID).ok();
                KillTimer(hwnd, HOVER_TIMER_ID).ok();
                if let Some(sh) = get_state() {
                    if sh.lock().settings.display_mode == DisplayMode::AppBar {
                        unregister_appbar(hwnd);
                    }
                }
                remove_tray_icon(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

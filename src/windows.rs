// usage-monitor v2 — Windows system-tray + floating overlay for Claude + Codex usage
//
// Architecture:
//   • System tray icon (Shell_NotifyIconW) — dynamic dual progress-bar icon
//   • Left-click tray  → show / hide overlay
//   • Right-click tray → context menu with show/hide per service + mode select
//   • Floating mode:    draggable (HTCAPTION anywhere), resizable (edge NCHITTEST)
//   • AppBar mode:      Win10 child / Win11 independent surface within the taskbar
//   • WM_TIMER (1 s)  → countdown repaint
//   • WM_APP          → monitor thread data-ready repaint + tray icon refresh


use usage_monitor::{claude, codex, cursor, monitor, state, platform};
use crate::{overlay, windows_compat};

use log::{info, LevelFilter};
use monitor::set_refresh_secs;
use simplelog::{CombinedLogger, Config, SharedLogger};
#[cfg(debug_assertions)]
use simplelog::{ColorChoice, TermLogger, TerminalMode};
use state::{
    appbar_col_widths, calc_window_height, effective_win_w, load_settings, new_shared, save_settings,
    DisplayMode, LayoutInfo, MonitorInfo, RefreshRate, Settings, SharedState, Theme, APPBAR_COL_W,
};
use std::fs::OpenOptions;
use std::path::PathBuf;
use windows::{
    core::{w, Result, PCWSTR},
    Win32::{
        Foundation::{BOOL, COLORREF, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            CreateSolidBrush, EnumDisplayMonitors, GetMonitorInfoW, HDC, InvalidateRect,
            SetWindowRgn, MonitorFromWindow, MONITOR_DEFAULTTONEAREST, HMONITOR, MONITORINFO,
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
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIF_INFO, NIIF_INFO, NIM_ADD, NIM_DELETE,
                NIM_MODIFY, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::*,
        },
    },
};

include!("windows_choseway.rs");

// ─── Constants ────────────────────────────────────────────────────────────────
const DEF_W: i32      = 320;
const MIN_W: i32      = 240;
const MIN_H: i32      = 96;    // single-section minimum (Neon: 24 + 80 + 8 = 112; Classic: 120)
/// System DPI scale (1.0 at 96 dpi). The taskbar band height and the dock fonts scale
/// with DPI, so the column widths must too or the labels get clipped.
static WINDOW_DPI: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(96);
static SUMMARY_FULLSCREEN_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static APPBAR_STATUS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// Space kept free for the clock on a secondary taskbar when no clock window is found.
const SECONDARY_CLOCK_W: i32 = 112;
/// WM_MOUSELEAVE (winuser.h); requested through TrackMouseEvent.
const WM_MOUSE_LEAVE: u32 = 0x02A3;
fn system_scale() -> f32 {
    WINDOW_DPI.load(std::sync::atomic::Ordering::Relaxed).max(96) as f32 / 96.0
}

/// Compute the AppBar width from current settings (sum of visible column widths at 96 dpi:
/// Claude 200px to fit its three rows, others 180px), scaled by the system DPI.
fn appbar_width(settings: &Settings) -> i32 {
    let base = appbar_col_widths(settings).iter().sum::<i32>().max(APPBAR_COL_W);
    (base as f32 * system_scale()).round() as i32
}

/// Layout key of the last WM_APP repaint (Claude/Codex row counts and banner) — the floating
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
const IDM_CODEX_DETAILS:    u32 = 115;
const IDM_CURSOR_COOKIE:   u32 = 113;
const IDM_CURSOR_CLR_COOKIE: u32 = 114;
const IDM_REFRESH_5S:      u32 = 201;
const IDM_REFRESH_1M:      u32 = 202;
const IDM_REFRESH_5M:      u32 = 203;
const IDM_MANUAL_REFRESH:  u32 = 150;
const IDM_AUTOSTART:       u32 = 301;
const IDM_UPDATE_CHECK: u32 = 320;
const IDM_UPDATE_AUTO: u32 = 321;
const IDM_UPDATE_PREVIEW: u32 = 322;
const IDM_UPDATE_INSTALL: u32 = 323;
const IDM_EXIT:           u32 = 400;
const IDM_MONITOR_BASE:   u32 = 500; // +index per monitor
const IDM_THEME_CLASSIC:  u32 = 601;
const IDM_THEME_NEON:     u32 = 602;
const IDM_THEME_CHOSEWAY: u32 = 603;

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
    let mut list: Vec<MonitorInfo> = raw.into_iter()
        .map(|(_, mi)| MonitorInfo {
            rect: (mi.rcMonitor.left, mi.rcMonitor.top, mi.rcMonitor.right, mi.rcMonitor.bottom),
            work_rect: (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom),
            primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
        })
        .collect();
    // EnumDisplayMonitors order is unspecified: primary first, then left to right.
    list.sort_by_key(|m| (!m.primary, m.rect.0, m.rect.1));
    list
}

// ─── Dynamic tray icon ────────────────────────────────────────────────────────
fn update_tray_icon(hwnd: HWND, state: &SharedState) {
    let (c5h, x5h, c_err, x_err, cr_err, cr_pct, codex_tip) = {
        let s = state.lock();
        (
            s.claude.util("5h"),
            s.codex.primary_utilization(),
            s.claude_error.clone(),
            s.codex_error.clone(),
            s.cursor_error.clone(),
            s.cursor.auto_usage_pct,
            s.codex.tooltip(),
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
            let cr_str = cr_pct.map(|v| format!(" · Cursor {:.0}%", v * 100.0)).unwrap_or_default();
            let err = if !c_err.is_empty() || !x_err.is_empty() || !cr_err.is_empty() {
                " ⚠".to_string()
            } else { String::new() };
            format!("Claude {} · Codex {}{}{}\0", c_str, codex_tip, cr_str, err)
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
        let n = tip_w.len().min(nid.szTip.len() - 1);
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
        let n = tip_w.len().min(nid.szTip.len() - 1);
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

/// Win10 uses a taskbar child; Win11 uses a separate composited surface inside
/// the taskbar bounds. Parenting alone does not establish visible rendering.
/// Taskbar on the selected monitor: `Shell_TrayWnd` on the primary display, a
/// `Shell_SecondaryTrayWnd` elsewhere. Falls back to the primary taskbar.
unsafe fn selected_taskbar(hwnd: HWND) -> (HWND, bool) {
    let primary = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default();
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const SharedState;
    let target = if ptr.is_null() { None } else {
        let s = (&*ptr).lock();
        s.monitors.get(s.settings.monitor_idx).map(|m| m.rect)
    };
    let Some(target) = target else { return (primary, true) };
    let mut candidates = vec![primary];
    let mut prev = HWND::default();
    while candidates.len() < 16 {
        let next = FindWindowExW(HWND::default(), prev, w!("Shell_SecondaryTrayWnd"), PCWSTR::null())
            .unwrap_or_default();
        if next.0.is_null() { break; }
        candidates.push(next);
        prev = next;
    }
    for (i, taskbar) in candidates.iter().enumerate() {
        if taskbar.0.is_null() { continue; }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(MonitorFromWindow(*taskbar, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            let m = info.rcMonitor;
            if (m.left, m.top, m.right, m.bottom) == target { return (*taskbar, i == 0); }
        }
    }
    (primary, true)
}

fn register_appbar(hwnd: HWND, appbar_w: i32) -> i32 {
    unsafe {
        let (taskbar, primary) = selected_taskbar(hwnd);
        let mut rect = RECT::default();
        if taskbar.0.is_null() || GetWindowRect(taskbar, &mut rect).is_err()
            || rect.right <= rect.left || rect.bottom <= rect.top
        {
            appbar_status(hwnd, 5, "taskbar unavailable; edge fallback");
            return position_summary_strip(hwnd, appbar_w);
        }
        // Primary: hug the notification area. Secondary taskbars have no tray, only a
        // clock: use its button when Explorer exposes one, else keep clear of it.
        let edge = FindWindowExW(taskbar, None,
            if primary { w!("TrayNotifyWnd") } else { w!("ClockButton") }, PCWSTR::null())
            .unwrap_or_default();
        let mut edge_rect = RECT::default();
        let tray_left = if !edge.0.is_null() && GetWindowRect(edge, &mut edge_rect).is_ok()
            && edge_rect.left > rect.left && edge_rect.left <= rect.right
        { edge_rect.left }
        else if primary { rect.right }
        else { rect.right - (SECONDARY_CLOCK_W as f32 * system_scale()).round() as i32 };
        if windows_compat::windows11() {
            let mut monitor = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(MonitorFromWindow(taskbar, MONITOR_DEFAULTTONEAREST), &mut monitor).as_bool() {
                let bounds = monitor.rcMonitor;
                return position_taskbar_surface(hwnd, taskbar, rect, tray_left, appbar_w, bounds);
            }
            appbar_status(hwnd, 5, "taskbar monitor unavailable; edge fallback");
            return position_summary_strip(hwnd, appbar_w);
        }
        if !attach_taskbar_child(hwnd, taskbar) {
            appbar_status(hwnd, 6, "child attachment failed; edge fallback");
            return position_summary_strip(hwnd, appbar_w);
        }
        if SUMMARY_FULLSCREEN_HIDDEN.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        let (x, y, width, height) = platform::taskbar_child_rect(
            (rect.left, rect.top, rect.right, rect.bottom), tray_left, appbar_w);
        let mut old = RECT::default();
        let _ = GetWindowRect(hwnd, &mut old);
        if old.left != rect.left + x || old.top != rect.top + y
            || old.right - old.left != width || old.bottom - old.top != height
        {
            let _ = SetWindowPos(hwnd, HWND_TOP, x, y, width, height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW);
            log_window_state("[appbar] embedded", hwnd);
        } else {
            // Restore visibility even when geometry is unchanged.
            let _ = SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        appbar_status(hwnd, 1, "Win10 child visible");
        height
    }
}

fn appbar_status(hwnd: HWND, code: u32, reason: &str) {
    if APPBAR_STATUS.swap(code, std::sync::atomic::Ordering::Relaxed) != code {
        info!("[appbar] status={} reason={}", code, reason);
        log_window_state("[appbar] status changed", hwnd);
    }
}

/// Independent DWM surface: no child clipping or destruction with Explorer's tree.
unsafe fn configure_taskbar_surface(hwnd: HWND) -> Result<()> {
    if !GetParent(hwnd).unwrap_or_default().0.is_null() { let _ = SetParent(hwnd, None); }
    if !GetParent(hwnd).unwrap_or_default().0.is_null() { return Err(windows::core::Error::from_win32()); }
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    SetWindowLongW(hwnd, GWL_STYLE, ((style & !(WS_CHILD.0 | WS_CLIPSIBLINGS.0)) | WS_POPUP.0) as i32);
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let wanted = (ex & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0;
    SetWindowLongW(hwnd, GWL_EXSTYLE, wanted as i32);
    // A layered window is invisible until its opacity/redirection is initialized.
    SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
    if style & WS_CHILD.0 != 0 || ex != wanted {
        SetWindowPos(hwnd, None, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED)?;
    }
    Ok(())
}

unsafe fn foreground_covers_monitor(hwnd: HWND, bounds: RECT) -> bool {
    let foreground = GetForegroundWindow();
    let mut rect = RECT::default();
    let mut class = [0u16; 128];
    let length = GetClassNameW(foreground, &mut class).max(0) as usize;
    let name = String::from_utf16_lossy(&class[..length.min(class.len())]);
    foreground != hwnd && !foreground.0.is_null() && !IsIconic(foreground).as_bool()
        && !matches!(name.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
        && GetWindowLongW(foreground, GWL_STYLE) as u32 & WS_CAPTION.0 == 0
        && GetWindowRect(foreground, &mut rect).is_ok()
        && rect.left <= bounds.left && rect.top <= bounds.top
        && rect.right >= bounds.right && rect.bottom >= bounds.bottom
}

unsafe fn position_taskbar_surface(hwnd: HWND, taskbar: HWND, taskbar_rect: RECT,
    tray_left: i32, width: i32, bounds: RECT) -> i32 {
    let height = (taskbar_rect.bottom - taskbar_rect.top).max(1);
    let layout = platform::taskbar_surface_rect(
        (taskbar_rect.left, taskbar_rect.top, taskbar_rect.right, taskbar_rect.bottom),
        (bounds.left, bounds.top, bounds.right, bounds.bottom), tray_left, width);
    let fullscreen = foreground_covers_monitor(hwnd, bounds);
    if !IsWindowVisible(taskbar).as_bool() || layout.is_none() || fullscreen {
        let _ = ShowWindow(hwnd, SW_HIDE);
        appbar_status(hwnd, if fullscreen { 4 } else { 3 },
            if fullscreen { "fullscreen; surface hidden" } else { "taskbar hidden/offscreen; surface hidden" });
        return height;
    }
    let (x, y, w, h) = layout.unwrap();
    let was_visible = IsWindowVisible(hwnd).as_bool();
    if let Err(error) = configure_taskbar_surface(hwnd) {
        log::warn!("[appbar] surface setup failed: {:?}", error.code());
        appbar_status(hwnd, 7, "surface setup failed; edge fallback");
        return position_summary_strip(hwnd, width);
    }
    SUMMARY_FULLSCREEN_HIDDEN.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut old = RECT::default();
    let _ = GetWindowRect(hwnd, &mut old);
    let changed = old.left != x || old.top != y || old.right - old.left != w || old.bottom - old.top != h;
    let flags = if changed { SWP_NOACTIVATE | SWP_SHOWWINDOW }
        else { SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOMOVE | SWP_NOSIZE };
    if let Err(error) = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, flags) {
        log::warn!("[appbar] surface position failed: {:?}", error.code());
        appbar_status(hwnd, 7, "surface position failed; edge fallback");
        return position_summary_strip(hwnd, width);
    }
    if changed || !was_visible { let _ = InvalidateRect(hwnd, None, false); }
    appbar_status(hwnd, 2, "Win11 independent surface inside taskbar");
    h
}

/// SetParent does not change WS_POPUP/WS_CHILD itself. Verify the resulting
/// parent: its return value can be null even after a successful first attach.
unsafe fn attach_taskbar_child(hwnd: HWND, taskbar: HWND) -> bool {
    if taskbar.0.is_null() { return false; }
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    let child_style = (style & !WS_POPUP.0) | WS_CHILD.0 | WS_CLIPSIBLINGS.0;
    if style != child_style { SetWindowLongW(hwnd, GWL_STYLE, child_style as i32); }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let child_ex = ex & !(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0 | WS_EX_APPWINDOW.0 | WS_EX_NOACTIVATE.0);
    if ex != child_ex { SetWindowLongW(hwnd, GWL_EXSTYLE, child_ex as i32); }
    if GetParent(hwnd).unwrap_or_default() != taskbar {
        let _ = SetParent(hwnd, taskbar);
    }
    if GetParent(hwnd).unwrap_or_default() != taskbar {
        // Failed attachment must not leave a desktop popup carrying WS_CHILD.
        SetWindowLongW(hwnd, GWL_STYLE, style as i32);
        SetWindowLongW(hwnd, GWL_EXSTYLE, ex as i32);
        return false;
    }
    if style != child_style || ex != child_ex {
        let _ = SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED);
    }
    true
}

fn unregister_appbar(hwnd: HWND) {
    APPBAR_STATUS.store(0, std::sync::atomic::Ordering::Relaxed);
    SUMMARY_FULLSCREEN_HIDDEN.store(false, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        let _ = SetParent(hwnd, None);
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        SetWindowLongW(hwnd, GWL_EXSTYLE, (ex & !WS_EX_NOACTIVATE.0) as i32);
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

/// Temporary fallback when taskbar embedding is unavailable; retried by the timer.
fn position_summary_strip(hwnd: HWND, width: i32) -> i32 {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const SharedState;
        let fallback = (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let (work, bounds) = if !ptr.is_null() {
            let s = (&*ptr).lock();
            s.monitors.get(s.settings.monitor_idx).or_else(|| s.monitors.first())
                .map(|m| (m.work_rect, m.rect)).unwrap_or((fallback, fallback))
        } else { (fallback, fallback) };
        let (x, y, w, h) = platform::summary_rect(work, width,
            WINDOW_DPI.load(std::sync::atomic::Ordering::Relaxed));
        let foreground = GetForegroundWindow();
        let mut foreground_rect = RECT::default();
        let mut class = [0u16; 128];
        let length = GetClassNameW(foreground, &mut class).max(0) as usize;
        let name = String::from_utf16_lossy(&class[..length.min(class.len())]);
        let fullscreen = foreground != hwnd && !foreground.0.is_null()
            && name != "Progman" && name != "WorkerW" && name != "Shell_TrayWnd"
            && GetWindowLongW(foreground, GWL_STYLE) as u32 & WS_CAPTION.0 == 0
            && GetWindowRect(foreground, &mut foreground_rect).is_ok()
            && foreground_rect.left <= bounds.0 && foreground_rect.top <= bounds.1
            && foreground_rect.right >= bounds.2 && foreground_rect.bottom >= bounds.3;
        if fullscreen {
            if IsWindowVisible(hwnd).as_bool() {
                SUMMARY_FULLSCREEN_HIDDEN.store(true, std::sync::atomic::Ordering::Relaxed);
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return h;
        }
        if SUMMARY_FULLSCREEN_HIDDEN.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        if GetParent(hwnd).unwrap_or_default().0 != std::ptr::null_mut() { let _ = SetParent(hwnd, None); }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        SetWindowLongW(hwnd, GWL_STYLE, ((style & !(WS_CHILD.0 | WS_CLIPSIBLINGS.0)) | WS_POPUP.0) as i32);
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        SetWindowLongW(hwnd, GWL_EXSTYLE,
            ((ex & !(WS_EX_LAYERED.0 | WS_EX_APPWINDOW.0)) | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as i32);
        let mut old = RECT::default();
        let _ = GetWindowRect(hwnd, &mut old);
        if old.left != x || old.top != y || old.right - old.left != w || old.bottom - old.top != h {
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW);
        }
        h
    }
}

// ─── Window geometry ──────────────────────────────────────────────────────────
fn apply_window_layout(hwnd: HWND, state: &SharedState) {
    if is_dashboard(hwnd){layout_dashboard(hwnd,state);return;}
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

    if settings.theme == Theme::ChoseWay && settings.display_mode != DisplayMode::AppBar {
        layout_dashboard(hwnd,state); return;
    }
    match settings.display_mode {
        DisplayMode::Floating => {
            let fw = effective_win_w(&settings).max(MIN_W);
            let fh = if settings.win_h < MIN_H { calc_window_height(&settings, layout) } else { settings.win_h };
            let fx = if settings.win_x == -1 { ml + mw - fw - 16 } else { settings.win_x }.clamp(ml, (mr - fw).max(ml));
            let fy = if settings.win_y == -1 { mt + mh - fh - 48 } else { settings.win_y }.clamp(mt, (mb - fh).max(mt));
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
            let fw = effective_win_w(&settings).max(MIN_W);
            let fh = if settings.win_h < MIN_H { calc_window_height(&settings, layout) } else { settings.win_h };
            let fx = if settings.win_x == -1 { ml + mw - fw - 16 } else { settings.win_x }.clamp(ml, (mr - fw).max(ml));
            let fy = if settings.win_y == -1 { mt + mh - fh - 48 } else { settings.win_y }.clamp(mt, (mb - fh).max(mt));
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
                // Reset the previous mode; register_appbar configures the platform path.
                let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                let new_ex = ex & !(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0 | WS_EX_APPWINDOW.0 | WS_EX_NOACTIVATE.0);
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
            "{} hwnd={:?} visible={} style={:#x} exstyle={:#x} parent={:?} dpi={} window=({},{})->({},{}) client={}x{}",
            tag,
            hwnd,
            IsWindowVisible(hwnd).as_bool(),
            style,
            ex,
            GetParent(hwnd).unwrap_or_default(),
            windows_compat::dpi(hwnd),
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

fn notify_update_ready(hwnd: HWND, state: &SharedState) {
    static ANNOUNCED: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let package = state.lock().update.ready.clone();
    let Some(package) = package else { return; };
    let Ok(mut last) = ANNOUNCED.lock() else { return; };
    if *last == package.version { return; }
    *last = package.version.clone();
    let mut nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd, uID: TRAY_UID, uFlags: NIF_INFO, dwInfoFlags: NIIF_INFO, ..Default::default() };
    let title: Vec<u16> = "Usage Monitor 有新版本".encode_utf16().collect();
    let text: Vec<u16> = format!("v{} 已校验并准备好。右键托盘 → 软件更新 → 安装更新并重启。", package.version).encode_utf16().collect();
    let n = title.len().min(nid.szInfoTitle.len() - 1); nid.szInfoTitle[..n].copy_from_slice(&title[..n]);
    let n = text.len().min(nid.szInfo.len() - 1); nid.szInfo[..n].copy_from_slice(&text[..n]);
    unsafe { let _ = Shell_NotifyIconW(NIM_MODIFY, &nid); }
}

fn show_update_error(hwnd: HWND) {
    unsafe {
        let _ = MessageBoxW(hwnd, w!("更新无法启动：缓存校验失败，或便携版未放在本机目录。请重新检查更新。"),
            w!("Usage Monitor 更新"), MB_OK | MB_ICONERROR);
    }
}

// ─── Context menu ─────────────────────────────────────────────────────────────
fn show_context_menu(hwnd: HWND, state: &SharedState) {
    let (settings, monitors, update) = {
        let s = state.lock();
        (s.settings.clone(), s.monitors.clone(), s.update.clone())
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
        AppendMenuW(hmenu, MF_STRING, IDM_CODEX_DETAILS as usize, w!("Codex 用量详情…")).ok();

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

        // ── Theme (radio-style submenu) ───────────────────────────────────
        let h_theme = CreatePopupMenu().unwrap_or_default();
        let tf = |t: Theme| if settings.theme == t { MF_CHECKED } else { MF_UNCHECKED };
        AppendMenuW(h_theme, MF_STRING | tf(Theme::ChoseWay), IDM_THEME_CHOSEWAY as usize, w!("千往 ChoseWay")).ok();
        AppendMenuW(h_theme, MF_STRING | tf(Theme::Neon),
            IDM_THEME_NEON as usize, w!("Neon HUD（赛博）")).ok();
        AppendMenuW(h_theme, MF_STRING | tf(Theme::Classic),
            IDM_THEME_CLASSIC as usize, w!("Classic（经典）")).ok();
        AppendMenuW(hmenu, MF_POPUP, h_theme.0 as usize, w!("主题")).ok();

        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).ok();

        // ── Manual refresh ─────────────────────────────────────────────────
        AppendMenuW(hmenu, MF_STRING, IDM_MANUAL_REFRESH as usize, w!("立即刷新")).ok();

        let h_update = CreatePopupMenu().unwrap_or_default();
        let status: Vec<u16> = format!("v{} · {}\0", env!("CARGO_PKG_VERSION"),
            if update.message.is_empty() { "等待检查" } else { &update.message }).encode_utf16().collect();
        let _ = AppendMenuW(h_update, MF_STRING | MF_GRAYED, 0, PCWSTR(status.as_ptr()));
        let _ = AppendMenuW(h_update, MF_STRING, IDM_UPDATE_CHECK as usize, w!("检查更新"));
        let _ = AppendMenuW(h_update, MF_STRING | if update.ready.is_some() { MF_ENABLED } else { MF_GRAYED },
            IDM_UPDATE_INSTALL as usize, w!("安装更新并重启"));
        let _ = AppendMenuW(h_update, MF_STRING | if settings.auto_update_check { MF_CHECKED } else { MF_UNCHECKED },
            IDM_UPDATE_AUTO as usize, w!("自动检查并准备更新"));
        let _ = AppendMenuW(h_update, MF_STRING | if settings.update_preview { MF_CHECKED } else { MF_UNCHECKED },
            IDM_UPDATE_PREVIEW as usize, w!("接收预览版"));
        let _ = AppendMenuW(hmenu, MF_POPUP, h_update.0 as usize, w!("软件更新"));

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
            for (i, m) in monitors.iter().enumerate() {
                let lbl: Vec<u16> = format!("显示器 {}{} · {}×{}\0", i + 1,
                    if m.primary { " · 主屏" } else { "" }, m.rect.2 - m.rect.0, m.rect.3 - m.rect.1)
                    .encode_utf16().collect();
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
            IDM_UPDATE_CHECK => { usage_monitor::updater::request_check(); }
            IDM_UPDATE_AUTO | IDM_UPDATE_PREVIEW => {
                {
                    let mut s = state.lock();
                    if cmd == IDM_UPDATE_AUTO { s.settings.auto_update_check = !s.settings.auto_update_check; }
                    else { s.settings.update_preview = !s.settings.update_preview; s.update.ready = None; }
                    save_settings(&s.settings);
                }
                if cmd == IDM_UPDATE_PREVIEW || state.lock().settings.auto_update_check {
                    usage_monitor::updater::request_check();
                }
            }
            IDM_UPDATE_INSTALL => {
                let package = state.lock().update.ready.clone();
                if let Some(package) = package {
                    match usage_monitor::windows_update::launch(&package) {
                        Ok(()) => { let _ = PostMessageW(hwnd, WM_CLOSE, None, None); }
                        Err(_) => { show_update_error(hwnd); }
                    }
                }
            }
            IDM_CODEX_DETAILS => {
                let s = state.lock();
                let details = format!("{}\n{}", s.codex.details(), s.codex_error);
                drop(s);
                let wide: Vec<u16> = details.encode_utf16().chain(Some(0)).collect();
                MessageBoxW(hwnd, PCWSTR(wide.as_ptr()), w!("Codex 用量详情（只读查询）"), MB_OK | MB_ICONINFORMATION);
            }
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
                let saved=state.lock().settings.clone();
                if saved.theme==Theme::ChoseWay&&prev_mode==DisplayMode::AppBar&&saved.win_x != -1&&saved.win_y != -1 {
                    let _=SetWindowPos(hwnd,None,saved.win_x,saved.win_y,0,0,SWP_NOSIZE|SWP_NOZORDER|SWP_NOACTIVATE);
                }
                apply_window_layout(hwnd, state);
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }

            IDM_MODE_APPBAR => {
                state.lock().settings.display_mode = DisplayMode::AppBar;
                save_settings(&state.lock().settings);
                apply_window_layout(hwnd, state);
            }

            IDM_THEME_CLASSIC | IDM_THEME_NEON | IDM_THEME_CHOSEWAY => {
                let theme = match cmd {IDM_THEME_NEON=>Theme::Neon,IDM_THEME_CHOSEWAY=>Theme::ChoseWay,_=>Theme::Classic};
                {
                    let mut s = state.lock();
                    if s.settings.theme != theme {
                        s.settings.theme = theme;
                        // Size follows the theme: width back to the theme default, height auto.
                        s.settings.win_w = -1;
                        s.settings.win_h = -1;
                    }
                    save_settings(&s.settings);
                }
                overlay::set_theme(theme);
                info!("[theme] switched to {:?}", theme);
                apply_window_layout(hwnd, state);
                InvalidateRect(hwnd, None, false).ok();
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
                claude::reset_refresh_attempts();
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
                    if state.lock().settings.theme==Theme::ChoseWay {
                        let rect=monitors[idx].work_rect;
                        let _=SetWindowPos(hwnd,None,rect.0+20,rect.1+20,0,0,SWP_NOSIZE|SWP_NOZORDER|SWP_NOACTIVATE);
                    }
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
    if settings.theme == Theme::ChoseWay { layout_dashboard(hwnd,state); return; }

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
pub fn run() -> Result<()> {
    init_logger();
    info!("[appbar] build={} Windows 11={} (Win11 surface / Win10 child)", windows_compat::windows_build(), windows_compat::windows11());
    WINDOW_DPI.store(windows_compat::initial_dpi(), std::sync::atomic::Ordering::Relaxed);
    TASKBAR_CREATED.store(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }, std::sync::atomic::Ordering::Relaxed);

    // Diagnostic mode: `usage-monitor --probe [--cli]` runs one Claude fetch, prints the
    // outcome to stdout and the log, and exits without creating any window.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("Usage Monitor {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--probe-codex") {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().expect("HTTP client");
        match rt.block_on(codex::fetch(&client)) {
            Ok(usage) => {
                println!("{}", usage.details());
                for row in usage.display_rows() {
                    println!("row: {} {}", row.label, row.value.unwrap_or_else(|| state::fmt_pct(row.utilization)));
                }
            }
            Err(error) => { eprintln!("Codex: {}", error); std::process::exit(1); }
        }
        return Ok(());
    }
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
        overlay::set_theme(settings.theme);
        let monitors  = enum_monitors();
        info!("Found {} monitors", monitors.len());

        let shared = new_shared();
        if usage_monitor::updater::cache_root().join("last-result.json").is_file() {
            shared.lock().update.message = "上次更新取消或失败，可重新检查；旧版备份已保留".into();
            let _ = std::fs::remove_file(usage_monitor::updater::cache_root().join("last-result.json"));
        }
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

        let (mut wx, mut wy, mut ww, mut wh) = match settings.display_mode {
            DisplayMode::Floating => {
                let fw = effective_win_w(&settings).max(MIN_W);
                // Use calc_window_height when win_h is -1 (auto) or from settings.
                // No data yet at startup → 2 Claude rows; WM_APP resizes once rows arrive.
                let fh = if settings.win_h < MIN_H {
                    calc_window_height(&settings, LayoutInfo { claude_rows: 2, codex_rows: 2, banner: false })
                } else {
                    settings.win_h
                };
                let fx = if settings.win_x == -1 { ml + mw - fw - 16 } else { settings.win_x }.clamp(ml, (mr - fw).max(ml));
                let fy = if settings.win_y == -1 { mt + mh - fh - 48 } else { settings.win_y }.clamp(mt, (mb - fh).max(mt));
                (fx, fy, fw, fh)
            }
            DisplayMode::CompactBar => {
                let fw = effective_win_w(&settings).max(MIN_W);
                let fh = if settings.win_h < MIN_H {
                    calc_window_height(&settings, LayoutInfo { claude_rows: 2, codex_rows: 2, banner: false })
                } else {
                    settings.win_h
                };
                let fx = if settings.win_x == -1 { ml + mw - fw - 16 } else { settings.win_x }.clamp(ml, (mr - fw).max(ml));
                let fy = if settings.win_y == -1 { mt + mh - fh - 48 } else { settings.win_y }.clamp(mt, (mb - fh).max(mt));
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

        if settings.theme == Theme::ChoseWay && settings.display_mode != DisplayMode::AppBar {
            let scale=system_scale();
            let size=overlay::choseway::preferred_size(&shared.lock(),(mw as f32/scale) as i32-16,(mh as f32/scale) as i32-16);
            ww=(size.0 as f32*scale).round()as i32;wh=(size.1 as f32*scale).round()as i32;
            wx=if settings.win_x==-1{(mr-ww-16).max(ml)}else{settings.win_x.clamp(ml,(mr-ww).max(ml))};
            wy=if settings.win_y==-1{(mb-wh-16).max(mt)}else{settings.win_y.clamp(mt,(mb-wh).max(mt))};
        }
        // AppBar starts neutral; register_appbar selects child or composited surface.
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

        MAIN_WINDOW.store(hwnd.0 as isize,std::sync::atomic::Ordering::Relaxed);
        WINDOW_DPI.store(windows_compat::dpi(hwnd), std::sync::atomic::Ordering::Relaxed);
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

        if settings.theme == Theme::ChoseWay && settings.display_mode != DisplayMode::AppBar { layout_dashboard(hwnd,&shared); }
        // Tray icon
        create_tray_icon(hwnd);
        info!("Tray icon added");

        // AppBar registration (if starting in that mode)
        if settings.display_mode == DisplayMode::AppBar {
            register_appbar(hwnd, appbar_width(&settings));
        }

        // Background polling
        let window_id = hwnd.0 as usize;
        let _monitor = monitor::spawn(shared.clone(), move || {
            let _ = PostMessageW(HWND(window_id as *mut _), WM_APP, None, None);
        });
        set_refresh_secs(settings.refresh_rate.secs());

        // Timers: 1s appbar relayout + 100ms floating hover auto-hide.
        SetTimer(hwnd, TIMER_ID, 1000, None);
        SetTimer(hwnd, HOVER_TIMER_ID, 100, None);

        // AppBar positioning controls visibility (auto-hide/fullscreen included).
        if settings.display_mode != DisplayMode::AppBar { let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE); }

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

        let taskbar_created = TASKBAR_CREATED.load(std::sync::atomic::Ordering::Relaxed);
        if taskbar_created != 0 && msg == taskbar_created {
            create_tray_icon(hwnd);
            if let Some(shared) = get_state() { apply_window_layout(hwnd, shared); }
            return LRESULT(0);
        }
        match msg {
            WM_CLOSE if is_dashboard(hwnd) => { hide_dashboard(); LRESULT(0) }
            WM_DASHBOARD_DISMISS => { hide_dashboard(); LRESULT(0) }
            // Losing activation (another window, Alt+Tab, Start) closes the popup.
            WM_ACTIVATE if is_dashboard(hwnd) => {
                if wparam.0 & 0xFFFF == 0 && !MODAL_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
                    hide_dashboard();
                }
                LRESULT(0)
            }
            WM_CTLCOLOREDIT => {
                use windows::Win32::Graphics::Gdi::{SetBkColor,SetTextColor,HDC};
                let theme = get_state().map(|s| s.lock().settings.theme).unwrap_or(Theme::ChoseWay);
                let (bg, fg) = overlay::popup::field_color(theme);
                let dc=HDC(wparam.0 as *mut _); SetBkColor(dc,colorref(bg));SetTextColor(dc,colorref(fg));
                LRESULT(cookie_brush(theme).0 as isize)
            }
            WM_LBUTTONUP => {
                if let Some(shared)=get_state() {
                    let s=shared.lock();
                    let panel=is_dashboard(hwnd)||(s.settings.theme==Theme::ChoseWay&&s.settings.display_mode!=DisplayMode::AppBar);
                    let bar=!is_dashboard(hwnd)&&s.settings.display_mode==DisplayMode::AppBar;
                    let mut rect=RECT::default();let _=GetClientRect(hwnd,&mut rect);
                    let action=if panel {overlay::popup::hit_test(&s,rect.right,rect.bottom,ui_scale(hwnd),(lparam.0&65535)as i16 as i32,((lparam.0>>16)&65535)as i16 as i32)} else {None};
                    drop(s);
                    if bar {toggle_dashboard(hwnd,shared);} else if let Some(cmd)=action {ui_action(hwnd,cmd,shared);}
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some(shared)=get_state() {
                    let mut s=shared.lock();
                    if is_dashboard(hwnd)||(s.settings.theme==Theme::ChoseWay&&s.settings.display_mode!=DisplayMode::AppBar) {
                        let mut rect=RECT::default();let _=GetClientRect(hwnd,&mut rect);
                        let hover=overlay::popup::hit_test(&s,rect.right,rect.bottom,ui_scale(hwnd),(lparam.0&65535)as i16 as i32,((lparam.0>>16)&65535)as i16 as i32);
                        if hover!=s.ui_hover{s.ui_hover=hover;let _=InvalidateRect(hwnd,None,false);}
                        // Ask for WM_MOUSELEAVE so a hover highlight never sticks.
                        let mut track = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT>() as u32,
                            dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut track);
                    }
                }
                LRESULT(0)
            }
            WM_MOUSE_LEAVE => {
                if let Some(shared)=get_state() {
                    let mut s=shared.lock();
                    if s.ui_hover.take().is_some() { let _=InvalidateRect(hwnd,None,false); }
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if let Some(shared)=get_state() {
                    let mut s=shared.lock();
                    if is_dashboard(hwnd)||(s.settings.theme==Theme::ChoseWay&&s.settings.display_mode!=DisplayMode::AppBar) {
                        let mut rect=RECT::default();let _=GetClientRect(hwnd,&mut rect);
                        let controls=overlay::popup::controls(&s,rect.right,rect.bottom,ui_scale(hwnd));
                        let mut command=None;
                        let mut dismiss=false;
                        match wparam.0 {
                            // Esc: settings → overview → closed popup.
                            27=>{ if !s.settings_open && is_dashboard(hwnd) { dismiss=true; } s.settings_open=false;s.ui_focus=0; },
                            9=>{s.ui_focus=s.ui_focus%controls.len().max(1)+1;},
                            13|32=>{command=s.ui_focus.checked_sub(1).and_then(|i|controls.get(i)).map(|b|b.command);},
                            _=>{}
                        }
                        drop(s);
                        if dismiss { hide_dashboard(); return LRESULT(0); }
                        if let Some(cmd)=command{ui_action(hwnd,cmd,shared);}else{sync_cookie_edit(hwnd,shared);}
                        let _=InvalidateRect(hwnd,None,false);
                    }
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                if !is_dashboard(hwnd) { WINDOW_DPI.store(dpi.max(96), std::sync::atomic::Ordering::Relaxed); }
                if let Some(shared) = get_state() {
                    let mode = shared.lock().settings.display_mode;
                    if mode == DisplayMode::Floating && lparam.0 != 0 {
                        let rect = &*(lparam.0 as *const RECT);
                        let _ = SetWindowPos(hwnd, None, rect.left, rect.top, rect.right - rect.left,
                            rect.bottom - rect.top, SWP_NOZORDER | SWP_NOACTIVATE);
                    } else { apply_window_layout(hwnd, shared); }
                }
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                if let Some(shared) = get_state() {
                    let monitors = enum_monitors();
                    {
                        let mut s = shared.lock();
                        s.settings.monitor_idx = s.settings.monitor_idx.min(monitors.len().saturating_sub(1));
                        s.monitors = monitors;
                    }
                    apply_window_layout(hwnd, shared);
                }
                LRESULT(0)
            }
            WM_POWERBROADCAST if wparam.0 == 0x12 => {
                monitor::wake_pollers();
                LRESULT(1)
            }
            // Avoid background erase flicker; we fully paint in WM_PAINT.
            WM_ERASEBKGND => LRESULT(1),

            // ── Paint ──────────────────────────────────────────────────────
            WM_PAINT => {
                if let Some(shared) = get_state() {
                    let s = shared.lock();
                    if is_dashboard(hwnd) { overlay::popup::paint(hwnd,&s,ui_scale(hwnd)); }
                    else { overlay::paint(hwnd, &s); }
                }
                LRESULT(0)
            }

            // ── 1-second timer → countdown repaint ────────────────────────
            WM_TIMER if wparam.0 == TIMER_ID => {
                if is_dashboard(hwnd){let _=InvalidateRect(hwnd,None,false);return LRESULT(0);}
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
                    if s.settings.display_mode != DisplayMode::Floating || !s.settings.hover_auto_hide || s.settings_open {
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
                    notify_update_ready(hwnd, sh);
                    let cw_float={let s=sh.lock();s.settings.theme==Theme::ChoseWay&&s.settings.display_mode==DisplayMode::Floating};
                    if cw_float {layout_dashboard(hwnd,sh);}
                    // Grow/shrink the floating window when service row counts or the
                    // banner line changes (e.g. the Fable row arriving after first fetch).
                    let key = {
                        let s = sh.lock();
                        let l = s.layout();
                        ((l.claude_rows as u32) << 9) | ((l.codex_rows as u32) << 1) | (l.banner as u32)
                    };
                    if LAST_LAYOUT_KEY.swap(key, std::sync::atomic::Ordering::Relaxed) != key {
                        resize_to_content(hwnd, sh);
                    }
                }
                repaint_dashboard();
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
                            toggle_dashboard(hwnd,sh);
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
                    .map(|s| s.lock().settings.display_mode == DisplayMode::Floating || is_dashboard(hwnd))
                    .unwrap_or(true);

                if !floating {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }

                let sx = (lparam.0 & 0xFFFF) as i16 as i32;
                let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let mut wr = RECT::default();
                GetWindowRect(hwnd, &mut wr).ok();

                if let Some(shared)=get_state() {
                    let theme=shared.lock().settings.theme;
                    if is_dashboard(hwnd)||theme==Theme::ChoseWay {
                        let x=sx-wr.left;let y=sy-wr.top;
                        let s=shared.lock();
                        let button=overlay::popup::hit_test(&s,wr.right-wr.left,wr.bottom-wr.top,ui_scale(hwnd),x,y).is_some();
                        let caption=if theme==Theme::ChoseWay {70.0} else {48.0};
                        return LRESULT(if !button && y < (caption*ui_scale(hwnd))as i32 {HTCAPTION}else{HTCLIENT}as isize);
                    }
                }
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
                let appbar = get_state().map(|s| s.lock().settings.display_mode == DisplayMode::AppBar).unwrap_or(false);
                mmi.ptMinTrackSize.x = if appbar { 1 } else { MIN_W };
                mmi.ptMinTrackSize.y = if appbar { 1 } else { MIN_H };
                LRESULT(0)
            }

            // ── Track resize / move → update region & save pos ─────────────
            WM_SIZE | WM_MOVE => {
                if is_dashboard(hwnd) { if let Some(sh)=get_state(){sync_cookie_edit(hwnd,sh);} let _=InvalidateRect(hwnd,None,false); return LRESULT(0); }
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
            WM_DESTROY if is_dashboard(hwnd) => { DASHBOARD_WINDOW.store(0,std::sync::atomic::Ordering::Relaxed); LRESULT(0) }
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

#[cfg(test)]
mod taskbar_tests {
    use super::*;

    #[test]
    fn independent_surface_initializes_opacity_and_survives_taskbar_destruction() {
        unsafe {
            // Offscreen mock taskbar and real app WndProc, with no sampling/network/settings writes.
            extern "system" fn test_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
                if msg == WM_DESTROY { unsafe { DefWindowProcW(hwnd, msg, wp, lp) } }
                else { wnd_proc(hwnd, msg, wp, lp) }
            }
            let instance = HINSTANCE(GetModuleHandleW(None).unwrap().0);
            let class = WNDCLASSW { lpfnWndProc: Some(test_proc), hInstance: instance,
                lpszClassName: w!("UsageMonitorSurfaceTest"), ..Default::default() };
            assert_ne!(RegisterClassW(&class), 0);
            let parent = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("Mock taskbar"),
                WS_POPUP | WS_VISIBLE, -30000, -30000, 1200, 48, None, None, None, None).unwrap();
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class.lpszClassName, w!("Surface test"),
                WS_POPUP, -30000, -30000, 560, 48, None, None, instance, None).unwrap();
            let shared = new_shared();
            shared.lock().settings.display_mode = DisplayMode::AppBar;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &shared as *const SharedState as isize);
            let mut rect = RECT::default();
            GetWindowRect(parent, &mut rect).unwrap();
            let bounds = RECT { left: rect.left, top: rect.top - 1000, right: rect.right, bottom: rect.bottom };
            position_taskbar_surface(hwnd, parent, rect, rect.right - 200, 180, bounds);
            assert!(IsWindowVisible(hwnd).as_bool());
            assert!(GetParent(hwnd).unwrap_or_default().0.is_null());
            let mut actual = RECT::default();
            GetWindowRect(hwnd, &mut actual).unwrap();
            assert_eq!(actual.top, rect.top);
            assert_eq!(actual.bottom, rect.bottom, "floating minimum height must not stretch Bar mode");
            assert_eq!(actual.right - actual.left, 180);
            let mut alpha = 0u8;
            GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), None).unwrap();
            assert_eq!(alpha, 255);
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_ne!(ex & WS_EX_TOPMOST.0, 0);
            assert_ne!(ex & WS_EX_NOACTIVATE.0, 0);
            let _ = ShowWindow(parent, SW_HIDE);
            position_taskbar_surface(hwnd, parent, rect, rect.right - 200, 180, bounds);
            assert!(!IsWindowVisible(hwnd).as_bool());
            let _ = ShowWindow(parent, SW_SHOWNOACTIVATE);
            position_taskbar_surface(hwnd, parent, rect, rect.right - 200, 180, bounds);
            assert!(IsWindowVisible(hwnd).as_bool());
            DestroyWindow(parent).unwrap();
            assert!(IsWindow(hwnd).as_bool(), "Explorer ownership must not destroy the independent surface");
            unregister_appbar(hwnd);
            assert_eq!(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_NOACTIVATE.0, 0);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DestroyWindow(hwnd).unwrap();
            UnregisterClassW(class.lpszClassName, instance).unwrap();
        }
    }

    // Real HWNDs, with a hidden mock taskbar: never change Explorer or user settings.
    #[test]
    fn popup_can_attach_fill_taskbar_and_detach() {
        unsafe {
            let parent = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("Taskbar test"),
                WS_POPUP, 100, 200, 1200, 48, None, None, None, None).unwrap();
            let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_LAYERED, w!("STATIC"), w!("Bar test"),
                WS_POPUP, 0, 0, 560, 40, None, None, None, None).unwrap();
            assert!(attach_taskbar_child(hwnd, parent));
            assert!(attach_taskbar_child(hwnd, parent), "reattaching should be harmless");
            assert_eq!(GetParent(hwnd).unwrap(), parent);
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            assert_ne!(style & WS_CHILD.0, 0);
            assert_eq!(style & WS_POPUP.0, 0);
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_eq!(ex & (WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0 | WS_EX_TOPMOST.0), 0);
            let mut parent_rect = RECT::default();
            GetWindowRect(parent, &mut parent_rect).unwrap();
            let (x, y, w, h) = platform::taskbar_child_rect(
                (parent_rect.left, parent_rect.top, parent_rect.right, parent_rect.bottom),
                parent_rect.right - 200, 560);
            MoveWindow(hwnd, x, y, w, h, false).unwrap();
            let mut child_rect = RECT::default();
            GetWindowRect(hwnd, &mut child_rect).unwrap();
            assert_eq!(child_rect.top, parent_rect.top);
            assert_eq!(child_rect.bottom, parent_rect.bottom);
            assert_eq!(child_rect.right, parent_rect.right - 200);
            unregister_appbar(hwnd);
            assert!(GetParent(hwnd).unwrap_or_default().0.is_null());
            assert_eq!(GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_CHILD.0, 0);
            DestroyWindow(parent).unwrap();
            let style = GetWindowLongW(hwnd, GWL_STYLE);
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
            assert!(!attach_taskbar_child(hwnd, parent), "stale Explorer handle must fail");
            assert_eq!(GetWindowLongW(hwnd, GWL_STYLE), style);
            assert_eq!(GetWindowLongW(hwnd, GWL_EXSTYLE), ex);
            DestroyWindow(hwnd).unwrap();
        }
    }
}

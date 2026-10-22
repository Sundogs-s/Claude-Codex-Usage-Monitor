// Included by windows.rs: native dashboard host and password Edit integration.
static MAIN_WINDOW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static DASHBOARD_WINDOW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static COOKIE_EDIT: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static COOKIE_FONT: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static COOKIE_OLD_PROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// Low-level mouse hook that closes the popup on a click outside it.
static DASHBOARD_HOOK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// When the popup was last dismissed; a bar click right after it must not reopen it.
static DASHBOARD_HIDDEN_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// A confirmation dialog owns the input; the popup must not auto-dismiss meanwhile.
static MODAL_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const WM_DASHBOARD_DISMISS: u32 = WM_USER + 3;
const DISMISS_REOPEN_GUARD_MS: u64 = 400;
fn is_dashboard(hwnd: HWND) -> bool {
    let value = DASHBOARD_WINDOW.load(std::sync::atomic::Ordering::Relaxed);
    value != 0 && value == hwnd.0 as isize
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
pub(crate) fn ui_scale(hwnd: HWND) -> f32 {
    let dpi = windows_compat::dpi(hwnd).max(96) as f32 / 96.0;
    unsafe {
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        if is_dashboard(hwnd) || rect.bottom > 200 {
            let (bw, bh) = if is_dashboard(hwnd) {
                overlay::popup::base_size(overlay::current_theme())
            } else {
                (640, 590)
            };
            dpi.min(rect.right.max(1) as f32 / bw as f32)
                .min(rect.bottom.max(1) as f32 / bh as f32)
                .max(0.5)
        } else {
            dpi
        }
    }
}
fn colorref(c: u32) -> COLORREF {
    COLORREF(((c >> 16) & 0xFF) | (c & 0xFF00) | ((c & 0xFF) << 16))
}
/// Solid brush behind the password Edit, one per theme.
fn cookie_brush(theme: Theme) -> windows::Win32::Graphics::Gdi::HBRUSH {
    static BRUSHES: [std::sync::atomic::AtomicIsize; 3] = [
        std::sync::atomic::AtomicIsize::new(0),
        std::sync::atomic::AtomicIsize::new(0),
        std::sync::atomic::AtomicIsize::new(0),
    ];
    let slot = &BRUSHES[match theme {
        Theme::Classic => 0,
        Theme::Neon => 1,
        Theme::ChoseWay => 2,
    }];
    let mut brush = slot.load(std::sync::atomic::Ordering::Relaxed);
    if brush == 0 {
        brush = unsafe { CreateSolidBrush(colorref(overlay::popup::field_color(theme).0)).0 as isize };
        slot.store(brush, std::sync::atomic::Ordering::Relaxed);
    }
    windows::Win32::Graphics::Gdi::HBRUSH(brush as *mut _)
}
fn dashboard_size(hwnd: HWND, state: &SharedState) -> (RECT, i32, i32) {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let bounds = info.rcWork;
        let scale = windows_compat::dpi(hwnd).max(96) as f32 / 96.0;
        let size = overlay::popup::preferred_size(
            &state.lock(),
            ((bounds.right - bounds.left - 16) as f32 / scale) as i32,
            ((bounds.bottom - bounds.top - 16) as f32 / scale) as i32,
        );
        (
            bounds,
            (size.0 as f32 * scale).round() as i32,
            (size.1 as f32 * scale).round() as i32,
        )
    }
}
fn layout_dashboard(hwnd: HWND, state: &SharedState) {
    unsafe {
        let (bounds, w, h) = dashboard_size(hwnd, state);
        let mut old = RECT::default();
        let _ = GetWindowRect(hwnd, &mut old);
        let x = old
            .left
            .clamp(bounds.left, (bounds.right - w).max(bounds.left));
        let y = (old.bottom - h).clamp(bounds.top, (bounds.bottom - h).max(bounds.top));
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let taskbar = !is_dashboard(hwnd) && state.lock().settings.show_in_taskbar;
        let new_ex = (ex & !(WS_EX_NOACTIVATE.0 | WS_EX_APPWINDOW.0 | WS_EX_TOOLWINDOW.0))
            | WS_EX_LAYERED.0
            | WS_EX_TOPMOST.0
            | if taskbar {
                WS_EX_APPWINDOW.0
            } else {
                WS_EX_TOOLWINDOW.0
            };
        if ex != new_ex {
            SetWindowLongW(hwnd, GWL_EXSTYLE, new_ex as i32);
        }
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        if ex != new_ex
            || old.right - old.left != w
            || old.bottom - old.top != h
            || x != old.left
            || y != old.top
        {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        overlay::apply_rounded_region(hwnd, w, h);
        sync_cookie_edit(hwnd, state);
        let _ = InvalidateRect(hwnd, None, false);
    }
}
extern "system" fn cookie_edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_KEYDOWN && [9, 27].contains(&wp.0) {
            let parent = GetParent(hwnd).unwrap_or_default();
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(parent);
            return SendMessageW(parent, WM_KEYDOWN, wp, lp);
        }
        let old = COOKIE_OLD_PROC.load(std::sync::atomic::Ordering::Relaxed);
        if old == 0 {
            DefWindowProcW(hwnd, msg, wp, lp)
        } else {
            CallWindowProcW(
                Some(std::mem::transmute::<
                    isize,
                    unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                >(old)),
                hwnd,
                msg,
                wp,
                lp,
            )
        }
    }
}
fn repaint_dashboard() {
    let hwnd = HWND(DASHBOARD_WINDOW.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
    unsafe {
        if !hwnd.0.is_null() && IsWindowVisible(hwnd).as_bool() {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const SharedState;
            if !ptr.is_null() {
                layout_dashboard(hwnd, &*ptr);
            }
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}
fn toggle_dashboard(main: HWND, state: &SharedState) {
    unsafe {
        let mut hwnd = HWND(DASHBOARD_WINDOW.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
        if hwnd.0.is_null() || !IsWindow(hwnd).as_bool() {
            let mut class = [0u16; 128];
            GetClassNameW(main, &mut class);
            let instance = HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0);
            let Ok(window) = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TOPMOST,
                PCWSTR(class.as_ptr()),
                w!("Usage Monitor · 用量总览"),
                WS_POPUP,
                0,
                0,
                640,
                590,
                None,
                None,
                instance,
                None,
            ) else {
                return;
            };
            hwnd = window;
            DASHBOARD_WINDOW.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, GetWindowLongPtrW(main, GWLP_USERDATA));
            SetTimer(hwnd, TIMER_ID, 1000, None);
        }
        if IsWindowVisible(hwnd).as_bool() {
            hide_dashboard();
            return;
        }
        // The click that dismissed the popup also lands on the bar: keep it closed.
        let hidden_at = DASHBOARD_HIDDEN_AT.load(std::sync::atomic::Ordering::Relaxed);
        if now_ms().saturating_sub(hidden_at) < DISMISS_REOPEN_GUARD_MS {
            return;
        }
        {
            let mut s = state.lock();
            s.settings_open = false;
            s.ui_focus = 0;
            s.ui_hover = None;
            s.ui_notice.clear();
        }
        place_dashboard(hwnd, main, state);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        sync_cookie_edit(hwnd, state);
        if DASHBOARD_HOOK.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            if let Ok(hook) = SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(dashboard_mouse_proc),
                HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0),
                0,
            ) {
                DASHBOARD_HOOK.store(hook.0 as isize, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}
/// Right-aligned just above the bar, on the bar's monitor.
fn place_dashboard(hwnd: HWND, main: HWND, state: &SharedState) {
    unsafe {
        let (bounds, w, h) = dashboard_size(main, state);
        let mut bar = RECT::default();
        let _ = GetWindowRect(main, &mut bar);
        let x = (bar.right - w).clamp(bounds.left, (bounds.right - w).max(bounds.left));
        let y = (bar.top - h - 10).clamp(bounds.top, (bounds.bottom - h).max(bounds.top));
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        overlay::apply_rounded_region(hwnd, w, h);
        sync_cookie_edit(hwnd, state);
        let _ = InvalidateRect(hwnd, None, false);
    }
}
fn hide_dashboard() {
    unsafe {
        let hook = DASHBOARD_HOOK.swap(0, std::sync::atomic::Ordering::Relaxed);
        if hook != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut _));
        }
        let hwnd = HWND(DASHBOARD_WINDOW.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
        if !hwnd.0.is_null() && IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_HIDE);
            DASHBOARD_HIDDEN_AT.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
        }
    }
}
/// A press outside the popup dismisses it, unless it lands on one of this process's
/// own transient windows (the Edit context menu); the bar itself counts as outside.
extern "system" fn dashboard_mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let press = matches!(wp.0 as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN);
        let dash = HWND(DASHBOARD_WINDOW.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
        if code >= 0
            && press
            && !dash.0.is_null()
            && !MODAL_OPEN.load(std::sync::atomic::Ordering::Relaxed)
        {
            let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
            let mut rect = RECT::default();
            let _ = GetWindowRect(dash, &mut rect);
            let inside = info.pt.x >= rect.left
                && info.pt.x < rect.right
                && info.pt.y >= rect.top
                && info.pt.y < rect.bottom;
            if !inside {
                let target = WindowFromPoint(info.pt);
                let mut pid = 0u32;
                GetWindowThreadProcessId(target, Some(&mut pid));
                let main = HWND(MAIN_WINDOW.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
                let own_transient = pid == std::process::id()
                    && target != main
                    && GetAncestor(target, GA_ROOT) != main;
                if !own_transient {
                    let _ = PostMessageW(dash, WM_DASHBOARD_DISMISS, WPARAM(0), LPARAM(0));
                }
            }
        }
        CallNextHookEx(HHOOK::default(), code, wp, lp)
    }
}
fn sync_cookie_edit(hwnd: HWND, state: &SharedState) {
    unsafe {
        let should_show = {
            let s = state.lock();
            s.settings_open
                && (is_dashboard(hwnd)
                    || (s.settings.theme == Theme::ChoseWay
                        && s.settings.display_mode != DisplayMode::AppBar))
        };
        let mut edit = HWND(COOKIE_EDIT.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
        if !should_show {
            if !edit.0.is_null() && GetParent(edit).unwrap_or_default() == hwnd {
                let _ = ShowWindow(edit, SW_HIDE);
            }
            return;
        }
        if edit.0.is_null()
            || !IsWindow(edit).as_bool()
            || GetParent(edit).unwrap_or_default() != hwnd
        {
            if !edit.0.is_null() {
                let _ = DestroyWindow(edit);
            }
            let Ok(child) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!(""),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | WINDOW_STYLE(ES_PASSWORD as u32 | ES_AUTOHSCROLL as u32),
                0,
                0,
                240,
                30,
                hwnd,
                HMENU(900 as *mut _),
                None,
                None,
            ) else {
                return;
            };
            edit = child;
            COOKIE_EDIT.store(edit.0 as isize, std::sync::atomic::Ordering::Relaxed);
            let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, cookie_edit_proc as *const () as isize);
            COOKIE_OLD_PROC.store(old, std::sync::atomic::Ordering::Relaxed);
            // EM_SETLIMITTEXT; keeps the native password field below the bridge limit.
            let _ = SendMessageW(edit, 0x00C5, WPARAM(32000), LPARAM(0));
            let _ = SendMessageW(
                edit,
                0x1501,
                WPARAM(0),
                LPARAM(w!("粘贴新的 Cursor Cookie").0 as isize),
            );
        }
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let scale = ui_scale(hwnd);
        let field = overlay::popup::cookie_rect(&state.lock(), rect.right, scale);
        let _ = SetWindowPos(
            edit,
            None,
            field.x,
            field.y,
            field.w,
            field.h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let old = COOKIE_FONT.swap(0, std::sync::atomic::Ordering::Relaxed);
        let font = windows::Win32::Graphics::Gdi::CreateFontW(
            -(13.0 * scale).round() as i32,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            0,
            0,
            w!("Segoe UI"),
        );
        let _ = SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        let _ = InvalidateRect(edit, None, true);
        COOKIE_FONT.store(font.0 as isize, std::sync::atomic::Ordering::Relaxed);
        if old != 0 {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(
                windows::Win32::Graphics::Gdi::HFONT(old as *mut _),
            );
        }
    }
}
fn ui_action(host: HWND, command: u32, state: &SharedState) {
    unsafe {
        let main = HWND(MAIN_WINDOW.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
        let target = if main.0.is_null() { host } else { main };
        match command {
            overlay::choseway::SETTINGS => {
                let mut s = state.lock();
                s.settings_open = !s.settings_open;
                s.ui_focus = 0;
                s.ui_notice.clear();
            }
            overlay::choseway::MINI_PROGRESS => {
                let mut s = state.lock();
                s.settings.mini_progress = !s.settings.mini_progress;
                save_settings(&s.settings);
            }
            overlay::choseway::HIDE => {
                if is_dashboard(host) {
                    hide_dashboard();
                } else {
                    let _ = ShowWindow(host, SW_HIDE);
                }
                return;
            }
            IDM_EXIT => {
                MODAL_OPEN.store(true, std::sync::atomic::Ordering::Relaxed);
                let answer = MessageBoxW(
                    host,
                    w!("确定退出 Usage Monitor？\n退出后任务栏和浮窗将不再显示用量。"),
                    w!("Usage Monitor"),
                    MB_OKCANCEL | MB_ICONQUESTION,
                );
                MODAL_OPEN.store(false, std::sync::atomic::Ordering::Relaxed);
                if answer == IDOK {
                    handle_menu(target, IDM_EXIT, state, &[]);
                }
                return;
            }
            overlay::popup::MONITOR_NEXT => {
                let (monitors, next) = {
                    let s = state.lock();
                    let n = s.monitors.len().max(1);
                    (s.monitors.clone(), (s.settings.monitor_idx.min(n - 1) + 1) % n)
                };
                if monitors.len() > 1 {
                    handle_menu(target, IDM_MONITOR_BASE + next as u32, state, &monitors);
                    if is_dashboard(host) {
                        place_dashboard(host, target, state);
                    }
                }
            }
            overlay::choseway::SAVE_COOKIE => {
                let edit = HWND(COOKIE_EDIT.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
                let len = GetWindowTextLengthW(edit);
                if len > 0 && len <= 32000 {
                    let mut value = vec![0u16; len as usize + 1];
                    let n = GetWindowTextW(edit, &mut value).max(0) as usize;
                    let cookie = String::from_utf16_lossy(&value[..n]).trim().to_owned();
                    let notice = if !cookie.contains('=') {
                        "Cookie 格式无效，请检查后重试。"
                    } else if cursor::save_cookie(&cookie).is_ok() {
                        let _ = SetWindowTextW(edit, w!(""));
                        monitor::wake_pollers();
                        "Cookie 已保存。"
                    } else {
                        "Cookie 保存失败，请检查本机目录权限。"
                    };
                    state.lock().ui_notice = notice.into();
                } else {
                    state.lock().ui_notice = "请先粘贴 Cursor Cookie。".into();
                }
            }
            IDM_SHOW_TASKBAR => {
                let (value, mode) = {
                    let mut s = state.lock();
                    s.settings.show_in_taskbar = !s.settings.show_in_taskbar;
                    save_settings(&s.settings);
                    (s.settings.show_in_taskbar, s.settings.display_mode)
                };
                if mode == DisplayMode::Floating {
                    apply_ex_style(target, value);
                }
            }
            _ => {
                state.lock().ui_notice.clear();
                let monitors = state.lock().monitors.clone();
                handle_menu(target, command, state, &monitors);
                if command == IDM_MODE_APPBAR || (command == IDM_MODE_FLOAT && is_dashboard(host)) {
                    state.lock().settings_open = false;
                    if is_dashboard(host) {
                        hide_dashboard();
                    }
                } else if matches!(command, IDM_THEME_CLASSIC | IDM_THEME_NEON | IDM_THEME_CHOSEWAY) {
                    if is_dashboard(host) {
                        place_dashboard(host, target, state);
                    } else if state.lock().settings.theme != Theme::ChoseWay {
                        state.lock().settings_open = false;
                    }
                }
            }
        }
        sync_cookie_edit(host, state);
        let _ = InvalidateRect(host, None, false);
        let _ = InvalidateRect(target, None, false);
        repaint_dashboard();
    }
}

#[cfg(test)]
mod cw_interaction_tests {
    use super::*;
    #[test]
    fn native_settings_hit_testing_password_field_and_escape_work() {
        unsafe {
            extern "system" fn test_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
                if msg == WM_DESTROY {
                    LRESULT(0)
                } else {
                    wnd_proc(hwnd, msg, wp, lp)
                }
            }
            let instance = HINSTANCE(GetModuleHandleW(None).unwrap().0);
            let class = WNDCLASSW {
                lpfnWndProc: Some(test_proc),
                hInstance: instance,
                lpszClassName: w!("UsageMonitorCWInteractionTest"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class.lpszClassName,
                w!("UI test"),
                WS_POPUP,
                -30000,
                -30000,
                640,
                590,
                None,
                None,
                instance,
                None,
            )
            .unwrap();
            let shared = new_shared();
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &shared as *const SharedState as isize);
            let _ = SendMessageW(hwnd, WM_LBUTTONUP, WPARAM(0), LPARAM((550 << 16) | 50));
            assert!(shared.lock().settings_open);
            let edit = HWND(COOKIE_EDIT.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
            assert!(IsWindow(edit).as_bool());
            assert_ne!(
                GetWindowLongW(edit, GWL_STYLE) as u32 & ES_PASSWORD as u32,
                0
            );
            let sx = (-30000 + 50) as u32 & 65535;
            let sy = (-30000 + 550) as u32 & 65535;
            assert_eq!(
                SendMessageW(
                    hwnd,
                    WM_NCHITTEST,
                    WPARAM(0),
                    LPARAM(((sy << 16) | sx) as isize)
                )
                .0,
                HTCLIENT as isize,
                "button must receive client clicks rather than window drag"
            );
            let _ = SendMessageW(edit, WM_KEYDOWN, WPARAM(27), LPARAM(0));
            assert!(!shared.lock().settings_open);
            // Classic / Neon panels reuse the same host: settings open and hit the same rects.
            DASHBOARD_WINDOW.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
            for theme in [Theme::Classic, Theme::Neon] {
                shared.lock().settings.theme = theme;
                overlay::set_theme(theme);
                let _ = SetWindowPos(hwnd, None, 0, 0, 440, 640, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
                shared.lock().settings_open = false;
                // Settings icon in the overview header.
                let _ = SendMessageW(hwnd, WM_LBUTTONUP, WPARAM(0), LPARAM((24 << 16) | (440 - 64)));
                assert!(shared.lock().settings_open, "{:?} settings button", theme);
                let edit = HWND(COOKIE_EDIT.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
                // The offscreen test host is never shown, so check the child's own style.
                assert_ne!(GetWindowLongW(edit, GWL_STYLE) as u32 & WS_VISIBLE.0, 0, "{:?} password field", theme);
                let _ = SendMessageW(hwnd, WM_KEYDOWN, WPARAM(27), LPARAM(0));
                assert!(!shared.lock().settings_open, "Esc returns to the overview");
            }
            DASHBOARD_WINDOW.store(0, std::sync::atomic::Ordering::Relaxed);
            shared.lock().settings.theme = Theme::ChoseWay;
            overlay::set_theme(Theme::ChoseWay);
            let _ = SetWindowPos(hwnd, None, 0, 0, 640, 590, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            SetWindowLongW(hwnd, GWL_EXSTYLE, WS_EX_NOACTIVATE.0 as i32);
            layout_dashboard(hwnd, &shared);
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_eq!(ex & WS_EX_NOACTIVATE.0, 0);
            assert_ne!(ex & WS_EX_LAYERED.0, 0);
            assert_ne!(ex & WS_EX_TOOLWINDOW.0, 0);
            let mut alpha = 0u8;
            GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), None).unwrap();
            assert_eq!(
                alpha, 255,
                "floating mode must restore opacity after leaving AppBar"
            );
            let primary_dpi = WINDOW_DPI.load(std::sync::atomic::Ordering::Relaxed);
            DASHBOARD_WINDOW.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
            let _ = SendMessageW(hwnd, WM_DPICHANGED, WPARAM(192), LPARAM(0));
            assert_eq!(
                WINDOW_DPI.load(std::sync::atomic::Ordering::Relaxed),
                primary_dpi,
                "a second panel must not change taskbar DPI"
            );
            DASHBOARD_WINDOW.store(0, std::sync::atomic::Ordering::Relaxed);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DestroyWindow(hwnd).unwrap();
            COOKIE_EDIT.store(0, std::sync::atomic::Ordering::Relaxed);
            let _ = UnregisterClassW(class.lpszClassName, instance);
        }
    }
}

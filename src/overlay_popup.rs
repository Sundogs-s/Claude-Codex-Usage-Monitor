//! Taskbar popup for every theme. ChoseWay keeps its own dashboard; Classic and Neon
//! draw a 440-wide overview / settings panel in their own palettes. Drawing and hit
//! testing share the same logical rectangles, transformed once by `scale`.
use super::choseway::{self, BoxRect};
use super::*;
use crate::state::RefreshRate;
use usage_monitor::presentation::{self, RowView, ServiceView};

/// Cycle the AppBar / floating window to the next monitor.
pub const MONITOR_NEXT: u32 = 704;
const IDM_FLOAT: u32 = 101;
const IDM_APPBAR: u32 = 103;
const POPUP_W: i32 = 440;
const POPUP_MIN_H: i32 = 640;
const HEAD_H: i32 = 48;
const FOOT_H: i32 = 46;
const ROW_H: i32 = 22;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Icon(&'static str),
    Button,
    Primary,
    Danger,
    Back,
    Toggle(bool),
    Seg(bool),
}

#[derive(Clone, Debug)]
pub struct Control {
    pub rect: BoxRect,
    pub command: u32,
    pub text: String,
    pub kind: Kind,
}

/// Design size the dashboard scale is derived from.
pub fn base_size(theme: Theme) -> (i32, i32) {
    match theme {
        Theme::ChoseWay => (640, 590),
        _ => (POPUP_W, POPUP_MIN_H),
    }
}

pub fn preferred_size(state: &AppState, max_w: i32, max_h: i32) -> (i32, i32) {
    if state.settings.theme == Theme::ChoseWay {
        return choseway::preferred_size(state, max_w, max_h);
    }
    let view = presentation::snapshot(state);
    let content = HEAD_H
        + view
            .services
            .iter()
            .filter(|s| s.enabled)
            .map(section_height)
            .sum::<i32>()
        + FOOT_H
        + 16;
    (
        POPUP_W.min(max_w.max(320)),
        content.max(POPUP_MIN_H).min(max_h.max(300)),
    )
}

/// "显示器 2 · 主屏 · 2560×1440" for the selected monitor.
pub fn monitor_label(state: &AppState) -> String {
    let idx = state
        .settings
        .monitor_idx
        .min(state.monitors.len().saturating_sub(1));
    match state.monitors.get(idx) {
        Some(m) => format!(
            "显示器 {}{} · {}×{}",
            idx + 1,
            if m.primary { " · 主屏" } else { "" },
            m.rect.2 - m.rect.0,
            m.rect.3 - m.rect.1
        ),
        None => "显示器 1".into(),
    }
}

fn r(x: i32, y: i32, w: i32, h: i32) -> BoxRect {
    BoxRect { x, y, w, h }
}
fn sc(v: i32, k: f32) -> i32 {
    (v as f32 * k).round() as i32
}
fn sb(b: BoxRect, k: f32) -> BoxRect {
    r(sc(b.x, k), sc(b.y, k), sc(b.w, k), sc(b.h, k))
}
fn logical(w: i32, h: i32, k: f32) -> (i32, i32) {
    (
        (w as f32 / k).round() as i32,
        (h as f32 / k).round() as i32,
    )
}

// ─── Controls ────────────────────────────────────────────────────────────────

pub fn controls(state: &AppState, w: i32, h: i32, scale: f32) -> Vec<Control> {
    if state.settings.theme == Theme::ChoseWay {
        return choseway::controls(state, w, h, scale)
            .into_iter()
            .map(|c| Control {
                rect: c.rect,
                command: c.command,
                text: c.text,
                kind: match c.toggle {
                    Some(on) => Kind::Toggle(on),
                    None => Kind::Button,
                },
            })
            .collect();
    }
    let (w, h) = logical(w, h, scale);
    let mut list = Vec::new();
    let mut add = |rect: BoxRect, command: u32, text: &str, kind: Kind| {
        list.push(Control {
            rect: sb(rect, scale),
            command,
            text: text.into(),
            kind,
        })
    };
    let s = &state.settings;
    if !state.settings_open {
        add(r(w - 116, 9, 32, 30), 150, "", Kind::Icon("repeat"));
        add(r(w - 80, 9, 32, 30), choseway::SETTINGS, "", Kind::Icon("settings"));
        add(r(w - 44, 9, 32, 30), choseway::HIDE, "", Kind::Icon("close"));
        add(r(w - 124, h - 38, 110, 30), IDM_FLOAT, "切换为浮窗", Kind::Button);
        return list;
    }
    add(r(10, 9, 72, 30), choseway::SETTINGS, "返回", Kind::Back);
    add(r(w - 44, 9, 32, 30), choseway::HIDE, "", Kind::Icon("close"));
    let half = (w - 36 - 16) / 2;
    let (x1, x2) = (18, 18 + half + 16);
    let sx = 86;
    let seg = |n: i32, i: i32| {
        let sw = (w - 18 - sx - (n - 1) * 4) / n;
        (sx + i * (sw + 4), sw)
    };
    // 平台
    let g = HEAD_H;
    for (i, (cmd, text, on)) in [
        (110, "Claude", s.show_claude),
        (111, "Codex", s.show_codex),
        (112, "Cursor", s.show_cursor),
        (choseway::MINI_PROGRESS, "Bar 缩略进度条", s.mini_progress),
    ]
    .into_iter()
    .enumerate()
    {
        let x = if i % 2 == 0 { x1 } else { x2 };
        add(r(x, g + 26 + (i as i32 / 2) * 30, half, 26), cmd, text, Kind::Toggle(on));
    }
    // 显示
    let g = 142;
    for (i, (cmd, text, on)) in [
        (IDM_APPBAR, "任务栏 Bar", s.display_mode == DisplayMode::AppBar),
        (IDM_FLOAT, "浮窗", s.display_mode != DisplayMode::AppBar),
    ]
    .into_iter()
    .enumerate()
    {
        let (x, sw) = seg(2, i as i32);
        add(r(x, g + 26, sw, 26), cmd, text, Kind::Seg(on));
    }
    for (i, (cmd, text, theme)) in [
        (603, "千往", Theme::ChoseWay),
        (602, "Neon", Theme::Neon),
        (601, "Classic", Theme::Classic),
    ]
    .into_iter()
    .enumerate()
    {
        let (x, sw) = seg(3, i as i32);
        add(r(x, g + 58, sw, 26), cmd, text, Kind::Seg(s.theme == theme));
    }
    add(r(sx, g + 90, w - 18 - sx, 26), MONITOR_NEXT, &monitor_label(state), Kind::Button);
    add(r(x1, g + 122, half, 26), 104, "任务栏按钮", Kind::Toggle(s.show_in_taskbar));
    add(r(x2, g + 122, half, 26), 105, "悬停自动隐藏", Kind::Toggle(s.hover_auto_hide));
    // 刷新与启动
    let g = 302;
    for (i, (cmd, text, rate)) in [
        (201, "5 秒", RefreshRate::Secs5),
        (202, "1 分钟", RefreshRate::Min1),
        (203, "5 分钟", RefreshRate::Min5),
    ]
    .into_iter()
    .enumerate()
    {
        let (x, sw) = seg(3, i as i32);
        add(r(x, g + 26, sw, 26), cmd, text, Kind::Seg(s.refresh_rate == rate));
    }
    add(r(x1, g + 58, half, 26), 301, "开机自启", Kind::Toggle(s.auto_start));
    // Cursor 认证
    let g = 398;
    add(r(w - 18 - 52 - 8 - 56, g + 26, 56, 28), choseway::SAVE_COOKIE, "保存", Kind::Primary);
    add(r(w - 18 - 52, g + 26, 52, 28), 114, "清除", Kind::Button);
    // 软件更新
    let g = 464;
    if state.update.ready.is_some() {
        add(r(w - 18 - 96, g + 26, 96, 28), 323, "安装并重启", Kind::Primary);
    } else {
        add(r(w - 18 - 84, g + 26, 84, 28), 320, "检查更新", Kind::Button);
    }
    add(r(x1, g + 58, half, 26), 321, "自动检查并准备", Kind::Toggle(s.auto_update_check));
    add(r(x2, g + 58, half, 26), 322, "接收预览版", Kind::Toggle(s.update_preview));
    add(r(12, h - 38, 176, 30), 400, "退出 Usage Monitor…", Kind::Danger);
    list
}

pub fn hit_test(state: &AppState, w: i32, h: i32, scale: f32, x: i32, y: i32) -> Option<u32> {
    if state.settings.theme == Theme::ChoseWay {
        return choseway::hit_test(state, w, h, scale, x, y);
    }
    controls(state, w, h, scale)
        .iter()
        .find(|c| c.rect.contains(x, y))
        .map(|c| c.command)
}

/// Native password Edit position (client pixels).
pub fn cookie_rect(state: &AppState, w: i32, scale: f32) -> BoxRect {
    if state.settings.theme == Theme::ChoseWay {
        return choseway::cookie_rect(w, scale);
    }
    let lw = (w as f32 / scale).round() as i32;
    sb(r(26, 430, cookie_frame(lw).w - 16, 20), scale)
}
fn cookie_frame(w: i32) -> BoxRect {
    r(18, 424, w - 18 - 52 - 8 - 56 - 8 - 18, 28)
}

// ─── Skin ────────────────────────────────────────────────────────────────────

struct Skin {
    neon: bool,
    bg: u32,
    head: u32,
    line: u32,
    text: u32,
    title: u32,
    dim: u32,
    accent: u32,
    field: u32,
    seg_on: u32,
    hover: u32,
    warn: u32,
    stale: u32,
    radius: i32,
}

fn skin(theme: Theme) -> Skin {
    if theme == Theme::Neon {
        Skin {
            neon: true,
            bg: neon::BG,
            head: 0x080C14,
            line: neon::LINE,
            text: neon::TEXT,
            title: neon::CYAN,
            dim: 0x7F9DAF,
            accent: neon::CYAN,
            field: 0x0A1018,
            seg_on: 0x0E2230,
            hover: 0x0E1A24,
            warn: 0xFF5C85,
            stale: neon::STALE,
            radius: 0,
        }
    } else {
        Skin {
            neon: false,
            bg: BG,
            head: HEADER_BG,
            line: DIVIDER,
            text: TEXT_PRIMARY,
            title: TEXT_TITLE,
            dim: TEXT_DIM,
            accent: CODEX_HI,
            field: 0x0B0D12,
            seg_on: DIVIDER,
            hover: BAR_TRACK,
            warn: WARN_HI,
            stale: STALE,
            radius: 8,
        }
    }
}

/// Background colour behind the native password Edit for the active theme.
pub fn field_color(theme: Theme) -> (u32, u32) {
    match theme {
        Theme::ChoseWay => (0x121212, 0xF4EFE9),
        t => {
            let k = skin(t);
            (k.field, k.text)
        }
    }
}

fn brand(id: &str, neon: bool) -> (u32, u32, u32) {
    match (id, neon) {
        ("claude", false) => (CLAUDE_HI, CLAUDE_MID, CLAUDE_LO),
        ("codex", false) => (CODEX_HI, CODEX_MID, CODEX_LO),
        (_, false) => (CURSOR_HI, CURSOR_MID, CURSOR_LO),
        ("claude", true) => (neon::CLAUDE.1, 0, neon::CLAUDE.0),
        ("codex", true) => (neon::CODEX.1, 0, neon::CODEX.0),
        (_, true) => (neon::CURSOR.1, 0, neon::CURSOR.0),
    }
}

// ─── Primitives (logical rectangles) ─────────────────────────────────────────

unsafe fn fill(dc: HDC, b: BoxRect, color: u32, k: f32) {
    let b = sb(b, k);
    gdi_fill(dc, b.x, b.y, b.w, b.h, color);
}
unsafe fn rbox(dc: HDC, b: BoxRect, radius: i32, color: u32, k: f32) {
    let b = sb(b, k);
    if radius <= 0 {
        gdi_fill(dc, b.x, b.y, b.w, b.h, color);
    } else {
        gdi_rrect(dc, b.x, b.y, b.w, b.h, sc(radius, k), color);
    }
}
/// Outlined box: 1px `line` border around an `inner` fill.
unsafe fn frame(dc: HDC, b: BoxRect, radius: i32, line: u32, inner: u32, k: f32) {
    let o = sb(b, k);
    let t = 1.max(sc(1, k));
    if radius <= 0 {
        gdi_fill(dc, o.x, o.y, o.w, o.h, line);
        gdi_fill(dc, o.x + t, o.y + t, o.w - 2 * t, o.h - 2 * t, inner);
    } else {
        gdi_rrect(dc, o.x, o.y, o.w, o.h, sc(radius, k), line);
        gdi_rrect(dc, o.x + t, o.y + t, o.w - 2 * t, o.h - 2 * t, sc(radius - 1, k), inner);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

/// Text in a logical box. Non-ASCII strings use Segoe UI so GDI font linking
/// supplies CJK glyphs; ASCII uses the requested monospace / display face.
#[allow(clippy::too_many_arguments)]
unsafe fn txt(dc: HDC, s: &str, b: BoxRect, size: i32, color: u32, bold: bool, mono: bool, align: Align, k: f32) {
    let segoe = wstr("Segoe UI");
    let face: &[u16] = if !s.is_ascii() || !mono { &segoe } else { mono_face() };
    let b = sb(b, k);
    let font = make_font_weight(face, -sc(size, k).max(1), if bold { 700 } else { 400 });
    let old = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, rgb(color));
    let flags = DT_SINGLELINE
        | DT_VCENTER
        | DT_END_ELLIPSIS
        | match align {
            Align::Left => DT_LEFT,
            Align::Right => DT_RIGHT,
            Align::Center => DRAW_TEXT_FORMAT(1),
        };
    gdi_text(dc, s, b.x, b.y, b.w, b.h, flags);
    SelectObject(dc, old);
    let _ = DeleteObject(font);
}

fn label_text(label: &str, neon: bool) -> String {
    let label = match label {
        "Extra" => "额外用量",
        v => v,
    };
    if neon {
        label.to_uppercase()
    } else {
        label.into()
    }
}

fn error_text(error: &str) -> &'static str {
    if error.contains("Cookie") {
        "Cookie 缺失或已过期，请在设置中更新。"
    } else if error.contains("Login") {
        "登录不可用，请通过本机 CLI 登录。"
    } else if error.contains("Rate") {
        "平台限流，稍后自动重试。"
    } else {
        "刷新失败，保留最近数据等待重试。"
    }
}

fn section_height(v: &ServiceView) -> i32 {
    let rows = v.rows.len().max(1) as i32;
    12 + 18 + 10
        + if v.error.is_some() { ROW_H } else { 0 }
        + rows * ROW_H
        + if v.activity.is_empty() { 0 } else { 52 }
        + 6
}

fn section_meta(state: &AppState, id: &str) -> String {
    match id {
        "claude" => claude_meta(state).0,
        "codex" => codex_meta(state),
        _ => state
            .cursor
            .reset_date
            .map(|t| format!("重置 {}", fmt_countdown(secs_until(Some(t)))))
            .unwrap_or_default(),
    }
}

// ─── Painting ────────────────────────────────────────────────────────────────

pub unsafe fn paint(hwnd: HWND, state: &AppState, scale: f32) {
    if state.settings.theme == Theme::ChoseWay {
        choseway::paint(hwnd, state, false, scale);
        return;
    }
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let mem = CreateCompatibleDC(dc);
    let bmp = CreateCompatibleBitmap(dc, rect.right.max(1), rect.bottom.max(1));
    let old = SelectObject(mem, bmp);
    draw(mem, state, rect.right, rect.bottom, scale);
    let _ = BitBlt(dc, 0, 0, rect.right, rect.bottom, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

pub unsafe fn draw(dc: HDC, state: &AppState, w: i32, h: i32, scale: f32) {
    let k = skin(state.settings.theme);
    let (lw, lh) = logical(w, h, scale);
    gdi_fill(dc, 0, 0, w, h, k.bg);
    if k.neon {
        let step = sc(8, scale).max(4);
        let mut x = 0;
        while x < w {
            gdi_fill(dc, x, 0, 1, h, neon::GRID);
            x += step;
        }
        let mut y = 0;
        while y < h {
            gdi_fill(dc, 0, y, w, 1, neon::GRID);
            y += step;
        }
    }
    // Header and footer bands.
    fill(dc, r(0, 0, lw, HEAD_H), k.head, scale);
    fill(dc, r(0, HEAD_H - 1, lw, 1), k.line, scale);
    fill(dc, r(0, lh - FOOT_H, lw, FOOT_H), k.head, scale);
    fill(dc, r(0, lh - FOOT_H, lw, 1), k.line, scale);
    if state.settings_open {
        draw_settings(dc, state, &k, lw, lh, scale);
    } else {
        draw_overview(dc, state, &k, lw, lh, scale);
    }
    draw_controls(dc, state, &k, w, h, scale);
    if k.neon {
        let last_x = w - 1;
        let last_y = h - 1;
        neon_corner(dc, 0, 0, 1, 1);
        neon_corner(dc, last_x, 0, -1, 1);
        neon_corner(dc, 0, last_y, 1, -1);
        neon_corner(dc, last_x, last_y, -1, -1);
    } else {
        // 1px outline that follows the rounded window region.
        let o = sb(r(0, 0, lw, lh), scale);
        let rad = sc(14, scale);
        let pen = CreatePen(PS_SOLID, 1, rgb(k.line));
        let old_pen = SelectObject(dc, pen);
        let brush = windows::Win32::Graphics::Gdi::GetStockObject(
            windows::Win32::Graphics::Gdi::NULL_BRUSH,
        );
        let old_brush = SelectObject(dc, brush);
        let _ = RoundRect(dc, o.x, o.y, o.x + o.w - 1, o.y + o.h - 1, rad, rad);
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        let _ = DeleteObject(pen);
    }
}

unsafe fn draw_header(dc: HDC, state: &AppState, k: &Skin, title: &str, x: i32, scale: f32) {
    if k.neon {
        let font = make_font_weight(display_face(), -sc(14, scale), 700);
        let old = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT);
        let b = sb(r(x, 0, 200, HEAD_H), scale);
        glow_text(dc, title, b.x, b.y, b.w, b.h, DT_SINGLELINE | DT_VCENTER | DT_LEFT, k.title, k.head);
        SelectObject(dc, old);
        let _ = DeleteObject(font);
    } else {
        txt(dc, title, r(x, 0, 200, HEAD_H), 15, k.title, true, false, Align::Left, scale);
    }
    if state.settings_open {
        return;
    }
    let (chip, color) = neon_status(state);
    if k.neon {
        let cx = x + 150;
        frame(dc, r(cx, 15, 54, 18), 0, blend(color, k.head, 0.5), k.head, scale);
        fill(dc, r(cx + 6, 21, 6, 6), color, scale);
        txt(dc, chip, r(cx + 14, 15, 38, 18), 10, color, true, true, Align::Center, scale);
    } else {
        let dot = status_dot(state);
        let b = sb(r(x + 128, 21, 7, 7), scale);
        gdi_circle(dc, b.x + b.w / 2, b.y + b.h / 2, b.w / 2, dot);
        let label = match chip {
            "ERR" => "异常",
            "STALE" => "陈旧",
            _ => "实时",
        };
        txt(dc, label, r(x + 140, 0, 60, HEAD_H), 12, k.dim, false, false, Align::Left, scale);
    }
}

unsafe fn draw_overview(dc: HDC, state: &AppState, k: &Skin, w: i32, h: i32, scale: f32) {
    draw_header(
        dc,
        state,
        k,
        if k.neon { "USAGE//MONITOR" } else { "Usage Monitor" },
        18,
        scale,
    );
    let view = presentation::snapshot(state);
    let services: Vec<&ServiceView> = view.services.iter().filter(|s| s.enabled).collect();
    let mut y = HEAD_H;
    for (i, s) in services.iter().enumerate() {
        if i > 0 {
            fill(dc, r(18, y, w - 36, 1), k.line, scale);
            if k.neon {
                fill(dc, r(18, y - 1, 10, 3), neon::CYAN, scale);
            }
        }
        draw_section(dc, state, s, k, w, y, scale);
        y += section_height(s);
    }
    if services.is_empty() {
        txt(dc, "尚未启用平台，请在设置中开启。", r(18, 120, w - 36, 40), 13, k.dim, false, false, Align::Center, scale);
    }
    let updated = view.services.iter().filter_map(|s| s.updated).max();
    let note = if !state.ui_notice.is_empty() {
        state.ui_notice.clone()
    } else {
        let time = updated
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string());
        match (time, k.neon) {
            (Some(t), true) => format!("UPD {} · v{}", t, env!("CARGO_PKG_VERSION")),
            (Some(t), false) => format!("更新于 {} · v{}", t, env!("CARGO_PKG_VERSION")),
            (None, _) => format!("v{}", env!("CARGO_PKG_VERSION")),
        }
    };
    txt(dc, &note, r(18, h - FOOT_H, w - 160, FOOT_H), 11, k.dim, false, true, Align::Left, scale);
}

unsafe fn draw_section(dc: HDC, state: &AppState, v: &ServiceView, k: &Skin, w: i32, top: i32, scale: f32) {
    let (hi, mid, lo) = brand(v.id, k.neon);
    let mut y = top + 12;
    if k.neon {
        let b = sb(r(10, y + 1, 4, 16), scale);
        neon_vgrad(dc, b.x, b.y, b.w, b.h, hi, lo);
        txt(dc, &v.title.to_uppercase(), r(22, y, 160, 18), 14, hi, true, true, Align::Left, scale);
    } else {
        let b = sb(r(22, y + 9, 0, 0), scale);
        draw_diamond(dc, b.x, b.y, sc(5, scale), hi);
        txt(dc, v.title, r(34, y, 160, 18), 15, blend(hi, 0xFFFFFF, 0.3), true, false, Align::Left, scale);
    }
    let meta = section_meta(state, v.id);
    let meta = if k.neon { meta.to_uppercase() } else { meta };
    txt(dc, &meta, r(w - 238, y, 220, 18), 11, if v.stale { k.stale } else { k.dim }, false, true, Align::Right, scale);
    y += 28;
    if let Some(error) = v.error {
        txt(dc, error_text(error), r(22, y, w - 40, 16), 11, k.warn, false, false, Align::Left, scale);
        y += ROW_H;
    }
    let (lx, ew, pw) = (if k.neon { 22 } else { 18 }, 56, 46);
    let bx = lx + 54;
    let bw = w - 18 - ew - 8 - pw - 8 - bx;
    let quota: Vec<&RowView> = v.rows.iter().filter(|r| r.value.is_none()).collect();
    let values: Vec<&RowView> = v.rows.iter().filter(|r| r.value.is_some()).collect();
    for row in &quota {
        let util = row.utilization.filter(|u| u.is_finite());
        let warn = util.unwrap_or(0.0) >= 0.8;
        txt(dc, &label_text(&row.label, k.neon), r(lx, y, 52, 16), 12, if k.neon { neon::LABEL } else { k.text }, false, true, Align::Left, scale);
        let bar = r(bx, y + 3, bw, 10);
        if k.neon {
            let b = sb(bar, scale);
            let gap = sc(3, scale).max(2);
            let seg_w = ((b.w - 15 * gap) / 16).max(2);
            let (blo, bhi) = if warn { (0xB0174A, neon::WARN) } else { (lo, hi) };
            neon_seg_bar(dc, b.x, b.y, b.w, b.h, seg_w, gap, util, blo, bhi, k.bg, v.stale);
        } else {
            let track = r(bx, y + 4, bw, 8);
            rbox(dc, track, 4, BAR_TRACK, scale);
            if let Some(u) = util {
                let t = sb(track, scale);
                let fw = (t.w as f32 * u.clamp(0.0, 1.0)).round() as i32;
                if fw > 0 {
                    let (a, b, c) = row_colors(Some(u), hi, mid, lo, v.stale);
                    gdi_grad_bar(dc, t.x, t.y, fw, t.h, c, b, a);
                }
            }
        }
        if util.is_none() {
            let mut x = 0;
            while x < bw {
                fill(dc, r(bx + x, y + 8, 3.min(bw - x), 1), k.dim, scale);
                x += 6;
            }
        }
        let pct_color = if v.stale {
            k.dim
        } else if warn {
            k.warn
        } else {
            if k.neon { neon::PCT } else { k.text }
        };
        txt(
            dc,
            &if util.is_some() { fmt_pct(util) } else { "—".into() },
            r(w - 18 - ew - 8 - pw, y, pw, 16),
            13,
            pct_color,
            true,
            true,
            Align::Right,
            scale,
        );
        let eta = row
            .reset
            .map(|t| fmt_countdown((t - chrono::Utc::now().timestamp()).max(0)))
            .unwrap_or_default();
        txt(dc, &eta, r(w - 18 - ew, y, ew, 16), 11, k.dim, false, true, Align::Right, scale);
        y += ROW_H;
    }
    if !v.activity.is_empty() {
        y += 2;
        let n = v.activity.len() as i32;
        let cw = (w - lx - 18 - (n - 1) * 8) / n;
        for (i, a) in v.activity.iter().enumerate() {
            let cx = lx + i as i32 * (cw + 8);
            frame(dc, r(cx, y, cw, 44), k.radius, k.line, if k.neon { k.field } else { k.head }, scale);
            let label = if k.neon {
                match a.label.as_str() {
                    "今日" => "TODAY".to_string(),
                    "昨日" => "YESTERDAY".to_string(),
                    l if l.contains("已返回") => "7 DAYS*".to_string(),
                    _ => "7 DAYS".to_string(),
                }
            } else {
                a.label.clone()
            };
            txt(dc, &label, r(cx + 9, y + 4, cw - 18, 15), 10, k.dim, false, true, Align::Left, scale);
            txt(dc, a.value.as_deref().unwrap_or("—"), r(cx + 9, y + 19, cw - 18, 22), 16, if k.neon { neon::PCT } else { k.text }, true, true, Align::Left, scale);
        }
        y += 50;
    }
    for row in values {
        txt(dc, &label_text(&row.label, k.neon), r(lx, y, 120, 16), 12, if k.neon { neon::LABEL } else { k.text }, false, true, Align::Left, scale);
        txt(dc, row.value.as_deref().unwrap_or("—"), r(w - 18 - 140, y, 140, 16), 13, if k.neon { neon::PCT } else { k.text }, true, true, Align::Right, scale);
        y += ROW_H;
    }
}

unsafe fn draw_settings(dc: HDC, state: &AppState, k: &Skin, w: i32, h: i32, scale: f32) {
    draw_header(dc, state, k, if k.neon { "CONFIG//设置" } else { "设置" }, 92, scale);
    for (y, cap_classic, cap_neon) in [
        (HEAD_H, "平台", "PLATFORMS // 平台"),
        (142, "显示", "DISPLAY // 显示"),
        (302, "刷新与启动", "REFRESH // 刷新与启动"),
        (398, "Cursor 认证", "AUTH // CURSOR 认证"),
        (464, "软件更新", "UPDATE // 软件更新"),
    ] {
        if y != HEAD_H {
            fill(dc, r(0, y, w, 1), k.line, scale);
        }
        txt(
            dc,
            if k.neon { cap_neon } else { cap_classic },
            r(18, y + 8, w - 36, 14),
            if k.neon { 10 } else { 11 },
            if k.neon { neon::CYAN } else { k.dim },
            true,
            false,
            Align::Left,
            scale,
        );
    }
    for (y, label) in [(168, "模式"), (200, "主题"), (232, "显示器"), (328, "刷新间隔")] {
        txt(dc, label, r(18, y, 66, 26), 12, if k.neon { neon::LABEL } else { k.title }, false, false, Align::Left, scale);
    }
    let field = cookie_frame(w);
    frame(dc, field, k.radius, k.line, k.field, scale);
    let status = if !state.update.message.is_empty() {
        format!("v{} · {}", env!("CARGO_PKG_VERSION"), state.update.message)
    } else {
        format!("v{}", env!("CARGO_PKG_VERSION"))
    };
    txt(dc, &status, r(18, 490, w - 140, 28), 11, k.dim, false, true, Align::Left, scale);
    let note = if state.ui_notice.is_empty() {
        if k.neon { "AUTO-SAVED" } else { "更改即时保存" }.to_string()
    } else {
        state.ui_notice.clone()
    };
    txt(dc, &note, r(200, h - FOOT_H, w - 218, FOOT_H), 11, k.dim, false, true, Align::Right, scale);
}

unsafe fn draw_controls(dc: HDC, state: &AppState, k: &Skin, w: i32, h: i32, scale: f32) {
    for (i, c) in controls(state, w, h, scale).iter().enumerate() {
        let b = r(
            (c.rect.x as f32 / scale).round() as i32,
            (c.rect.y as f32 / scale).round() as i32,
            (c.rect.w as f32 / scale).round() as i32,
            (c.rect.h as f32 / scale).round() as i32,
        );
        let hover = state.ui_hover == Some(c.command);
        let focus = state.ui_focus == i + 1;
        let ring = if focus { k.accent } else { k.line };
        match c.kind {
            Kind::Icon(name) => {
                let bg = if hover || focus { k.hover } else { k.head };
                if k.neon {
                    frame(dc, b, 0, if hover || focus { neon::CYAN } else { k.line }, bg, scale);
                } else if hover || focus {
                    frame(dc, b, k.radius, ring, bg, scale);
                }
                choseway::icon(dc, name, r(b.x + (b.w - 16) / 2, b.y + (b.h - 16) / 2, 16, 16), if k.neon { neon::CYAN } else { k.title }, bg, scale);
            }
            Kind::Back => {
                let bg = if hover || focus { k.hover } else { k.head };
                frame(dc, b, k.radius, if k.neon || hover || focus { ring } else { bg }, bg, scale);
                txt(dc, "‹ 返回", b, 12, if k.neon { neon::CYAN } else { k.title }, k.neon, false, Align::Center, scale);
            }
            Kind::Button | Kind::Primary | Kind::Danger => {
                let (line, bg, color) = match c.kind {
                    Kind::Primary if k.neon => (neon::CYAN, neon::CYAN, k.bg),
                    Kind::Primary => (CODEX_HI, CODEX_MID, 0xFFFFFF),
                    Kind::Danger if k.neon => (0x5A1630, 0x14070C, k.warn),
                    Kind::Danger => (if hover || focus { k.line } else { k.head }, k.head, k.warn),
                    _ if k.neon => (if hover { neon::CYAN } else { neon::CYAN_DIM }, k.field, neon::CYAN),
                    _ => (k.line, if hover { k.hover } else { k.field }, k.title),
                };
                frame(dc, b, k.radius, if focus { k.accent } else { line }, bg, scale);
                let label = if k.neon && c.kind != Kind::Danger && c.command != MONITOR_NEXT {
                    c.text.to_uppercase()
                } else {
                    c.text.clone()
                };
                txt(dc, &label, r(b.x + 8, b.y, b.w - 16, b.h), 12, color, k.neon, true, if c.command == MONITOR_NEXT { Align::Left } else { Align::Center }, scale);
                if c.command == MONITOR_NEXT && state.monitors.len() > 1 {
                    txt(dc, "切换 ›", r(b.x + b.w - 60, b.y, 52, b.h), 11, k.dim, false, false, Align::Right, scale);
                }
            }
            Kind::Seg(on) => {
                let (line, bg, color) = if on {
                    (if k.neon { neon::CYAN } else { k.seg_on }, k.seg_on, if k.neon { neon::PCT } else { 0xFFFFFF })
                } else {
                    (k.line, if hover { k.hover } else { k.field }, k.dim)
                };
                frame(dc, b, if k.neon { 0 } else { 6 }, if focus { k.accent } else { line }, bg, scale);
                txt(dc, &if k.neon { c.text.to_uppercase() } else { c.text.clone() }, b, 12, color, on, true, Align::Center, scale);
            }
            Kind::Toggle(on) => {
                if hover || focus {
                    frame(dc, r(b.x - 6, b.y, b.w + 6, b.h), k.radius, if focus { k.accent } else { k.hover }, k.hover, scale);
                }
                let label = if k.neon { c.text.to_uppercase() } else { c.text.clone() };
                let mut tx = b.x;
                let brand_id = match c.command {
                    110 => Some("claude"),
                    111 => Some("codex"),
                    112 => Some("cursor"),
                    _ => None,
                };
                if let Some(id) = brand_id {
                    let (hi, _, lo) = brand(id, k.neon);
                    if k.neon {
                        fill(dc, r(b.x, b.y + 7, 4, 12), lo, scale);
                    } else {
                        rbox(dc, r(b.x, b.y + 9, 8, 8), 2, hi, scale);
                    }
                    tx += 14;
                }
                txt(dc, &label, r(tx, b.y, b.w - (tx - b.x) - 46, b.h), 12, k.text, false, true, Align::Left, scale);
                if k.neon {
                    let key = r(b.x + b.w - 40, b.y + 4, 40, 18);
                    if on {
                        frame(dc, key, 0, neon::CYAN, neon::CYAN, scale);
                        txt(dc, "ON", key, 10, k.bg, true, true, Align::Center, scale);
                    } else {
                        frame(dc, key, 0, k.line, k.field, scale);
                        txt(dc, "OFF", key, 10, k.dim, true, true, Align::Center, scale);
                    }
                } else {
                    let sw = r(b.x + b.w - 30, b.y + 5, 30, 16);
                    rbox(dc, sw, 8, if on { k.accent } else { k.line }, scale);
                    rbox(dc, r(sw.x + if on { 16 } else { 2 }, sw.y + 2, 12, 12), 6, if on { 0xFFFFFF } else { k.dim }, scale);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{CreateDIBSection, GdiFlush, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};

    fn sample() -> AppState {
        let mut state = AppState::default();
        state.claude.rows = ["5h", "7d"]
            .iter()
            .zip([0.6, 0.45])
            .map(|(l, u)| crate::state::UsageRow { label: (*l).into(), utilization: Some(u), reset: None })
            .collect();
        state.claude.tier = Some("Pro".into());
        state.claude.fetched_at = Some(chrono::Utc::now());
        state.codex.plan = Some("plus".into());
        state.codex.windows = vec![
            crate::state::CodexWindow { label: "5h".into(), minutes: 300, utilization: 0.32, reset: None },
            crate::state::CodexWindow { label: "7d".into(), minutes: 10080, utilization: 0.84, reset: None },
        ];
        state.cursor.auto_usage_pct = Some(0.18);
        state.cursor.api_usage_pct = None;
        state.monitors = vec![crate::state::MonitorInfo { rect: (0, 0, 2560, 1440), work_rect: (0, 0, 2560, 1392), primary: true }];
        state
    }

    #[test]
    fn classic_and_neon_popups_render_and_share_hit_rectangles() {
        let mut state = sample();
        for theme in [Theme::Classic, Theme::Neon] {
            state.settings.theme = theme;
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                for open in [false, true] {
                    state.settings_open = open;
                    let (lw, lh) = preferred_size(&state, 2000, 2000);
                    assert_eq!((lw, lh), (POPUP_W, POPUP_MIN_H));
                    let (width, height) = ((lw as f32 * scale) as i32, (lh as f32 * scale) as i32);
                    let list = controls(&state, width, height, scale);
                    assert!(!list.is_empty());
                    for c in &list {
                        assert!(c.rect.x >= 0 && c.rect.y >= 0 && c.rect.x + c.rect.w <= width && c.rect.y + c.rect.h <= height, "{:?} outside", c);
                        assert_eq!(hit_test(&state, width, height, scale, c.rect.x + c.rect.w / 2, c.rect.y + c.rect.h / 2), Some(c.command));
                    }
                    if open {
                        let field = cookie_rect(&state, width, scale);
                        assert!(list.iter().all(|c| !(c.rect.contains(field.x + 2, field.y + 2))), "Edit must not cover a button");
                        assert!(list.iter().any(|c| c.command == 201 && c.kind == Kind::Seg(false)));
                        assert!(list.iter().any(|c| c.command == MONITOR_NEXT && c.text.contains("主屏")));
                    }
                    unsafe {
                        let dc = CreateCompatibleDC(None);
                        let mut bits = std::ptr::null_mut();
                        let info = BITMAPINFO {
                            bmiHeader: BITMAPINFOHEADER {
                                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                                biWidth: width,
                                biHeight: -height,
                                biPlanes: 1,
                                biBitCount: 32,
                                biCompression: BI_RGB.0,
                                ..Default::default()
                            },
                            ..Default::default()
                        };
                        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
                        let old = SelectObject(dc, bitmap);
                        draw(dc, &state, width, height, scale);
                        let _ = GdiFlush();
                        let pixels = std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize);
                        assert!(pixels.chunks_exact(4).filter(|p| p[0] > 180 && p[1] > 180 && p[2] > 180).count() > 200);
                        if let Some(dir) = std::env::var_os("USAGE_MONITOR_PREVIEW_DIR") {
                            let mut bmp = vec![0u8; 54];
                            bmp[..2].copy_from_slice(b"BM");
                            bmp[2..6].copy_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
                            bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
                            bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
                            bmp[18..22].copy_from_slice(&width.to_le_bytes());
                            bmp[22..26].copy_from_slice(&(-height).to_le_bytes());
                            bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
                            bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
                            bmp.extend_from_slice(pixels);
                            let dir = std::path::PathBuf::from(dir);
                            std::fs::create_dir_all(&dir).unwrap();
                            std::fs::write(dir.join(format!("popup-{:?}-{}-{}.bmp", theme, if open { "settings" } else { "overview" }, (scale * 100.0) as i32)), bmp).unwrap();
                        }
                        SelectObject(dc, old);
                        let _ = DeleteObject(bitmap);
                        let _ = DeleteDC(dc);
                    }
                }
            }
        }
    }

    #[test]
    fn choseway_bar_draws_both_claude_windows() {
        let mut state = sample();
        state.settings.theme = Theme::ChoseWay;
        state.settings.show_cursor = false;
        for (scale, band) in [(1.0f32, 48), (1.25, 60), (2.0, 96)] {
            let width = (2 * crate::state::CHOSEWAY_APPBAR_COL_W) as f32 * scale;
            let (width, height) = (width as i32, band);
            unsafe {
                let dc = CreateCompatibleDC(None);
                let mut bits = std::ptr::null_mut();
                let info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: width,
                        biHeight: -height,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
                let old = SelectObject(dc, bitmap);
                choseway::draw_bar(dc, &state, width, height, scale);
                let _ = GdiFlush();
                let pixels = std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize);
                // Two progress rows per column: bright fill appears in both halves of the band.
                let bright_rows: Vec<bool> = (0..height)
                    .map(|y| (0..width).any(|x| {
                        let p = &pixels[((y * width + x) * 4) as usize..][..3];
                        p.iter().all(|c| *c > 200)
                    }))
                    .collect();
                assert!(bright_rows[..height as usize / 2].iter().any(|b| *b));
                assert!(bright_rows[height as usize / 2..].iter().any(|b| *b));
                if let Some(dir) = std::env::var_os("USAGE_MONITOR_PREVIEW_DIR") {
                    let mut bmp = vec![0u8; 54];
                    bmp[..2].copy_from_slice(b"BM");
                    bmp[2..6].copy_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
                    bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
                    bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
                    bmp[18..22].copy_from_slice(&width.to_le_bytes());
                    bmp[22..26].copy_from_slice(&(-height).to_le_bytes());
                    bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
                    bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
                    bmp.extend_from_slice(pixels);
                    let dir = std::path::PathBuf::from(dir);
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join(format!("choseway-bar-{}.bmp", (scale * 100.0) as i32)), bmp).unwrap();
                }
                SelectObject(dc, old);
                let _ = DeleteObject(bitmap);
                let _ = DeleteDC(dc);
            }
        }
    }

    #[test]
    fn bar_rows_show_claude_windows_and_skip_value_rows() {
        let mut state = sample();
        state.claude.rows.push(crate::state::UsageRow { label: "Opus".into(), utilization: Some(0.9), reset: None });
        state.codex.plan = Some("pro".into());
        state.codex.windows.remove(0);
        state.codex.credits = Some("100.00".into());
        let view = presentation::snapshot(&state);
        let claude: Vec<_> = choseway::bar_rows(&view.services[0]).iter().map(|r| r.label.clone()).collect();
        assert_eq!(claude, ["5h", "7d"]);
        let codex: Vec<_> = choseway::bar_rows(&view.services[1]).iter().map(|r| r.label.clone()).collect();
        assert_eq!(codex, ["7d"], "Pro keeps a single 7d row and never shows Credits");
        let cursor = choseway::bar_rows(&view.services[2]);
        assert_eq!(cursor.len(), 2);
        assert!(cursor[1].utilization.is_none(), "missing quota stays unknown, not 0");
    }
}

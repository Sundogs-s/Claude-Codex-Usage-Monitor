//! ChoseWay native GDI dashboard. Drawing and hit testing share the same rectangles.
use super::*;
use usage_monitor::presentation::{self, RowView, ServiceView};
use windows::Win32::Graphics::Gdi::{
    SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HALFTONE,
};

pub const SETTINGS: u32 = 700;
pub const MINI_PROGRESS: u32 = 701;
pub const SAVE_COOKIE: u32 = 702;
pub const HIDE: u32 = 703;
const BG_CW: u32 = 0x070707;
const PANEL: u32 = 0x121212;
const CARD: u32 = 0x1B1B1B;
const LINE: u32 = 0x303030;
const TEXT: u32 = 0xF4EFE9;
const MUTED: u32 = 0xB9AEA4;
const FAINT: u32 = 0x898582;
const RED: u32 = 0xD4121F;
const WARN: u32 = 0xD9A13B;

#[derive(Clone, Copy, Debug)]
pub struct BoxRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}
impl BoxRect {
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}
#[derive(Clone, Debug)]
pub struct Control {
    pub rect: BoxRect,
    pub command: u32,
    pub text: String,
    pub icon: &'static str,
    pub toggle: Option<bool>,
}

fn used_color(used: Option<f32>) -> u32 {
    match used {
        Some(u) if u >= 1.0 => 0xFF3748,
        Some(u) if u >= 0.8 => WARN,
        _ => TEXT,
    }
}
fn measure(service: &ServiceView, columns: i32) -> i32 {
    let rows = service
        .rows
        .iter()
        .map(|r| if r.value.is_some() { 30 } else { 46 })
        .sum::<i32>();
    58 + (rows + columns - 1) / columns
        + if columns > 1 { 46 } else { 0 }
        + if service.activity.is_empty() { 0 } else { 130 }
        + if service.error.is_some() { 40 } else { 0 }
}
/// Logical pixels, then a single DPI transform for geometry, paint and input.
pub fn preferred_size(state: &AppState, max_w: i32, max_h: i32) -> (i32, i32) {
    let view = presentation::snapshot(state);
    let top: Vec<_> = view
        .services
        .iter()
        .filter(|s| s.enabled && s.id != "cursor")
        .collect();
    let cursor = view.services.iter().find(|s| s.enabled && s.id == "cursor");
    let overhead = 142 + cursor.map_or(0, |s| 118 + if s.error.is_some() { 40 } else { 0 });
    let max_h = max_h.max(300);
    let mut columns = 1;
    while top.iter().map(|s| measure(s, columns)).max().unwrap_or(120) + overhead > max_h
        && columns < 8
    {
        columns += 1;
    }
    let height = (top.iter().map(|s| measure(s, columns)).max().unwrap_or(120) + overhead)
        .max(590)
        .min(max_h);
    ((640.max(columns * 560)).min(max_w).max(320), height)
}
fn columns(service: &ServiceView, height: i32) -> i32 {
    (1..=16)
        .find(|n| measure(service, *n) <= height)
        .unwrap_or(16)
}
fn logical(w: i32, h: i32, scale: f32) -> (i32, i32) {
    (
        (w as f32 / scale).round() as i32,
        (h as f32 / scale).round() as i32,
    )
}
fn box_scaled(r: BoxRect, scale: f32) -> BoxRect {
    BoxRect {
        x: (r.x as f32 * scale).round() as i32,
        y: (r.y as f32 * scale).round() as i32,
        w: (r.w as f32 * scale).round() as i32,
        h: (r.h as f32 * scale).round() as i32,
    }
}
fn r(x: i32, y: i32, w: i32, h: i32) -> BoxRect {
    BoxRect { x, y, w, h }
}

pub fn controls(state: &AppState, w: i32, h: i32, scale: f32) -> Vec<Control> {
    let (w, h) = logical(w, h, scale);
    let mut list = Vec::new();
    let mut add = |rect, command, text: &str, icon, toggle| {
        list.push(Control {
            rect: box_scaled(rect, scale),
            command,
            text: text.into(),
            icon,
            toggle,
        })
    };
    add(
        r(20, h - 50, 96, 30),
        SETTINGS,
        if state.settings_open {
            "用量总览"
        } else {
            "设置"
        },
        "settings",
        None,
    );
    add(r(w - 54, 22, 30, 30), HIDE, "", "close", None);
    if state.settings_open {
        add(r(128, h - 50, 102, 30), 320, "检查更新", "repeat", None);
        if state.update.ready.is_some() {
            add(r(242, h - 50, 112, 30), 323, "安装并重启", "", None);
        }
        let cw = (w - 52) / 2;
        let x1 = 34;
        let x2 = 46 + cw;
        for (i, (cmd, text, icon, on)) in [
            (110, "Claude", "claude", state.settings.show_claude),
            (111, "Codex", "codex", state.settings.show_codex),
            (112, "Cursor", "cursor", state.settings.show_cursor),
            (
                MINI_PROGRESS,
                "缩略进度条",
                "",
                state.settings.mini_progress,
            ),
        ]
        .iter()
        .enumerate()
        {
            add(
                r(x1, 122 + i as i32 * 34, cw - 28, 30),
                *cmd,
                text,
                icon,
                Some(*on),
            );
        }
        for (i, (cmd, label, rate)) in [
            (201, "5 秒", crate::state::RefreshRate::Secs5),
            (202, "1 分钟", crate::state::RefreshRate::Min1),
            (203, "5 分钟", crate::state::RefreshRate::Min5),
        ]
        .into_iter()
        .enumerate()
        {
            add(
                r(x2 + i as i32 * 88, 145, 80, 30),
                cmd,
                label,
                "",
                Some(state.settings.refresh_rate == rate),
            );
        }
        add(
            r(x1, 262, cw - 28, 30),
            super::popup::MONITOR_NEXT,
            &super::popup::monitor_label(state),
            "desktop",
            None,
        );
        add(
            r(x2, 190, cw - 28, 30),
            301,
            "登录时启动",
            "",
            Some(state.settings.auto_start),
        );
        add(
            r(x2, 226, cw - 28, 30),
            104,
            "显示任务栏按钮",
            "",
            Some(state.settings.show_in_taskbar),
        );
        add(r(x1, 416, 94, 30), SAVE_COOKIE, "保存 Cookie", "", None);
        add(r(x1 + 104, 416, 64, 30), 114, "清除", "", None);
        add(
            r(x2, 354, cw - 28, 30),
            321,
            "自动检查更新",
            "",
            Some(state.settings.auto_update_check),
        );
        add(
            r(x2, 388, cw - 28, 30),
            322,
            "接收预览版",
            "",
            Some(state.settings.update_preview),
        );
        add(
            r(x2, 430, 184, 30),
            103,
            "切换至任务栏缩略",
            "external-link",
            None,
        );
    } else {
        add(r(w - 150, 22, 84, 30), 150, "刷新", "repeat", None);
        add(
            r(128, h - 50, 126, 30),
            103,
            "任务栏缩略",
            "external-link",
            None,
        );
        add(r(266, h - 50, 70, 30), 400, "退出", "log-out", None);
    }
    list
}
pub fn cookie_rect(w: i32, scale: f32) -> BoxRect {
    let width = (w as f32 / scale).round() as i32;
    box_scaled(r(36, 376, (width - 52) / 2 - 32, 30), scale)
}
pub fn hit_test(state: &AppState, w: i32, h: i32, scale: f32, x: i32, y: i32) -> Option<u32> {
    controls(state, w, h, scale)
        .iter()
        .find(|b| b.rect.contains(x, y))
        .map(|b| b.command)
}

unsafe fn text(
    dc: HDC,
    s: &str,
    rect: BoxRect,
    size: i32,
    color: u32,
    bold: bool,
    right: bool,
    scale: f32,
) {
    let rect = box_scaled(rect, scale);
    let font = make_font_weight(
        &wstr("Segoe UI"),
        -((size as f32 * scale).round() as i32),
        if bold { 700 } else { 400 },
    );
    let old = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, rgb(color));
    gdi_text(
        dc,
        s,
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | if right { DT_RIGHT } else { DT_LEFT },
    );
    SelectObject(dc, old);
    let _ = DeleteObject(font);
}
unsafe fn box_fill(dc: HDC, rect: BoxRect, corner: i32, color: u32, scale: f32) {
    let rect = box_scaled(rect, scale);
    gdi_rrect(
        dc,
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        (corner as f32 * scale).round() as i32,
        color,
    );
}
unsafe fn card(dc: HDC, rect: BoxRect, scale: f32) {
    box_fill(dc, rect, 18, LINE, scale);
    box_fill(
        dc,
        r(rect.x + 1, rect.y + 1, rect.w - 2, rect.h - 2),
        17,
        CARD,
        scale,
    );
}
pub(super) unsafe fn icon(dc: HDC, name: &str, rect: BoxRect, color: u32, bg: u32, scale: f32) {
    let mask: &[u8] = match name {
        "claude" => include_bytes!("../assets/ui/claude.mask"),
        "codex" => include_bytes!("../assets/ui/codex.mask"),
        "cursor" => include_bytes!("../assets/ui/cursor.mask"),
        "settings" => include_bytes!("../assets/ui/settings.mask"),
        "repeat" => include_bytes!("../assets/ui/repeat.mask"),
        "close" => include_bytes!("../assets/ui/close.mask"),
        "external-link" => include_bytes!("../assets/ui/external-link.mask"),
        "log-out" => include_bytes!("../assets/ui/log-out.mask"),
        _ => include_bytes!("../assets/ui/desktop.mask"),
    };
    let mut pixels = vec![0u8; 96 * 96 * 4];
    for (pixel, alpha) in pixels.chunks_exact_mut(4).zip(mask.iter()) {
        let c = blend(bg, color, *alpha as f32 / 255.0);
        pixel[0] = (c & 255) as u8;
        pixel[1] = ((c >> 8) & 255) as u8;
        pixel[2] = (c >> 16) as u8;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: 96,
            biHeight: -96,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let rect = box_scaled(rect, scale);
    let old = SetStretchBltMode(dc, HALFTONE);
    StretchDIBits(
        dc,
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        0,
        0,
        96,
        96,
        Some(pixels.as_ptr() as *const _),
        &info,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
    SetStretchBltMode(dc, windows::Win32::Graphics::Gdi::STRETCH_BLT_MODE(old));
}
unsafe fn metric(dc: HDC, row: &RowView, rect: BoxRect, stale: bool, scale: f32) {
    let label = match row.label.as_str() {
        "5h" => "5 小时",
        "7d" => "7 天",
        "Extra" => "额外用量",
        v => v,
    };
    let color = if stale {
        MUTED
    } else {
        used_color(row.utilization)
    };
    text(
        dc,
        label,
        r(rect.x, rect.y, rect.w - 80, 20),
        11,
        MUTED,
        false,
        false,
        scale,
    );
    let value = row
        .value
        .clone()
        .unwrap_or_else(|| fmt_pct(row.utilization));
    text(
        dc,
        &value,
        r(rect.x + rect.w - 100, rect.y, 100, 20),
        13,
        color,
        true,
        true,
        scale,
    );
    if row.value.is_some() {
        return;
    }
    box_fill(dc, r(rect.x, rect.y + 22, rect.w, 5), 3, LINE, scale);
    if let Some(u) = row.utilization.filter(|u| u.is_finite()) {
        let width = (rect.w as f32 * u.clamp(0.0, 1.0)).round() as i32;
        if width > 0 {
            box_fill(
                dc,
                r(rect.x, rect.y + 22, width, 5),
                3,
                if stale { FAINT } else { used_color(Some(u)) },
                scale,
            );
        }
    } else {
        for x in (0..rect.w).step_by(6) {
            box_fill(
                dc,
                r(rect.x + x, rect.y + 23, 3.min(rect.w - x), 1),
                0,
                FAINT,
                scale,
            );
        }
    }
    text(
        dc,
        if row.utilization.is_some() {
            "已使用"
        } else {
            "暂无额度数据"
        },
        r(rect.x, rect.y + 28, 100, 15),
        10,
        FAINT,
        false,
        false,
        scale,
    );
    let reset = row
        .reset
        .map(|t| {
            format!(
                "{} 后重置",
                fmt_countdown((t - chrono::Utc::now().timestamp()).max(0))
            )
        })
        .unwrap_or_default();
    text(
        dc,
        &reset,
        r(rect.x + 78, rect.y + 28, rect.w - 78, 15),
        10,
        FAINT,
        false,
        true,
        scale,
    );
}
unsafe fn service(dc: HDC, view: &ServiceView, rect: BoxRect, cursor: bool, scale: f32) {
    card(dc, rect, scale);
    let x = rect.x + 14;
    let mut y = rect.y + 12;
    let width = rect.w - 28;
    icon(dc, view.id, r(x, y + 2, 24, 24), TEXT, CARD, scale);
    text(
        dc,
        &view.title,
        r(x + 34, y, width - 120, 21),
        14,
        TEXT,
        true,
        false,
        scale,
    );
    text(
        dc,
        &view.meta,
        r(x + 34, y + 22, width - 120, 14),
        10,
        MUTED,
        false,
        false,
        scale,
    );
    let known = view.rows.iter().any(|r| r.utilization.is_some());
    text(
        dc,
        if view.stale {
            "数据陈旧"
        } else if known {
            "已连接"
        } else {
            "暂无数据"
        },
        r(x + width - 74, y, 74, 22),
        10,
        if view.stale { WARN } else { MUTED },
        false,
        true,
        scale,
    );
    y += if cursor { 32 } else { 46 };
    if let Some(error) = view.error {
        let message = if error.contains("Cookie") {
            "Cookie 缺失或已过期，请在设置中更新。"
        } else if error.contains("Login") {
            "登录不可用，请通过本机 CLI 登录。"
        } else if error.contains("Rate") {
            "平台限流，稍后自动重试。"
        } else {
            "刷新失败，保留最近数据等待重试。"
        };
        text(
            dc,
            message,
            r(x, y, width, 30),
            11,
            WARN,
            false,
            false,
            scale,
        );
        y += 40;
    }
    let cols = if cursor { 2 } else { columns(view, rect.h) };
    let cell_w = (width - (cols - 1) * 14) / cols;
    let mut ys = vec![y; cols as usize];
    // Balance consecutive rows by occupied height, retaining every metric.
    for row in &view.rows {
        let col = ys
            .iter()
            .enumerate()
            .min_by_key(|(_, y)| **y)
            .map(|(i, _)| i)
            .unwrap_or(0);
        metric(
            dc,
            row,
            r(x + col as i32 * (cell_w + 14), ys[col], cell_w, 46),
            view.stale,
            scale,
        );
        ys[col] += if row.value.is_some() { 30 } else { 46 };
    }
    y = ys.iter().max().copied().unwrap_or(y) + 6;
    if !view.activity.is_empty() {
        text(
            dc,
            "Tokens 用量",
            r(x, y, width, 18),
            11,
            MUTED,
            false,
            false,
            scale,
        );
        y += 24;
        let tw = (width - 8) / 2;
        for (i, a) in view.activity.iter().enumerate() {
            let bx = x + (i as i32 % 2) * (tw + 8);
            let by = y + (i as i32 / 2) * 48;
            box_fill(dc, r(bx, by, tw, 44), 12, 0x202020, scale);
            text(
                dc,
                &a.label,
                r(bx + 9, by + 3, tw - 18, 15),
                10,
                FAINT,
                false,
                false,
                scale,
            );
            text(
                dc,
                a.value.as_deref().unwrap_or("—"),
                r(bx + 9, by + 17, tw - 18, 25),
                21,
                TEXT,
                true,
                false,
                scale,
            );
        }
    }
}

pub unsafe fn draw_dashboard(dc: HDC, state: &AppState, w: i32, h: i32, scale: f32) {
    let (width, height) = logical(w, h, scale);
    gdi_fill(dc, 0, 0, w, h, PANEL);
    for y in (0..240).step_by(8) {
        for x in (0..240).step_by(8) {
            let d = ((x * x + y * y) as f32).sqrt() / 240.0;
            let t = (1.0 - d).max(0.0);
            let block = box_scaled(r(x, y, 8, 8), scale);
            gdi_fill(
                dc,
                block.x,
                block.y,
                block.w,
                block.h,
                blend(PANEL, RED, 0.10 * t * t),
            );
        }
    }
    for x in (0..w).step_by((34.0 * scale).max(1.0) as usize) {
        gdi_fill(dc, x, 0, 1, h, 0x151515);
    }
    text(
        dc,
        if state.settings_open {
            "设置"
        } else {
            "用量总览"
        },
        r(66, 20, width - 240, 28),
        18,
        TEXT,
        true,
        false,
        scale,
    );
    text(
        dc,
        if state.settings_open {
            "显示、采集与更新"
        } else {
            "Claude · Codex · Cursor"
        },
        r(66, 48, width - 240, 17),
        11,
        MUTED,
        false,
        false,
        scale,
    );
    box_fill(dc, r(20, 24, 34, 34), 12, 0x30171A, scale);
    icon(
        dc,
        if state.settings_open {
            "settings"
        } else {
            "desktop"
        },
        r(27, 31, 20, 20),
        0xFFB3BA,
        0x30171A,
        scale,
    );
    if state.settings_open {
        let cw = (width - 52) / 2;
        for (rect, title) in [
            (r(20, 88, cw, 224), "平台与任务栏"),
            (r(32 + cw, 88, cw, 224), "刷新与启动"),
            (r(20, 324, cw, 174), "Cursor 认证"),
            (r(32 + cw, 324, cw, 174), "应用更新"),
        ] {
            card(dc, rect, scale);
            text(
                dc,
                title,
                r(rect.x + 14, rect.y + 10, rect.w - 28, 20),
                12,
                MUTED,
                true,
                false,
                scale,
            );
        }
        text(
            dc,
            "刷新间隔",
            r(46 + cw, 117, cw - 28, 20),
            11,
            MUTED,
            false,
            false,
            scale,
        );
        text(
            dc,
            "实际刷新遵守平台限流与退避。",
            r(46 + cw, 270, cw - 28, 20),
            10,
            FAINT,
            false,
            false,
            scale,
        );
        text(
            dc,
            "粘贴新的 Cookie 后保存；仅存本机。",
            r(34, 354, cw - 28, 16),
            10,
            FAINT,
            false,
            false,
            scale,
        );
        box_fill(dc, r(34, 374, cw - 28, 34), 14, LINE, scale);
        box_fill(dc, r(35, 375, cw - 30, 32), 13, PANEL, scale);
        text(
            dc,
            "粘贴新的 Cursor Cookie",
            r(44, 376, cw - 48, 30),
            11,
            FAINT,
            false,
            false,
            scale,
        );
        text(
            dc,
            "Claude / Codex 使用本机 CLI 登录状态。",
            r(34, 464, cw - 28, 20),
            10,
            FAINT,
            false,
            false,
            scale,
        );
    } else {
        let view = presentation::snapshot(state);
        let top: Vec<_> = view
            .services
            .iter()
            .filter(|s| s.enabled && s.id != "cursor")
            .collect();
        let cursor = view.services.iter().find(|s| s.enabled && s.id == "cursor");
        let cursor_h = cursor.map_or(0, |s| 106 + if s.error.is_some() { 40 } else { 0 });
        let top_h = height - 142 - if cursor.is_some() { cursor_h + 12 } else { 0 };
        let cw = if top.len() == 2 {
            (width - 52) / 2
        } else {
            width - 40
        };
        for (i, s) in top.iter().enumerate() {
            service(
                dc,
                s,
                r(20 + i as i32 * (cw + 12), 84, cw, top_h),
                false,
                scale,
            );
        }
        if let Some(s) = cursor {
            service(
                dc,
                s,
                r(
                    20,
                    84 + if top.is_empty() { 0 } else { top_h + 12 },
                    width - 40,
                    cursor_h,
                ),
                true,
                scale,
            );
        }
        if top.is_empty() && cursor.is_none() {
            text(
                dc,
                "尚未启用平台，请在设置中开启。",
                r(30, 200, width - 60, 80),
                14,
                MUTED,
                false,
                false,
                scale,
            );
        }
    }
    for (i, b) in controls(state, w, h, scale).iter().enumerate() {
        // Controls are already transformed. Convert once to the design coordinate system.
        let br = r(
            (b.rect.x as f32 / scale).round() as i32,
            (b.rect.y as f32 / scale).round() as i32,
            (b.rect.w as f32 / scale).round() as i32,
            (b.rect.h as f32 / scale).round() as i32,
        );
        let hover = state.ui_hover == Some(b.command);
        let focus = state.ui_focus == i + 1;
        let toggle = b.toggle.is_some() && ![201, 202, 203].contains(&b.command);
        let bg = if toggle {
            if hover {
                0x252525
            } else {
                CARD
            }
        } else if b.toggle == Some(true) {
            0x421A20
        } else if hover {
            0x2A2A2A
        } else {
            0x202020
        };
        if !toggle {
            box_fill(dc, br, 15, if focus { RED } else { LINE }, scale);
            box_fill(dc, r(br.x + 1, br.y + 1, br.w - 2, br.h - 2), 14, bg, scale);
        } else if hover || focus {
            box_fill(dc, br, 9, if focus { 0x421A20 } else { 0x252525 }, scale);
        }
        let ix = br.x + if toggle { 0 } else { 10 };
        if !b.icon.is_empty() {
            icon(dc, b.icon, r(ix, br.y + 7, 16, 16), TEXT, bg, scale);
        }
        let tx = if b.icon.is_empty() {
            br.x + if toggle { 0 } else { 10 }
        } else {
            ix + 23
        };
        text(
            dc,
            &b.text,
            r(
                tx,
                br.y,
                br.w - (tx - br.x) - if toggle { 40 } else { 8 },
                30,
            ),
            12,
            TEXT,
            true,
            false,
            scale,
        );
        if toggle {
            let on = b.toggle == Some(true);
            box_fill(
                dc,
                r(br.x + br.w - 30, br.y + 6, 30, 18),
                9,
                if on { RED } else { LINE },
                scale,
            );
            box_fill(
                dc,
                r(br.x + br.w - 27 + if on { 12 } else { 0 }, br.y + 9, 12, 12),
                6,
                TEXT,
                scale,
            );
        }
    }
    let note = if !state.ui_notice.is_empty() {
        state.ui_notice.clone()
    } else if state.settings_open && !state.update.message.is_empty() {
        state.update.message.clone()
    } else {
        format!("v{} · Windows", env!("CARGO_PKG_VERSION"))
    };
    text(
        dc,
        &note,
        r(330, height - 50, width - 354, 30),
        10,
        FAINT,
        false,
        true,
        scale,
    );
}

/// Taskbar rows for one service: Claude 5h/7d, otherwise the first two quota rows.
/// Value-only rows (Credits, reset counts) never enter the bar.
pub fn bar_rows(service: &ServiceView) -> Vec<&RowView> {
    let quota: Vec<&RowView> = service.rows.iter().filter(|r| r.value.is_none()).collect();
    if service.id == "claude" {
        let pick: Vec<&RowView> = ["5h", "7d"]
            .iter()
            .filter_map(|l| quota.iter().find(|r| r.label == *l).copied())
            .collect();
        if !pick.is_empty() {
            return pick;
        }
    }
    quota.into_iter().take(2).collect()
}
fn bar_label(label: &str) -> &str {
    match label {
        "额度" => "",
        v => v,
    }
}
pub(super) unsafe fn draw_bar(dc: HDC, state: &AppState, w: i32, h: i32, scale: f32) {
    gdi_fill(dc, 0, 0, w, h, BG_CW);
    let view = presentation::snapshot(state);
    let services: Vec<_> = view.services.iter().filter(|s| s.enabled).collect();
    let (width, height) = logical(w, h, scale);
    let cw = width / services.len().max(1) as i32;
    for (i, s) in services.iter().enumerate() {
        let x = i as i32 * cw;
        icon(
            dc,
            s.id,
            r(x + 8, (height - 20) / 2, 20, 20),
            if s.stale { MUTED } else { TEXT },
            BG_CW,
            scale,
        );
        let rows = bar_rows(s);
        let n = rows.len().max(1) as i32;
        let pct_w = 32;
        let label_w = 24;
        for (k, row) in rows.iter().enumerate() {
            let cy = height / 2 + (k as i32 * 2 - (n - 1)) * 8;
            let util = row.utilization.filter(|u| u.is_finite());
            let color = if s.stale {
                MUTED
            } else if util.is_none() {
                FAINT
            } else {
                used_color(util)
            };
            text(
                dc,
                bar_label(&row.label),
                r(x + 34, cy - 8, label_w, 16),
                9,
                FAINT,
                false,
                false,
                scale,
            );
            text(
                dc,
                &if util.is_some() {
                    fmt_pct(util)
                } else {
                    "—".into()
                },
                r(x + cw - 6 - pct_w, cy - 8, pct_w, 16),
                11,
                color,
                true,
                true,
                scale,
            );
            if state.settings.mini_progress {
                let bx = x + 34 + label_w + 2;
                let bw = (cw - 6 - pct_w - 4 - bx + x).max(8);
                box_fill(dc, r(bx, cy - 1, bw, 3), 2, LINE, scale);
                if let Some(u) = util {
                    let fill = (bw as f32 * u.clamp(0.0, 1.0)).round() as i32;
                    if fill > 0 {
                        box_fill(
                            dc,
                            r(bx, cy - 1, fill, 3),
                            2,
                            if s.stale { FAINT } else { color },
                            scale,
                        );
                    }
                } else {
                    for t in (0..bw).step_by(6) {
                        box_fill(dc, r(bx + t, cy, 3.min(bw - t), 1), 0, FAINT, scale);
                    }
                }
            }
        }
        if s.stale {
            box_fill(dc, r(x + cw - 6, 4, 4, 4), 2, WARN, scale);
        }
    }
    if services.is_empty() {
        icon(
            dc,
            "desktop",
            r(10, (height - 20) / 2, 20, 20),
            MUTED,
            BG_CW,
            scale,
        );
        text(
            dc,
            "未启用平台",
            r(40, 0, width - 46, height),
            11,
            MUTED,
            false,
            false,
            scale,
        );
    }
}
pub unsafe fn paint(hwnd: HWND, state: &AppState, bar: bool, scale: f32) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let mem = CreateCompatibleDC(dc);
    let bmp = CreateCompatibleBitmap(dc, rect.right.max(1), rect.bottom.max(1));
    let old = SelectObject(mem, bmp);
    if bar {
        draw_bar(mem, state, rect.right, rect.bottom, scale);
    } else {
        draw_dashboard(mem, state, rect.right, rect.bottom, scale);
    }
    let _ = BitBlt(dc, 0, 0, rect.right, rect.bottom, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{CreateDIBSection, GdiFlush};
    #[test]
    fn dashboard_renders_all_states_and_uses_identical_hit_rectangles_at_each_dpi() {
        let mut state = AppState::default();
        state.settings.theme = Theme::ChoseWay;
        state.claude.rows = (0..5)
            .map(|i| crate::state::UsageRow {
                label: ["5h", "7d", "Sonnet", "Opus", "Extra"][i].into(),
                utilization: Some([0.32, 0.48, 0.24, 0.61, 0.0][i]),
                reset: None,
            })
            .collect();
        state.claude.tier = Some("Max · 20×".into());
        state.claude.fetched_at = Some(chrono::Utc::now());
        state.codex.plan = Some("pro".into());
        state.codex.windows = vec![crate::state::CodexWindow {
            label: "7d".into(),
            minutes: 10080,
            utilization: 0.64,
            reset: None,
        }];
        state.codex.daily = Some(vec![crate::state::DailyTokens {
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            tokens: 1280000,
        }]);
        state.cursor.auto_usage_pct = Some(0.18);
        state.cursor.api_usage_pct = Some(0.86);
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            for settings in [false, true] {
                state.settings_open = settings;
                let width = (640.0 * scale) as i32;
                let height = (590.0 * scale) as i32;
                for b in controls(&state, width, height, scale) {
                    assert!(
                        b.rect.x >= 0
                            && b.rect.y >= 0
                            && b.rect.x + b.rect.w <= width
                            && b.rect.y + b.rect.h <= height
                    );
                    assert_eq!(
                        hit_test(
                            &state,
                            width,
                            height,
                            scale,
                            b.rect.x + b.rect.w / 2,
                            b.rect.y + b.rect.h / 2
                        ),
                        Some(b.command)
                    );
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
                    let bitmap =
                        CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
                    let old = SelectObject(dc, bitmap);
                    draw_dashboard(dc, &state, width, height, scale);
                    let _ = GdiFlush();
                    let pixels = std::slice::from_raw_parts(
                        bits as *const u8,
                        (width * height * 4) as usize,
                    );
                    assert!(
                        pixels
                            .chunks_exact(4)
                            .filter(|p| p[0] > 180 && p[1] > 180 && p[2] > 180)
                            .count()
                            > 300
                    );
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
                        std::fs::write(
                            dir.join(format!(
                                "choseway-{}-{}.bmp",
                                if settings { "settings" } else { "overview" },
                                (scale * 100.0) as i32
                            )),
                            bmp,
                        )
                        .unwrap();
                    }
                    SelectObject(dc, old);
                    let _ = DeleteObject(bitmap);
                    let _ = DeleteDC(dc);
                }
            }
        }
        state.settings_open = false;
        state
            .claude
            .rows
            .extend((0..12).map(|i| crate::state::UsageRow {
                label: format!("Model {i}"),
                utilization: None,
                reset: None,
            }));
        let size = preferred_size(&state, 1280, 720);
        assert!(size.0 <= 1280 && size.1 <= 720);
        let view = presentation::snapshot(&state);
        let card_h = size.1 - 260;
        let claude = &view.services[0];
        let cols = columns(claude, card_h);
        assert!(
            measure(claude, cols) <= card_h,
            "every dynamic row must fit without a scroll view"
        );
    }
}

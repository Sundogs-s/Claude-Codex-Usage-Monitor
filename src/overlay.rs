/// Modern GDI overlay painter.
///
/// Floating mode (default 320 × 200):
///   ┌─────────────────────────────────────┐
///   │  USAGE MONITOR                   ● │  ← header 28px
///   ├─────────────────────────────────────┤
///   │  ◆ CLAUDE                           │
///   │    5h  ████████████░░  82%  4h 23m  │
///   │    7d  █████░░░░░░░░░  41%  3d 12h  │
///   ├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┤  (dashed)
///   │  ◆ CODEX                            │
///   │    5h  ████░░░░░░░░░░  35%  3h 07m  │
///   │    7d  ███████░░░░░░░  47%  4d 08h  │
///   └─────────────────────────────────────┘
///
/// AppBar-dock mode (300 × 40, embedded in taskbar):
///   [◆C  5h ████ 82% 4h23m │ ◆X  5h ████ 35% 3h07m]
///         7d ████ 41% 3d12h        7d ████ 22% 5d06h
///
/// Compact-bar mode (full monitor width × 38px, floating above taskbar):
///   [◆ C  5h ███  82%  4h23m │ ◆ X  5h ██  35%  3h07m]
use crate::state::{fmt_countdown, fmt_pct, secs_until, AppState, DisplayMode};
use log::{debug, info};
use std::sync::atomic::{AtomicU32, Ordering};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{COLORREF, HWND, POINT, RECT},
        Graphics::Gdi::{
            BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
            CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW,
            EndPaint, FillRect, GetDeviceCaps, GetPixel, GetWindowDC, HFONT, LineTo, MoveToEx,
            Polygon, ReleaseDC, RoundRect, SelectObject, SetBkMode, SetPolyFillMode,
            SetTextColor, SetWindowRgn, ALTERNATE, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
            DRAW_TEXT_FORMAT, HDC, LOGPIXELSX, PAINTSTRUCT, PS_NULL, PS_SOLID, SRCCOPY,
            TRANSPARENT,
        },
        UI::WindowsAndMessaging::{GetClientRect, GetParent, GetWindowRect},
    },
};

// ─── Palette (0xRRGGBB) ──────────────────────────────────────────────────────
const BG:           u32 = 0x0F1117;
const HEADER_BG:    u32 = 0x161B22;
const DIVIDER:      u32 = 0x2D3142;
const TEXT_TITLE:   u32 = 0xCDD9E5;
const TEXT_PRIMARY: u32 = 0xE6EDF3;
const TEXT_DIM:     u32 = 0x8B949E;
const CLAUDE_HI:    u32 = 0xFFC247;
const CLAUDE_MID:   u32 = 0xF0A500;
const CLAUDE_LO:    u32 = 0x9B6A00;
const CODEX_HI:     u32 = 0x3DEFA0;
const CODEX_MID:    u32 = 0x00B16A;
const CODEX_LO:     u32 = 0x007045;
const WARN_HI:      u32 = 0xFF7B7B;
const WARN_MID:     u32 = 0xF85149;
const WARN_LO:      u32 = 0x9B2020;
const BAR_TRACK:    u32 = 0x21262D;
static APPBAR_PAINT_COUNT: AtomicU32 = AtomicU32::new(0);

fn rgb(c: u32) -> COLORREF {
    COLORREF(((c >> 16) & 0xFF) | (((c >> 8) & 0xFF) << 8) | ((c & 0xFF) << 16))
}

fn blend(a: u32, b: u32, t: f32) -> u32 {
    let lerp = |x: u32, y: u32| -> u32 {
        ((x as f32 * (1.0 - t) + y as f32 * t) as u32).min(255)
    };
    (lerp((a>>16)&0xFF, (b>>16)&0xFF) << 16)
    | (lerp((a>>8)&0xFF, (b>>8)&0xFF) << 8)
    |  lerp(a&0xFF, b&0xFF)
}

fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ─── Rounded window region ────────────────────────────────────────────────────
pub fn apply_rounded_region(hwnd: HWND, w: i32, h: i32) {
    unsafe {
        let hrgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, 14, 14);
        SetWindowRgn(hwnd, hrgn, true);
    }
}

// ─── Public paint entry ───────────────────────────────────────────────────────
pub fn paint(hwnd: HWND, state: &AppState) {
    debug!(
        "[paint] c5h={:?} c7d={:?} | x5h={:?} x7d={:?}",
        state.claude.utilization_5h, state.claude.utilization_7d,
        state.codex.utilization_5h,  state.codex.utilization_7d,
    );
    unsafe {
        match state.settings.display_mode {
            DisplayMode::Floating   => paint_floating(hwnd, state),
            DisplayMode::CompactBar => paint_compact(hwnd, state),
            DisplayMode::AppBar     => paint_appbar(hwnd, state),
        }
    }
}

// ─── Floating mode ────────────────────────────────────────────────────────────
unsafe fn paint_floating(hwnd: HWND, state: &AppState) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    GetClientRect(hwnd, &mut rc).ok();
    let w = rc.right;
    let h = rc.bottom;

    let dpi = GetDeviceCaps(hdc, LOGPIXELSX);
    // Design spec: title 10.5pt/600 → fh_title; section heading 12.5pt/600 → fh_head;
    // row labels/values 11pt mono → fh_row
    let fh_title = -(10 * dpi / 72);  // ~10pt covers 10.5pt well in GDI
    let fh_head  = -(12 * dpi / 72);  // ~12pt for 12.5pt heading
    let fh_row   = -(9  * dpi / 72);  // ~9pt for 11pt mono rows

    let fn_ui_bold = wstr("Segoe UI Semibold");
    let fn_mono    = wstr("JetBrains Mono");
    SetBkMode(hdc, TRANSPARENT);

    // Background
    gdi_fill(hdc, 0, 0, w, h, BG);

    // Header bar — 28px, #161B22
    let header_h = 28i32;
    gdi_fill(hdc, 0, 0, w, header_h, HEADER_BG);

    // Title text: "USAGE MONITOR" 10.5pt/600, #CDD9E5, padding 0 12px
    let hft = make_font_weight(&fn_ui_bold, fh_title, 600);
    let old = SelectObject(hdc, hft);
    SetTextColor(hdc, rgb(TEXT_TITLE));
    gdi_text(hdc, "USAGE MONITOR", 12, 0, w - 32, header_h,
             DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);
    DeleteObject(hft);

    // Status dot: 8px circle, right side, padding 12px from right
    let dot_r = 4i32;
    let dot_cx = w - 12 - dot_r;
    let dot_cy = header_h / 2;
    let dot_color = if state.claude_error.is_empty() && state.codex_error.is_empty() {
        CODEX_HI  // #3DEFA0 — connected
    } else {
        WARN_MID  // #F85149 — error
    };
    gdi_circle(hdc, dot_cx, dot_cy, dot_r, dot_color);

    // Header bottom border — 1px #2D3142
    gdi_hline(hdc, 0, header_h, w, DIVIDER);

    // Section layout: sections share remaining height equally
    let show_claude = state.settings.show_claude;
    let show_codex  = state.settings.show_codex;
    let n_sections = (show_claude as i32) + (show_codex as i32);
    let content_h = h - header_h;
    let section_h = if n_sections > 0 { content_h / n_sections } else { content_h };

    let mut sec_y = header_h;

    if show_claude {
        draw_float_section(
            hdc, &fn_ui_bold, &fn_mono, fh_head, fh_row,
            0, sec_y, w, section_h,
            "Claude", CLAUDE_HI, CLAUDE_MID, CLAUDE_LO,
            state.claude.utilization_5h, secs_until(state.claude.reset_5h),
            state.claude.utilization_7d, secs_until(state.claude.reset_7d),
        );
        sec_y += section_h;

        // Dashed divider between Claude and Codex sections
        if show_codex {
            let dash = 4i32;
            let gap  = 3i32;
            let pad  = 14i32;
            let mut dx = pad;
            while dx < w - pad {
                let seg = (dx + dash).min(w - pad) - dx;
                gdi_fill(hdc, dx, sec_y, seg, 1, DIVIDER);
                dx += dash + gap;
            }
        }
    }

    if show_codex {
        draw_float_section(
            hdc, &fn_ui_bold, &fn_mono, fh_head, fh_row,
            0, sec_y, w, section_h,
            "Codex", CODEX_HI, CODEX_MID, CODEX_LO,
            state.codex.utilization_5h, secs_until(state.codex.reset_5h),
            state.codex.utilization_7d, secs_until(state.codex.reset_7d),
        );
    }

    EndPaint(hwnd, &ps);
}

/// Draw one service section within the floating window.
/// Spec: padding 8px top, 14px left/right, 8px bottom.
/// Layout: heading row (diamond + name, 12.5pt/600, brand-light),
///         then two rows (5h / 7d): label 16px | bar flex | pct 40px right | eta 44px
///         bar height 8px, radius 4px.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_float_section(
    hdc: HDC,
    fn_bold: &[u16], fn_mono: &[u16],
    fh_head: i32, fh_row: i32,
    sx: i32, sy: i32, sw: i32, sh: i32,
    name: &str,
    hi: u32, mid: u32, lo: u32,
    u5h: Option<f32>, s5h: i64,
    u7d: Option<f32>, s7d: i64,
) {
    let pad_x   = 14i32;
    let pad_top =  8i32;
    let label_w = 16i32;
    let pct_w   = 40i32;
    let eta_w   = 44i32;
    let bar_h   =  8i32;
    let bar_r   =  4i32;
    let head_h  = 18i32;
    let row_h   = 22i32;
    let row_gap =  5i32;

    let content_x = sx + pad_x;
    let content_w = sw - pad_x * 2;
    let bar_w = content_w - label_w - 8 - pct_w - 4 - eta_w;
    let bar_w = bar_w.max(20);

    let mut y = sy + pad_top;

    // Heading: diamond icon + name text
    let dia_cx = content_x + 5;
    let dia_cy = y + head_h / 2;
    draw_diamond(hdc, dia_cx, dia_cy, 5, hi);

    let hfh = make_font_weight(fn_bold, fh_head, 600);
    let old = SelectObject(hdc, hfh);
    SetTextColor(hdc, rgb(hi));
    gdi_text(hdc, name, content_x + 14, y, content_w - 14, head_h,
             DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);
    DeleteObject(hfh);
    y += head_h + 4;

    // Two data rows — centered in remaining section height
    let rows_total = row_h * 2 + row_gap;
    let remaining = sh - (y - sy);
    if remaining > rows_total {
        y += (remaining - rows_total) / 2;
    }

    for (label, util, secs) in [("5h", u5h, s5h), ("7d", u7d, s7d)] {
        let bx = content_x + label_w + 8;
        let by = y + (row_h - bar_h) / 2;

        // Label: 16px wide, #CDD9E5, mono 600
        let hfm = make_font_weight(fn_mono, fh_row, 600);
        let old2 = SelectObject(hdc, hfm);
        SetTextColor(hdc, rgb(TEXT_TITLE));
        gdi_text(hdc, label, content_x, y, label_w, row_h,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        // Track
        gdi_rrect(hdc, bx, by, bar_w, bar_h, bar_r, BAR_TRACK);
        // Fill
        let fw = util.map(|u| ((u * bar_w as f32) as i32).max(0).min(bar_w)).unwrap_or(0);
        if fw > 0 {
            let (bhi, bmid, blo) = bar_colors(util, hi, mid, lo);
            gdi_grad_bar(hdc, bx, by, fw, bar_h, blo, bmid, bhi);
        }

        // Pct: 40px right-aligned, warn color at ≥80%
        let warn = util.unwrap_or(0.0) >= 0.80;
        SetTextColor(hdc, rgb(if warn { WARN_HI } else { TEXT_PRIMARY }));
        gdi_text(hdc, &fmt_pct(util),
                 bx + bar_w + 4, y, pct_w, row_h,
                 DT_RIGHT | DT_SINGLELINE | DT_VCENTER);

        // ETA: 44px, #8B949E
        SetTextColor(hdc, rgb(TEXT_DIM));
        gdi_text(hdc, &fmt_countdown(secs),
                 bx + bar_w + 4 + pct_w + 2, y, eta_w, row_h,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        SelectObject(hdc, old2);
        DeleteObject(hfm);
        y += row_h + row_gap;
    }
}

// (draw_section removed — replaced by draw_float_section above)

// ─── AppBar dock mode (300×40, embedded in taskbar) ──────────────────────────
//
// Design spec (components.jsx AppBarDock / DockSide / DockRow):
//   Total size: 300×40, background #161B22 (bgTitle)
//   Each side: padding 0 8px; badge = diamond(9px) + letter mono 10px/700; gap 7px
//   Two stacked rows, gap 3px, vertically centred
//   Row: label 12px · bar flex · pct 26px right-align · eta 30px
//   Bar: 5px height · radius 2.5
unsafe fn paint_appbar(hwnd: HWND, state: &AppState) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    GetClientRect(hwnd, &mut rc).ok();
    let w = rc.right;
    let h = rc.bottom;
    let n = APPBAR_PAINT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n % 120 == 1 {
        info!(
            "[paint_appbar] count={} hwnd={:?} size={}x{} show_claude={} show_codex={} c5h={:?} x5h={:?}",
            n,
            hwnd,
            w,
            h,
            state.settings.show_claude,
            state.settings.show_codex,
            state.claude.utilization_5h,
            state.codex.utilization_5h
        );
    }

    // Scale all appbar typography/layout from the design base (300x40).
    // This keeps proportions stable when taskbar height is not 40 px.
    let scale = (h.max(1) as f32) / 40.0;
    let px_row = (9.5 * scale).round().clamp(8.0, 18.0) as i32;
    let px_badge = (10.0 * scale).round().clamp(8.0, 19.0) as i32;
    let fh_row = -px_row;
    let fh_badge = -px_badge;

    let fn_mono = wstr("JetBrains Mono");

    // Double-buffered paint to avoid visible redraw jitter in taskbar embedding.
    let mem_dc = CreateCompatibleDC(hdc);
    let mem_bmp = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
    let old_bmp = SelectObject(mem_dc, mem_bmp);
    SetBkMode(mem_dc, TRANSPARENT);

    // Deterministic clear: sample taskbar color and paint full rect each frame.
    // This avoids stale-text ghosting caused by parent-background self-copy.
    let clear = sample_taskbar_color(hwnd).unwrap_or(BG);
    gdi_fill(mem_dc, 0, 0, w, h, clear);

    let show_claude = state.settings.show_claude;
    let show_codex  = state.settings.show_codex;
    let both = show_claude && show_codex;
    let side_w = if both { w / 2 } else { w };
    let mut x_off = 0i32;

    if show_claude {
        draw_dock_side(
            mem_dc, &fn_mono, fh_badge, fh_row,
            x_off, 0, side_w, h,
            "C", CLAUDE_HI, CLAUDE_MID, CLAUDE_LO,
            state.claude.utilization_5h, secs_until(state.claude.reset_5h),
            state.claude.utilization_7d, secs_until(state.claude.reset_7d),
        );
        x_off += side_w;
    }

    if both {
        // 1px solid #2D3142 center divider, full height
        gdi_vline(mem_dc, x_off, 0, h, DIVIDER);
    }

    if show_codex {
        draw_dock_side(
            mem_dc, &fn_mono, fh_badge, fh_row,
            x_off, 0, side_w, h,
            "X", CODEX_HI, CODEX_MID, CODEX_LO,
            state.codex.utilization_5h, secs_until(state.codex.reset_5h),
            state.codex.utilization_7d, secs_until(state.codex.reset_7d),
        );
    }

    let _ = BitBlt(hdc, 0, 0, w, h, mem_dc, 0, 0, SRCCOPY);
    SelectObject(mem_dc, old_bmp);
    DeleteObject(mem_bmp);
    DeleteDC(mem_dc);

    EndPaint(hwnd, &ps);
}

unsafe fn sample_taskbar_color(hwnd: HWND) -> Option<u32> {
    let parent = GetParent(hwnd).unwrap_or_default();
    if parent.0 == std::ptr::null_mut() {
        return None;
    }
    let parent_dc = GetWindowDC(parent);
    if parent_dc.0 == std::ptr::null_mut() {
        return None;
    }

    let mut wr = RECT::default();
    let mut pr = RECT::default();
    let _ = GetWindowRect(hwnd, &mut wr);
    let _ = GetWindowRect(parent, &mut pr);

    // Prefer sampling just left of appbar to avoid reading our own rendered pixels.
    let sx = (wr.left - pr.left - 3).max(0);
    let sy = ((wr.top - pr.top) + 3).max(0);
    let c = GetPixel(parent_dc, sx, sy);
    let _ = ReleaseDC(parent, parent_dc);
    if c.0 == u32::MAX {
        return None;
    }
    let v = c.0;
    let r = v & 0xFF;
    let g = (v >> 8) & 0xFF;
    let b = (v >> 16) & 0xFF;
    Some((r << 16) | (g << 8) | b)
}

/// Draw one side of the AppBar dock (Claude or Codex).
/// Spec: padding 0 8px; badge (diamond 9px + letter mono 10pt/700); gap 7px to rows.
/// Rows: label 12px | bar flex | pct 26px right | eta 30px; bar 5px, r=2; gap 3px.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_dock_side(
    hdc: HDC,
    fn_mono: &[u16],
    _fh_badge: i32, fh_row: i32,
    x: i32, _y: i32, side_w: i32, h: i32,
    _letter: &str,
    hi: u32, mid: u32, lo: u32,
    u5h: Option<f32>, s5h: i64,
    u7d: Option<f32>, s7d: i64,
) {
    let scale = (h.max(1) as f32) / 40.0;
    let sc = |v: i32| ((v as f32) * scale).round().max(1.0) as i32;

    let pad      = sc(2);   // 左边界 padding 减小（原 4）
    let stripe_w = sc(4);
    let stripe_gap = sc(4);
    let label_w  = sc(10).clamp(9, 14);
    let pct_w    = sc(26).clamp(24, 34);
    let eta_w    = sc(32).clamp(30, 42);  // keep room but avoid squeezing progress bar
    let bar_h    = sc(5).max(3);
    let row_gap  = sc(6);

    // Left accent stripe (full appbar height, flush to side boundary).
    gdi_fill(hdc, x, 0, stripe_w, h, hi);

    let rows_area_x = x + pad + stripe_w + stripe_gap;
    let bar_w = (side_w - (rows_area_x - x) - pad - label_w - 5 - pct_w - 2 - eta_w).max(20);

    // Vertical centering: two bars + gap
    let total_h = bar_h * 2 + row_gap;
    let y_top   = (h - total_h) / 2;

    let bx = rows_area_x + label_w + 5;  // bar start x（间距从 2 增加到 5）

    // Row helper closure — draw label, track, fill, pct, eta at y_row
    for (row_y, label, util, secs) in [
        (y_top,              "5h", u5h, s5h),
        (y_top + bar_h + row_gap, "7d", u7d, s7d),
    ] {
        let hfm = make_font_weight(fn_mono, fh_row, 600);
        let old2 = SelectObject(hdc, hfm);

        // Label
        SetTextColor(hdc, rgb(TEXT_TITLE));
        gdi_text(hdc, label,
                 rows_area_x, row_y - 2, label_w, bar_h + 4,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        // Track
        gdi_rrect(hdc, bx, row_y, bar_w, bar_h, 2, BAR_TRACK);
        // Fill
        let fw = util.map(|u| ((u * bar_w as f32) as i32).max(0).min(bar_w)).unwrap_or(0);
        if fw > 0 {
            let (bhi, bmid, blo) = bar_colors(util, hi, mid, lo);
            gdi_grad_bar(hdc, bx, row_y, fw, bar_h, blo, bmid, bhi);
        }

        // Pct: 26px right-align, warn ≥80%
        let warn = util.unwrap_or(0.0) >= 0.80;
        SetTextColor(hdc, rgb(if warn { WARN_HI } else { TEXT_PRIMARY }));
        gdi_text(hdc, &fmt_pct(util),
                 bx + bar_w + 2, row_y - 2, pct_w, bar_h + 4,
                 DT_RIGHT | DT_SINGLELINE | DT_VCENTER);

        // ETA: 30px, #8B949E
        SetTextColor(hdc, rgb(TEXT_DIM));
        gdi_text(hdc, &fmt_countdown(secs),
                 bx + bar_w + 2 + pct_w + 2, row_y - 2, eta_w, bar_h + 4,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        SelectObject(hdc, old2);
        DeleteObject(hfm);
    }
}

// ─── Compact-bar mode (floating above taskbar, full monitor width) ─────────
unsafe fn paint_compact(hwnd: HWND, state: &AppState) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    GetClientRect(hwnd, &mut rc).ok();
    let w = rc.right;
    let h = rc.bottom;

    let dpi = GetDeviceCaps(hdc, LOGPIXELSX);
    let fh  = -(8 * dpi / 72);
    let fn_norm = wstr("Segoe UI");
    let fn_bold = wstr("Segoe UI Semibold");
    SetBkMode(hdc, TRANSPARENT);

    gdi_fill(hdc, 0, 0, w, h, HEADER_BG);
    gdi_hline(hdc, 0, 0, w, DIVIDER);

    let show_claude = state.settings.show_claude;
    let show_codex  = state.settings.show_codex;
    let both = show_claude && show_codex;
    let half = if both { w / 2 } else { w };
    let pad  = 8i32;
    let bw   = 56i32;
    let bh   = 5i32;

    let mut x_off = 0i32;

    if show_claude {
        draw_compact_half(hdc, &fn_bold, &fn_norm, fh,
            x_off + pad, 0, half - pad * 2, h,
            "◆ C", CLAUDE_HI, CLAUDE_MID, CLAUDE_LO,
            state.claude.utilization_5h, secs_until(state.claude.reset_5h),
            state.claude.utilization_7d, secs_until(state.claude.reset_7d),
            bw, bh);
        x_off += half;
    }

    if both {
        gdi_vline(hdc, x_off, 6, h - 6, DIVIDER);
    }

    if show_codex {
        draw_compact_half(hdc, &fn_bold, &fn_norm, fh,
            x_off + pad, 0, half - pad * 2, h,
            "◆ X", CODEX_HI, CODEX_MID, CODEX_LO,
            state.codex.utilization_5h, secs_until(state.codex.reset_5h),
            state.codex.utilization_7d, secs_until(state.codex.reset_7d),
            bw, bh);
    }

    EndPaint(hwnd, &ps);
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_compact_half(
    hdc: HDC,
    fn_bold: &[u16], fn_norm: &[u16], fh: i32,
    x: i32, _y: i32, _w: i32, h: i32,
    label: &str, hi: u32, mid: u32, lo: u32,
    u5h: Option<f32>, s5h: i64,
    u7d: Option<f32>, s7d: i64,
    bw: i32, bh: i32,
) {
    // Brand label
    let hfb = make_font_weight(fn_bold, fh, 600);
    let old = SelectObject(hdc, hfb);
    SetTextColor(hdc, rgb(hi));
    gdi_text(hdc, label, x, 0, 22, h, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old); DeleteObject(hfb);

    let hfn = make_font_weight(fn_norm, fh, 400);
    let old2 = SelectObject(hdc, hfn);
    let bx = x + 24;

    // 5h bar
    let by5 = h / 2 - bh - 2;
    gdi_rrect(hdc, bx, by5, bw, bh, 2, BAR_TRACK);
    let fw5 = u5h.map(|u| ((u * bw as f32) as i32).max(0)).unwrap_or(0);
    if fw5 > 0 {
        let (bhi, bmid, blo) = bar_colors(u5h, hi, mid, lo);
        gdi_grad_bar(hdc, bx, by5, fw5, bh, blo, bmid, bhi);
    }
    SetTextColor(hdc, rgb(if u5h.unwrap_or(0.0) >= 0.80 { WARN_MID } else { TEXT_DIM }));
    gdi_text(hdc, &fmt_pct(u5h), bx + bw + 2, 0, 34, h / 2,
             DT_RIGHT | DT_SINGLELINE | DT_VCENTER);

    // 7d bar
    let by7 = h / 2 + 2;
    gdi_rrect(hdc, bx, by7, bw, bh, 2, BAR_TRACK);
    let fw7 = u7d.map(|u| ((u * bw as f32) as i32).max(0)).unwrap_or(0);
    if fw7 > 0 {
        let (bhi, bmid, blo) = bar_colors(u7d, hi, mid, lo);
        gdi_grad_bar(hdc, bx, by7, fw7, bh, blo, bmid, bhi);
    }
    SetTextColor(hdc, rgb(if u7d.unwrap_or(0.0) >= 0.80 { WARN_MID } else { TEXT_DIM }));
    gdi_text(hdc, &fmt_pct(u7d), bx + bw + 2, h / 2, 34,
             h / 2, DT_RIGHT | DT_SINGLELINE | DT_VCENTER);

    // Countdowns
    SetTextColor(hdc, rgb(TEXT_DIM));
    gdi_text(hdc, &fmt_countdown(s5h), bx + bw + 38, 0, 46, h / 2,
             DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    gdi_text(hdc, &fmt_countdown(s7d), bx + bw + 38, h / 2, 46, h / 2,
             DT_LEFT | DT_SINGLELINE | DT_VCENTER);

    SelectObject(hdc, old2); DeleteObject(hfn);
}

fn bar_colors(u: Option<f32>, hi: u32, mid: u32, lo: u32) -> (u32, u32, u32) {
    if u.unwrap_or(0.0) >= 0.80 { (WARN_HI, WARN_MID, WARN_LO) }
    else { (hi, mid, lo) }
}

// ─── Tray icon drawing ────────────────────────────────────────────────────────
/// Draw a 16×16 or 32×32 tray icon bitmap with two stacked progress bars
/// (top = Claude 5h, bottom = Codex 5h). Returns an HICON the caller must
/// DestroyIcon when done.
pub fn create_tray_icon_hicon(
    claude_5h: Option<f32>,
    codex_5h:  Option<f32>,
    size: i32,
) -> windows::Win32::UI::WindowsAndMessaging::HICON {
    use windows::Win32::{
        Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC,
            GetDC, PatBlt, ReleaseDC, BLACKNESS,
        },
        UI::WindowsAndMessaging::{CreateIconIndirect, ICONINFO},
    };

    unsafe {
        let hdc_screen = GetDC(None);
        let hdc_mem    = CreateCompatibleDC(hdc_screen);
        let hbm_color  = CreateCompatibleBitmap(hdc_screen, size, size);
        let hbm_mask   = CreateCompatibleBitmap(hdc_screen, size, size);
        let old_bm     = SelectObject(hdc_mem, hbm_color);

        SetBkMode(hdc_mem, TRANSPARENT);

        // Background: #161B22
        gdi_fill(hdc_mem, 0, 0, size, size, HEADER_BG);

        let pad   = if size == 16 { 2 } else { 4 };
        let bh    = if size == 16 { 3 } else { 6 };
        let gap   = if size == 16 { 2 } else { 3 };
        let iw    = size - pad * 2;
        let total = bh * 2 + gap;
        let y_top = (size - total) / 2;

        let c_pct  = claude_5h.unwrap_or(0.0);
        let x_pct  = codex_5h.unwrap_or(0.0);
        let c_warn = c_pct >= 0.80;
        let x_warn = x_pct >= 0.80;

        // Claude bar (top)
        gdi_rrect(hdc_mem, pad, y_top, iw, bh, bh / 2, BAR_TRACK);
        let cw = ((c_pct * iw as f32) as i32).max(0).min(iw);
        if cw > 0 {
            let (bhi, bmid, blo) = if c_warn { (WARN_HI, WARN_MID, WARN_LO) }
                                   else       { (CLAUDE_HI, CLAUDE_MID, CLAUDE_LO) };
            gdi_grad_bar(hdc_mem, pad, y_top, cw, bh, blo, bmid, bhi);
        }

        // Codex bar (bottom)
        let y_bot = y_top + bh + gap;
        gdi_rrect(hdc_mem, pad, y_bot, iw, bh, bh / 2, BAR_TRACK);
        let xw = ((x_pct * iw as f32) as i32).max(0).min(iw);
        if xw > 0 {
            let (bhi, bmid, blo) = if x_warn { (WARN_HI, WARN_MID, WARN_LO) }
                                   else       { (CODEX_HI, CODEX_MID, CODEX_LO) };
            gdi_grad_bar(hdc_mem, pad, y_bot, xw, bh, blo, bmid, bhi);
        }

        // Mask bitmap: all black = fully opaque
        SelectObject(hdc_mem, hbm_mask);
        PatBlt(hdc_mem, 0, 0, size, size, BLACKNESS);
        SelectObject(hdc_mem, old_bm);

        let icon_info = ICONINFO {
            fIcon:    true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask:  hbm_mask,
            hbmColor: hbm_color,
        };
        let hicon = CreateIconIndirect(&icon_info).unwrap_or_default();

        DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);
        DeleteObject(hbm_color);
        DeleteObject(hbm_mask);

        hicon
    }
}

// ─── Low-level GDI helpers ───────────────────────────────────────────────────

/// Create a font with explicit weight (400=regular, 600=semibold, 700=bold).
unsafe fn make_font_weight(name: &[u16], height: i32, weight: i32) -> HFONT {
    CreateFontW(height, 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 0, 0, PCWSTR(name.as_ptr()))
}

/// Filled rotated-square diamond centred at (cx, cy) with half-size `r`.
/// This matches the BrandDiamond SVG in the design: a 45°-rotated square
/// with slightly rounded corners (rx≈1.2/10 * full = small).
unsafe fn draw_diamond(hdc: HDC, cx: i32, cy: i32, r: i32, color: u32) {
    // 4 vertices of a square rotated 45°: top, right, bottom, left
    let pts = [
        POINT { x: cx,     y: cy - r },  // top
        POINT { x: cx + r, y: cy      },  // right
        POINT { x: cx,     y: cy + r  },  // bottom
        POINT { x: cx - r, y: cy      },  // left
    ];
    let hb = CreateSolidBrush(rgb(color));
    // Use PS_NULL pen so there's no border that bleeds outside the shape
    let hp = CreatePen(PS_NULL, 0, rgb(color));
    let ob = SelectObject(hdc, hb);
    let op = SelectObject(hdc, hp);
    SetPolyFillMode(hdc, ALTERNATE);
    Polygon(hdc, &pts);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    DeleteObject(hb);
    DeleteObject(hp);
}

/// Filled circle centred at (cx, cy) with radius r.
unsafe fn gdi_circle(hdc: HDC, cx: i32, cy: i32, r: i32, color: u32) {
    let hb = CreateSolidBrush(rgb(color));
    let hp = CreatePen(PS_NULL, 0, rgb(color));
    let ob = SelectObject(hdc, hb);
    let op = SelectObject(hdc, hp);
    RoundRect(hdc, cx - r, cy - r, cx + r + 1, cy + r + 1, (r * 2) + 1, (r * 2) + 1);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    DeleteObject(hb);
    DeleteObject(hp);
}

unsafe fn gdi_fill(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: u32) {
    let b = CreateSolidBrush(rgb(color));
    FillRect(hdc, &RECT { left: x, top: y, right: x + w, bottom: y + h }, b);
    DeleteObject(b);
}

unsafe fn gdi_rrect(hdc: HDC, x: i32, y: i32, w: i32, h: i32, r: i32, color: u32) {
    let b = CreateSolidBrush(rgb(color));
    let p = CreatePen(PS_SOLID, 0, rgb(color));
    let ob = SelectObject(hdc, b);
    let op = SelectObject(hdc, p);
    RoundRect(hdc, x, y, x + w, y + h, r * 2, r * 2);
    SelectObject(hdc, ob); SelectObject(hdc, op);
    DeleteObject(b); DeleteObject(p);
}

/// Smooth left-to-right gradient bar (lo → mid → hi).
unsafe fn gdi_grad_bar(hdc: HDC, x: i32, y: i32, w: i32, h: i32, lo: u32, mid: u32, hi: u32) {
    let steps = w.max(1);
    for i in 0..steps {
        let t = i as f32 / steps as f32;
        let col = if t < 0.5 { blend(lo, mid, t * 2.0) } else { blend(mid, hi, (t - 0.5) * 2.0) };
        let b = CreateSolidBrush(rgb(col));
        FillRect(hdc, &RECT { left: x+i, top: y, right: x+i+1, bottom: y+h }, b);
        DeleteObject(b);
    }
    // Highlight top pixel
    if h >= 2 {
        let hc = blend(hi, 0xFFFFFF, 0.30);
        let b  = CreateSolidBrush(rgb(hc));
        FillRect(hdc, &RECT { left: x, top: y, right: x+w, bottom: y+1 }, b);
        DeleteObject(b);
    }
}

unsafe fn gdi_hline(hdc: HDC, x: i32, y: i32, w: i32, color: u32) {
    let p = CreatePen(PS_SOLID, 1, rgb(color));
    let o = SelectObject(hdc, p);
    MoveToEx(hdc, x, y, None); LineTo(hdc, x + w, y);
    SelectObject(hdc, o); DeleteObject(p);
}

unsafe fn gdi_vline(hdc: HDC, x: i32, y1: i32, y2: i32, color: u32) {
    let p = CreatePen(PS_SOLID, 1, rgb(color));
    let o = SelectObject(hdc, p);
    MoveToEx(hdc, x, y1, None); LineTo(hdc, x, y2);
    SelectObject(hdc, o); DeleteObject(p);
}


unsafe fn gdi_text(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32, flags: DRAW_TEXT_FORMAT) {
    let mut ws = wstr(text);
    let mut rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
    DrawTextW(hdc, &mut ws, &mut rc, flags);
}

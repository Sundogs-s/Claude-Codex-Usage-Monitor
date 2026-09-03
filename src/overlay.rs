/// Modern GDI overlay painter.
///
/// Floating mode (default 320 × auto-height):
///   ┌─────────────────────────────────────┐
///   │  USAGE MONITOR                   ● │  ← header 28px (● green / amber=stale / red=error)
///   ├─────────────────────────────────────┤
///   │  ◆ CLAUDE              Max 5x · now │  ← 80px + 27px per extra row
///   │    5h     ████████████░░  82%  4h23m│
///   │    7d     █████░░░░░░░░░  41%  5d1h │
///   │    Fable  ██████░░░░░░░░  41%  3d12h│
///   ├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┤  (dashed, 8px gap)
///   │  ■ CODEX                            │  ← 80px section
///   │    5h  ████░░░░░░░░░░  35%  3h 07m  │
///   │    7d  ███████░░░░░░░  47%  4d 08h  │
///   ├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┤
///   │  ● CURSOR                           │  ← 80px section
///   │   AUTO  ████████████░  73%   8d 2h  │
///   │   API   ██░░░░░░░░░░░  15%   8d 2h  │
///   │  Claude · rate-limited · retry 4m   │  ← optional 16px banner (stale / error)
///   └─────────────────────────────────────┘
///
/// AppBar-dock mode (embedded in taskbar, 40px tall; Claude column 200px, others 180px):
///   [◆ 5h ████ 82% 4h23m │ ■ 5h ████ 35% 3h07m │ ● AUTO ████ 73%]
///      7d ████ 41% 5d1h     7d ████ 22% 5d06h     API  ██░░ 15%
///      Fable ██ 41% 3d12h
///
/// Compact-bar mode (full monitor width × 38px, floating above taskbar): same rows, stacked.
use crate::state::{
    appbar_col_widths, fmt_age, fmt_countdown, fmt_pct, neon_section_height, secs_until,
    section_height, AppState, DisplayMode, Theme, BANNER_H, BOTTOM_PAD, DIVIDER_GAP,
    NEON_BANNER_H, NEON_BOTTOM_PAD, NEON_DIVIDER_GAP, NEON_HEADER_H, NEON_HEAD_H, NEON_ROW_H,
    NEON_SEC_PAD,
};
use log::{debug, info};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{COLORREF, HWND, POINT, RECT},
        Graphics::Gdi::{
            BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
            CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW,
            EndPaint, FillRect, GetDC, GetDeviceCaps, GetPixel, GetTextFaceW, GetWindowDC, HFONT,
            LineTo, MoveToEx, ReleaseDC as GdiReleaseDC,
            Polygon, ReleaseDC, RoundRect, SelectObject, SetBkMode, SetPolyFillMode,
            SetTextCharacterExtra, SetTextColor, SetWindowRgn, ALTERNATE, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT,
            DT_SINGLELINE, DT_VCENTER, DRAW_TEXT_FORMAT, HDC, LOGPIXELSX, PAINTSTRUCT, PS_NULL,
            PS_SOLID, SRCCOPY, TRANSPARENT,
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
const CLAUDE_HI:    u32 = 0xBE6143;
const CLAUDE_MID:   u32 = 0x944B34;
const CLAUDE_LO:    u32 = 0x5B2E20;
const CODEX_HI:     u32 = 0x515FE4;
const CODEX_MID:    u32 = 0x3F4AB1;
const CODEX_LO:     u32 = 0x262D6D;
// Cursor brand: light-grey theme
const CURSOR_HI:     u32 = 0xF5F5F5;  // near-white (main text / icon)
const CURSOR_MID:    u32 = 0xAAAAAA;  // light-mid grey (bar fill)
const CURSOR_LO:     u32 = 0x606060;  // medium-dark grey (bar track tint / dim text)
const CURSOR_STRIPE: u32 = 0xB0B0B0;  // light-grey accent stripe
const WARN_HI:      u32 = 0xFF7B7B;
const WARN_MID:     u32 = 0xF85149;
const WARN_LO:      u32 = 0x9B2020;
const BAR_TRACK:    u32 = 0x21262D;
/// Status dot: live data.
const OK_DOT:       u32 = 0x3DEFA0;
/// Status dot / meta text: last-known data, usage API rate-limited.
const STALE:        u32 = 0xE3B341;
/// How far stale bars are faded towards the background (0 = none, 1 = invisible).
const STALE_FADE:   f32 = 0.45;
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

// ─── Font resolution ─────────────────────────────────────────────────────────
//
// GDI silently substitutes a missing face (JetBrains Mono → Courier New), which
// wrecks the layout. Resolve each role once against the installed fonts.
static MONO_FACE:    std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
static DISPLAY_FACE: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();

const MONO_CANDIDATES:    &[&str] = &["JetBrains Mono", "Cascadia Mono", "Consolas"];
const DISPLAY_CANDIDATES: &[&str] = &["Bahnschrift", "Segoe UI Semibold", "Segoe UI"];

/// First candidate face the font mapper actually honours (checked via GetTextFaceW).
fn resolve_face(candidates: &[&str]) -> Vec<u16> {
    unsafe {
        let hdc = GetDC(None);
        let mut chosen: Option<&str> = None;
        if !hdc.0.is_null() {
            for c in candidates {
                let name = wstr(c);
                let f = CreateFontW(-12, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 0, 0, PCWSTR(name.as_ptr()));
                let old = SelectObject(hdc, f);
                let mut buf = [0u16; 64];
                let n = GetTextFaceW(hdc, Some(&mut buf)).max(0) as usize;
                let got = String::from_utf16_lossy(&buf[..n.min(64)]);
                SelectObject(hdc, old);
                DeleteObject(f);
                if got.trim_end_matches('\0').eq_ignore_ascii_case(c) {
                    chosen = Some(c);
                    break;
                }
            }
            let _ = GdiReleaseDC(None, hdc);
        }
        let face = chosen.unwrap_or(candidates[candidates.len() - 1]);
        info!("[font] {:?} → {}", candidates, face);
        wstr(face)
    }
}

fn mono_face() -> &'static [u16] {
    MONO_FACE.get_or_init(|| resolve_face(MONO_CANDIDATES))
}

fn display_face() -> &'static [u16] {
    DISPLAY_FACE.get_or_init(|| resolve_face(DISPLAY_CANDIDATES))
}

// ─── Row / section models shared by the three painters ───────────────────────

#[derive(Clone)]
struct RowDef {
    label: String,
    util:  Option<f32>,
    secs:  i64,
}

struct SectionDef {
    name:      &'static str,
    hi:        u32,
    mid:       u32,
    lo:        u32,
    rows:      Vec<RowDef>,
    /// Small right-aligned text in the heading row (plan tier / data age / error).
    meta:      String,
    meta_col:  u32,
    /// Fade bars: data is last-known, not live.
    dim:       bool,
    is_cursor: bool,
}

/// Claude rows from live state; two blank placeholders when nothing was fetched yet.
fn claude_rows(state: &AppState) -> Vec<RowDef> {
    if state.claude.rows.is_empty() {
        return vec![
            RowDef { label: "5h".into(), util: None, secs: 0 },
            RowDef { label: "7d".into(), util: None, secs: 0 },
        ];
    }
    state.claude.rows.iter().map(|r| RowDef {
        label: r.label.clone(),
        util:  r.util_or_none(),
        secs:  secs_until(r.reset),
    }).collect()
}

trait UtilOrNone { fn util_or_none(&self) -> Option<f32>; }
impl UtilOrNone for crate::state::UsageRow {
    fn util_or_none(&self) -> Option<f32> { self.utilization }
}

/// Heading meta text + colour for the Claude section.
fn claude_meta(state: &AppState) -> (String, u32) {
    if !state.claude_error.is_empty() {
        let e = state.claude_error.to_ascii_lowercase();
        let short = if e.contains("token expired") || e.contains("sign in") || e.contains("credentials") {
            "token expired"
        } else if e.contains("rate-limited") {
            "rate-limited"
        } else {
            "error"
        };
        return (short.to_string(), WARN_MID);
    }
    let age = fmt_age(state.claude.age_secs());
    if state.claude_stale {
        return (format!("as of {}", age), STALE);
    }
    match state.claude.tier.as_deref() {
        Some(t) if state.claude.fetched_at.is_some() => (format!("{} · {}", t, age), TEXT_DIM),
        Some(t) => (t.to_string(), TEXT_DIM),
        None if state.claude.fetched_at.is_some() => (age, TEXT_DIM),
        None => (String::new(), TEXT_DIM),
    }
}

fn codex_rows(state: &AppState) -> Vec<RowDef> {
    vec![
        RowDef { label: "5h".into(), util: state.codex.utilization_5h, secs: secs_until(state.codex.reset_5h) },
        RowDef { label: "7d".into(), util: state.codex.utilization_7d, secs: secs_until(state.codex.reset_7d) },
    ]
}

fn cursor_rows(state: &AppState) -> Vec<RowDef> {
    let reset_secs = secs_until(state.cursor.reset_date);
    vec![
        RowDef { label: "AUTO".into(), util: state.cursor.auto_usage_pct, secs: reset_secs },
        RowDef { label: "API".into(),  util: state.cursor.api_usage_pct,  secs: reset_secs },
    ]
}

/// Bottom banner text + colour: the first error, else the Claude stale notice.
fn banner_text(state: &AppState) -> Option<(String, u32)> {
    let st = &state.settings;
    if st.show_claude && !state.claude_error.is_empty() {
        return Some((state.claude_error.clone(), WARN_MID));
    }
    if st.show_codex && !state.codex_error.is_empty() {
        return Some((state.codex_error.clone(), WARN_MID));
    }
    if st.show_cursor && !state.cursor_error.is_empty() {
        return Some((state.cursor_error.clone(), WARN_MID));
    }
    if st.show_claude && state.claude_stale {
        let retry = state.claude_next_retry
            .map(|t| fmt_countdown(secs_until(Some(t))))
            .unwrap_or_else(|| "soon".to_string());
        return Some((format!("Claude · usage API rate-limited · retry in {}", retry), TEXT_DIM));
    }
    None
}

/// Header status dot colour.
fn status_dot(state: &AppState) -> u32 {
    let st = &state.settings;
    let any_err = (st.show_claude && !state.claude_error.is_empty())
        || (st.show_codex && !state.codex_error.is_empty())
        || (st.show_cursor && !state.cursor_error.is_empty());
    if any_err { WARN_MID }
    else if st.show_claude && state.claude_stale { STALE }
    else { OK_DOT }
}

/// Bar colours for a row, faded when `dim`.
fn row_colors(util: Option<f32>, hi: u32, mid: u32, lo: u32, dim: bool) -> (u32, u32, u32) {
    let (a, b, c) = bar_colors(util, hi, mid, lo);
    if dim {
        (blend(a, BG, STALE_FADE), blend(b, BG, STALE_FADE), blend(c, BG, STALE_FADE))
    } else {
        (a, b, c)
    }
}

// ─── Theme (mirrored here so the window-region helper can read it without state) ──
static THEME: AtomicU8 = AtomicU8::new(1); // 0 = Classic, 1 = Neon

pub fn set_theme(t: Theme) {
    THEME.store(if t == Theme::Neon { 1 } else { 0 }, Ordering::Relaxed);
}

fn current_theme() -> Theme {
    if THEME.load(Ordering::Relaxed) == 1 { Theme::Neon } else { Theme::Classic }
}

// ─── Window region: rounded 14px for Classic, square for Neon ─────────────────
pub fn apply_rounded_region(hwnd: HWND, w: i32, h: i32) {
    unsafe {
        let r = if current_theme() == Theme::Neon { 0 } else { 14 };
        let hrgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, r, r);
        SetWindowRgn(hwnd, hrgn, true);
    }
}

// ─── Public paint entry ───────────────────────────────────────────────────────
pub fn paint(hwnd: HWND, state: &AppState) {
    debug!(
        "[paint] theme={:?} c5h={:?} c7d={:?} stale={} | x5h={:?} x7d={:?}",
        state.settings.theme,
        state.claude.util("5h"), state.claude.util("7d"), state.claude_stale,
        state.codex.utilization_5h,  state.codex.utilization_7d,
    );
    unsafe {
        match (state.settings.theme, state.settings.display_mode) {
            (Theme::Neon, DisplayMode::AppBar) => paint_appbar_neon(hwnd, state),
            (Theme::Neon, _)                   => paint_floating_neon(hwnd, state),
            (Theme::Classic, DisplayMode::Floating)   => paint_floating(hwnd, state),
            (Theme::Classic, DisplayMode::CompactBar) => paint_compact(hwnd, state),
            (Theme::Classic, DisplayMode::AppBar)     => paint_appbar(hwnd, state),
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
    let fn_mono    = mono_face().to_vec();
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
    gdi_circle(hdc, dot_cx, dot_cy, dot_r, status_dot(state));

    // Header bottom border — 1px #2D3142
    gdi_hline(hdc, 0, header_h, w, DIVIDER);

    // ── Sections: 80px for two rows, +27px per extra row ─────────────────────
    let show_claude = state.settings.show_claude;
    let show_codex  = state.settings.show_codex;
    let show_cursor = state.settings.show_cursor;

    let mut sections: Vec<SectionDef> = Vec::new();
    if show_claude {
        let (meta, meta_col) = claude_meta(state);
        sections.push(SectionDef {
            name: "Claude", hi: CLAUDE_HI, mid: CLAUDE_MID, lo: CLAUDE_LO,
            rows: claude_rows(state),
            meta, meta_col,
            dim: state.claude_stale,
            is_cursor: false,
        });
    }
    if show_codex {
        sections.push(SectionDef {
            name: "Codex", hi: CODEX_HI, mid: CODEX_MID, lo: CODEX_LO,
            rows: codex_rows(state),
            meta: String::new(), meta_col: TEXT_DIM, dim: false,
            is_cursor: false,
        });
    }
    if show_cursor {
        sections.push(SectionDef {
            name: "Cursor", hi: CURSOR_HI, mid: CURSOR_MID, lo: CURSOR_LO,
            rows: cursor_rows(state),
            meta: String::new(), meta_col: TEXT_DIM, dim: false,
            is_cursor: true,
        });
    }

    let divider_gap = DIVIDER_GAP;
    let mut sec_y = header_h;

    for (idx, sec) in sections.iter().enumerate() {
        // Dashed divider before each section except the first
        if idx > 0 {
            let dash = 4i32;
            let gap  = 3i32;
            let pad  = 14i32;
            let div_y = sec_y - divider_gap / 2;
            let mut dx = pad;
            while dx < w - pad {
                let seg = (dx + dash).min(w - pad) - dx;
                gdi_fill(hdc, dx, div_y, seg, 1, DIVIDER);
                dx += dash + gap;
            }
        }

        let section_h = section_height(sec.rows.len());
        draw_float_section(
            hdc, &fn_ui_bold, &fn_mono, fh_head, fh_row,
            0, sec_y, w, section_h,
            sec,
        );

        sec_y += section_h + divider_gap;
    }

    // Bottom banner: first error, else the Claude stale notice.
    if let Some((text, col)) = banner_text(state) {
        let ban_y = h - BOTTOM_PAD - BANNER_H;
        let hfr = make_font_weight(&fn_mono, fh_row + 1, 400);
        let old_f = SelectObject(hdc, hfr);
        SetTextColor(hdc, rgb(col));
        gdi_text(hdc, &text, 14, ban_y, w - 28, BANNER_H,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        SelectObject(hdc, old_f);
        DeleteObject(hfr);
    }

    EndPaint(hwnd, &ps);
}

/// Draw one service section within the floating window.
/// Spec: padding 8px top, 14px left/right, 8px bottom.
/// Layout: heading row (icon + name, 12.5pt/600, brand-light; meta text right-aligned),
///         then N rows: label_w | bar flex | pct 40px right | eta 44px
///         bar height 8px, radius 4px.
///
/// Label column: 16px for "5h"/"7d", 40px when any label is longer (Fable / AUTO / API).
unsafe fn draw_float_section(
    hdc: HDC,
    fn_bold: &[u16], fn_mono: &[u16],
    fh_head: i32, fh_row: i32,
    sx: i32, sy: i32, sw: i32, sh: i32,
    sec: &SectionDef,
) {
    let pad_x   = 14i32;
    let pad_top =  8i32;
    let wide_labels = sec.rows.iter().any(|r| r.label.chars().count() > 2);
    let label_w = if wide_labels { 40i32 } else { 16i32 };
    let pct_w   = 40i32;
    let eta_w   = 44i32;
    let bar_h   =  8i32;
    let bar_r   =  4i32;
    let head_h  = 18i32;
    let row_h   = 22i32;
    let row_gap =  5i32;

    let content_x = sx + pad_x;
    let content_w = sw - pad_x * 2;
    let bar_w = (content_w - label_w - 8 - pct_w - 4 - eta_w).max(20);

    let mut y = sy + pad_top;

    // ── Heading icon ──────────────────────────────────────────────────────────
    let icon_cx = content_x + 5;
    let icon_cy = y + head_h / 2;
    if sec.is_cursor {
        gdi_circle(hdc, icon_cx, icon_cy, 5, sec.hi);
    } else {
        draw_diamond(hdc, icon_cx, icon_cy, 5, sec.hi);
    }

    // ── Heading name + meta ──────────────────────────────────────────────────
    let hfh = make_font_weight(fn_bold, fh_head, 600);
    let old = SelectObject(hdc, hfh);
    SetTextColor(hdc, rgb(sec.hi));
    gdi_text(hdc, sec.name, content_x + 14, y, content_w - 14, head_h,
             DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);
    DeleteObject(hfh);

    if !sec.meta.is_empty() {
        let hfm = make_font_weight(fn_mono, fh_row + 1, 400);
        let old = SelectObject(hdc, hfm);
        SetTextColor(hdc, rgb(sec.meta_col));
        gdi_text(hdc, &sec.meta, content_x + 90, y, content_w - 90, head_h,
                 DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
        SelectObject(hdc, old);
        DeleteObject(hfm);
    }
    y += head_h + 4;

    // ── Data rows — vertically centred in remaining section space ────────────
    let n = sec.rows.len() as i32;
    let rows_total = row_h * n + row_gap * (n - 1).max(0);
    let remaining = sh - (y - sy);
    if remaining > rows_total {
        y += (remaining - rows_total) / 2;
    }

    for r in &sec.rows {
        let bx = content_x + label_w + 8;
        let by = y + (row_h - bar_h) / 2;

        let hfm = make_font_weight(fn_mono, fh_row, 600);
        let old2 = SelectObject(hdc, hfm);
        SetTextColor(hdc, rgb(TEXT_TITLE));
        gdi_text(hdc, &r.label, content_x, y, label_w, row_h,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        // Bar track
        gdi_rrect(hdc, bx, by, bar_w, bar_h, bar_r, BAR_TRACK);
        // Bar fill
        let fw = r.util.map(|u| ((u * bar_w as f32) as i32).max(0).min(bar_w)).unwrap_or(0);
        if fw > 0 {
            let (bhi, bmid, blo) = row_colors(r.util, sec.hi, sec.mid, sec.lo, sec.dim);
            gdi_grad_bar(hdc, bx, by, fw, bar_h, blo, bmid, bhi);
        }

        // Percentage (warn ≥80%)
        let warn = r.util.unwrap_or(0.0) >= 0.80;
        let pct_col = if warn { WARN_HI } else { TEXT_PRIMARY };
        SetTextColor(hdc, rgb(if sec.dim { blend(pct_col, BG, STALE_FADE) } else { pct_col }));
        gdi_text(hdc, &fmt_pct(r.util),
                 bx + bar_w + 4, y, pct_w, row_h,
                 DT_RIGHT | DT_SINGLELINE | DT_VCENTER);

        // ETA / reset countdown
        SetTextColor(hdc, rgb(TEXT_DIM));
        gdi_text(hdc, &fmt_countdown(r.secs),
                 bx + bar_w + 4 + pct_w + 2, y, eta_w, row_h,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        SelectObject(hdc, old2);
        DeleteObject(hfm);
        y += row_h + row_gap;
    }
}

// ─── AppBar dock mode (embedded in taskbar) ───────────────────────────────────
//
// Layout: one column per visible section (Claude 200px, Codex/Cursor 180px).
// Each column: accent stripe (4px) | label | N stacked bars | pct | eta.
// Cursor column uses a light-grey accent stripe (CURSOR_STRIPE).
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
            "[paint_appbar] count={} hwnd={:?} size={}x{} claude={} codex={} cursor={} c5h={:?} x5h={:?} cr={:?}",
            n, hwnd, w, h,
            state.settings.show_claude, state.settings.show_codex, state.settings.show_cursor,
            state.claude.util("5h"), state.codex.utilization_5h,
            state.cursor.auto_usage_pct,
        );
    }

    let scale = (h.max(1) as f32) / 40.0;
    let px_row   = (9.5  * scale).round().clamp(8.0, 18.0) as i32;
    let px_badge = (10.0 * scale).round().clamp(8.0, 19.0) as i32;
    let fh_row   = -px_row;
    let fh_badge = -px_badge;

    let fn_mono = mono_face().to_vec();

    let mem_dc  = CreateCompatibleDC(hdc);
    let mem_bmp = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
    let old_bmp = SelectObject(mem_dc, mem_bmp);
    SetBkMode(mem_dc, TRANSPARENT);

    let clear = sample_taskbar_color(hwnd).unwrap_or(BG);
    gdi_fill(mem_dc, 0, 0, w, h, clear);

    let show_claude = state.settings.show_claude;
    let show_codex  = state.settings.show_codex;
    let show_cursor = state.settings.show_cursor;

    // Column widths follow the settings; the last column absorbs any remainder.
    let widths = appbar_col_widths(&state.settings);
    let mut cols: Vec<(u32, u32, u32, u32, Vec<RowDef>, bool)> = Vec::new(); // hi, mid, lo, stripe, rows, dim
    if show_claude { cols.push((CLAUDE_HI, CLAUDE_MID, CLAUDE_LO, CLAUDE_HI, claude_rows(state), state.claude_stale)); }
    if show_codex  { cols.push((CODEX_HI,  CODEX_MID,  CODEX_LO,  CODEX_HI,  codex_rows(state),  false)); }
    if show_cursor { cols.push((CURSOR_HI, CURSOR_MID, CURSOR_LO, CURSOR_STRIPE, cursor_rows(state), false)); }

    let mut x_off = 0i32;
    let n_cols = cols.len();
    for (i, (hi, mid, lo, stripe, rows, dim)) in cols.iter().enumerate() {
        let is_last = i + 1 == n_cols;
        // Column widths are specified at 96 dpi; the band (and its fonts) scale with DPI.
        let col_w = if is_last {
            (w - x_off).max(1)
        } else {
            (widths.get(i).copied().unwrap_or(180) as f32 * scale).round() as i32
        };
        draw_dock_side(
            mem_dc, &fn_mono, fh_badge, fh_row,
            x_off, 0, col_w, h,
            *hi, *mid, *lo, *stripe,
            rows, *dim,
        );
        x_off += col_w;
        if !is_last {
            gdi_vline(mem_dc, x_off, 0, h, DIVIDER);
        }
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

/// Draw one column of the AppBar dock with N stacked rows.
/// stripe_color: left accent stripe color (brand hi for Claude/Codex, light-grey for Cursor).
/// Two rows use a 6px gap; three or more rows tighten to fit the 40px band (row 5px + gap 8px → 3 rows = 31px).
#[allow(clippy::too_many_arguments)]
unsafe fn draw_dock_side(
    hdc: HDC,
    fn_mono: &[u16],
    _fh_badge: i32, fh_row: i32,
    x: i32, _y: i32, col_w: i32, h: i32,
    hi: u32, mid: u32, lo: u32, stripe_color: u32,
    rows: &[RowDef], dim: bool,
) {
    let scale = (h.max(1) as f32) / 40.0;
    let sc = |v: i32| ((v as f32) * scale).round().max(1.0) as i32;

    let n          = rows.len() as i32;
    let pad        = sc(2);
    let stripe_w   = sc(4);
    let stripe_gap = sc(4);
    // "5h"/"7d" fit in ~26px; "AUTO"/"Fable" (4–5 chars at ~6–9px each) need up to ~46px.
    let wide_labels = rows.iter().any(|r| r.label.chars().count() > 2);
    let label_w    = if wide_labels { sc(30).clamp(30, 46) } else { sc(16).clamp(26, 36) };
    let pct_w      = sc(26).clamp(24, 34);
    let eta_w      = sc(30).clamp(28, 40);
    let bar_h      = sc(5).max(3);
    let row_gap    = if n >= 3 { sc(8) } else { sc(6) };

    // Accent stripe — brand colour (light grey for Cursor)
    gdi_fill(hdc, x, 0, stripe_w, h, stripe_color);

    let rows_area_x = x + pad + stripe_w + stripe_gap;
    let bar_w = (col_w - (rows_area_x - x) - pad - label_w - 5 - pct_w - 2 - eta_w).max(16);

    let total_h = bar_h * n + row_gap * (n - 1).max(0);
    let y_top   = (h - total_h) / 2;
    let bx      = rows_area_x + label_w + 5;

    for (i, r) in rows.iter().enumerate() {
        let row_y = y_top + (bar_h + row_gap) * i as i32;
        let hfm  = make_font_weight(fn_mono, fh_row, 600);
        let old2 = SelectObject(hdc, hfm);

        SetTextColor(hdc, rgb(TEXT_TITLE));
        gdi_text(hdc, &r.label,
                 rows_area_x, row_y - 2, label_w, bar_h + 4,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        gdi_rrect(hdc, bx, row_y, bar_w, bar_h, 2, BAR_TRACK);
        let fw = r.util.map(|u| ((u * bar_w as f32) as i32).max(0).min(bar_w)).unwrap_or(0);
        if fw > 0 {
            let (bhi, bmid, blo) = row_colors(r.util, hi, mid, lo, dim);
            gdi_grad_bar(hdc, bx, row_y, fw, bar_h, blo, bmid, bhi);
        }

        let warn = r.util.unwrap_or(0.0) >= 0.80;
        let pct_col = if warn { WARN_HI } else { TEXT_PRIMARY };
        SetTextColor(hdc, rgb(if dim { blend(pct_col, BG, STALE_FADE) } else { pct_col }));
        gdi_text(hdc, &fmt_pct(r.util),
                 bx + bar_w + 2, row_y - 2, pct_w, bar_h + 4,
                 DT_RIGHT | DT_SINGLELINE | DT_VCENTER);

        SetTextColor(hdc, rgb(TEXT_DIM));
        gdi_text(hdc, &fmt_countdown(r.secs),
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
            &claude_rows(state), state.claude_stale,
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
            &codex_rows(state), false,
            bw, bh);
    }

    EndPaint(hwnd, &ps);
}

/// One half of the compact bar: brand label, then N rows of label | bar | pct | eta
/// stacked on an even pitch so three Claude rows fit the 38px strip.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_compact_half(
    hdc: HDC,
    fn_bold: &[u16], fn_norm: &[u16], fh: i32,
    x: i32, _y: i32, _w: i32, h: i32,
    label: &str, hi: u32, mid: u32, lo: u32,
    rows: &[RowDef], dim: bool,
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

    let n = rows.len().max(1) as i32;
    let label_w = 30i32;
    let lx = x + 24;
    let bx = lx + label_w + 2;
    let pitch = ((h - 4) / n).max(bh + 2);
    let y0 = (h - pitch * n) / 2;

    for (i, r) in rows.iter().enumerate() {
        let ry = y0 + pitch * i as i32;
        let by = ry + (pitch - bh) / 2;

        SetTextColor(hdc, rgb(TEXT_DIM));
        gdi_text(hdc, &r.label, lx, ry, label_w, pitch, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        gdi_rrect(hdc, bx, by, bw, bh, 2, BAR_TRACK);
        let fw = r.util.map(|u| ((u * bw as f32) as i32).max(0).min(bw)).unwrap_or(0);
        if fw > 0 {
            let (bhi, bmid, blo) = row_colors(r.util, hi, mid, lo, dim);
            gdi_grad_bar(hdc, bx, by, fw, bh, blo, bmid, bhi);
        }

        let warn = r.util.unwrap_or(0.0) >= 0.80;
        SetTextColor(hdc, rgb(if warn { WARN_MID } else { TEXT_DIM }));
        gdi_text(hdc, &fmt_pct(r.util), bx + bw + 2, ry, 34, pitch,
                 DT_RIGHT | DT_SINGLELINE | DT_VCENTER);

        SetTextColor(hdc, rgb(TEXT_DIM));
        gdi_text(hdc, &fmt_countdown(r.secs), bx + bw + 38, ry, 46, pitch,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    }

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
    // wstr() appends a NUL; DrawTextW takes a length-delimited slice, so drop it —
    // otherwise fonts with a visible .notdef glyph (Bahnschrift) paint a box after the text.
    let mut ws = wstr(text);
    let n = ws.len().saturating_sub(1);
    let mut rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
    DrawTextW(hdc, &mut ws[..n], &mut rc, flags);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Neon HUD theme (v0.5) — 8px pixel grid, segmented glowing bars, cyan structure.
//
// Floating (240 × auto):
//   ┌─┐ USAGE//MONITOR                      ⬡ LIVE ┌─┐   header 24
//   │ ▍CLAUDE                          MAX 5X · NOW│   section: pad 8 + head 16 + rows×16 + pad 8
//   │  5H     ▮▮▮▮▮▮▮▮▮▮▮▮▯▯▯▯   78%  4h23m        │   row 16: label 32 | bar 80 | pct 32 | eta 40
//   │  7D     ▮▮▮▮▯▯▯▯▯▯▯▯▯▯▯▯   22%  5d1h         │
//   │  FABLE  ▮▮▮▮▮▮▮▯▯▯▯▯▯▯▯▯   41%  3d12h        │
//   │ ──── ─────────────────────────────────────── │   divider gap 8 (line centred, cyan tick)
//   └─┘                                          └─┘
//
// AppBar (40 tall, 176px columns): full-height glowing brand stripe 4px | rows 9px.
// ═══════════════════════════════════════════════════════════════════════════════
mod neon {
    pub const BG:       u32 = 0x070A12;
    pub const GRID:     u32 = 0x0C1822;
    pub const LINE:     u32 = 0x143241;
    pub const CYAN:     u32 = 0x4FE3FF;
    pub const CYAN_DIM: u32 = 0x1E6F82;
    pub const TEXT:     u32 = 0xD6E6F2;
    pub const DIM:      u32 = 0x6B8A9C;
    pub const LABEL:    u32 = 0x9FB4C4;
    pub const OFF:      u32 = 0x10202B;
    pub const WARN:     u32 = 0xFF2E63;
    pub const STALE:    u32 = 0xFFB454;
    pub const PCT:      u32 = 0xE8F4FF;
    pub const BAR_BG:   u32 = 0x0B0E16;
    /// (lo, hi) brand gradient ends.
    pub const CLAUDE: (u32, u32) = (0xFF6A2A, 0xFFB454);
    pub const CODEX:  (u32, u32) = (0x5B7CFF, 0x9AB0FF);
    pub const CURSOR: (u32, u32) = (0xB9C7D6, 0xEAF2FA);
    /// How far stale bars fade towards the background.
    pub const STALE_FADE: f32 = 0.55;
    pub const FONT_DISPLAY: &str = "Bahnschrift";
    pub const FONT_MONO:    &str = "JetBrains Mono";
}

/// Text with a 1px halo in a blended colour (GDI "glow"). The halo must stay faint
/// (≈80% towards the background) or small counters (E, O, D at 12px) fill in.
unsafe fn glow_text(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32,
                    flags: DRAW_TEXT_FORMAT, color: u32, bg: u32) {
    let halo = blend(color, bg, 0.80);
    SetTextColor(hdc, rgb(halo));
    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        gdi_text(hdc, text, x + dx, y + dy, w, h, flags);
    }
    SetTextColor(hdc, rgb(color));
    gdi_text(hdc, text, x, y, w, h, flags);
}

/// Solid rectangle with a 2-step halo.
unsafe fn glow_fill(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: u32, bg: u32) {
    gdi_fill(hdc, x - 2, y - 2, w + 4, h + 4, blend(color, bg, 0.84));
    gdi_fill(hdc, x - 1, y - 1, w + 2, h + 2, blend(color, bg, 0.60));
    gdi_fill(hdc, x, y, w, h, color);
}

/// Vertical gradient fill, `top` → `bottom`.
unsafe fn neon_vgrad(hdc: HDC, x: i32, y: i32, w: i32, h: i32, top: u32, bottom: u32) {
    for r in 0..h {
        let t = if h > 1 { r as f32 / (h - 1) as f32 } else { 0.0 };
        gdi_fill(hdc, x, y + r, w, 1, blend(top, bottom, t));
    }
}

/// Segmented bar: `seg_w`-wide blocks with `gap` between, as many as fit `bar_w`.
/// Lit blocks carry the brand gradient and a halo; unlit blocks are OFF.
#[allow(clippy::too_many_arguments)]
unsafe fn neon_seg_bar(hdc: HDC, x: i32, y: i32, bar_w: i32, h: i32, seg_w: i32, gap: i32,
                       util: Option<f32>, lo: u32, hi: u32, bg: u32, dim: bool) {
    let n = ((bar_w + gap) / (seg_w + gap)).max(1);
    let lit = util.map(|u| (u * n as f32).round() as i32).unwrap_or(0).clamp(0, n);
    let (lo, hi) = if dim {
        (blend(lo, bg, neon::STALE_FADE), blend(hi, bg, neon::STALE_FADE))
    } else {
        (lo, hi)
    };
    if !dim {
        let halo = blend(hi, bg, 0.62);
        for i in 0..lit {
            let sx = x + i * (seg_w + gap);
            gdi_fill(hdc, sx - 1, y - 1, seg_w + 2, h + 2, halo);
        }
    }
    for i in 0..n {
        let sx = x + i * (seg_w + gap);
        if i < lit {
            neon_vgrad(hdc, sx, y, seg_w, h, hi, lo);
        } else {
            gdi_fill(hdc, sx, y, seg_w, h, neon::OFF);
        }
    }
}

/// Hexagon status glyph (outline + centre dot).
unsafe fn draw_hex(hdc: HDC, cx: i32, cy: i32, r: i32, color: u32, bg: u32) {
    let pts = |rr: i32| -> [POINT; 6] {
        let dx = (rr as f32 * 0.866) as i32;
        let dy = rr / 2;
        [
            POINT { x: cx,      y: cy - rr },
            POINT { x: cx + dx, y: cy - dy },
            POINT { x: cx + dx, y: cy + dy },
            POINT { x: cx,      y: cy + rr },
            POINT { x: cx - dx, y: cy + dy },
            POINT { x: cx - dx, y: cy - dy },
        ]
    };
    for (rr, col) in [(r + 1, blend(color, bg, 0.7)), (r, color), (r - 2, bg)] {
        if rr <= 0 { continue; }
        let b = CreateSolidBrush(rgb(col));
        let p = CreatePen(PS_NULL, 0, rgb(col));
        let ob = SelectObject(hdc, b);
        let op = SelectObject(hdc, p);
        let _ = Polygon(hdc, &pts(rr));
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        DeleteObject(b);
        DeleteObject(p);
    }
    gdi_fill(hdc, cx - 1, cy - 1, 3, 3, color);
}

/// Corner bracket: 8px L-shape, 2px thick, with halo. `sx`/`sy` = ±1 direction.
unsafe fn neon_corner(hdc: HDC, x: i32, y: i32, sx: i32, sy: i32) {
    let halo = blend(neon::CYAN, neon::BG, 0.66);
    let rect = |len: i32, thick: i32, horiz: bool| -> (i32, i32, i32, i32) {
        let (w, h) = if horiz { (len, thick) } else { (thick, len) };
        let x0 = if sx > 0 { x } else { x - w + 1 };
        let y0 = if sy > 0 { y } else { y - h + 1 };
        (x0, y0, w, h)
    };
    for (len, thick, col) in [(9, 3, halo), (8, 2, neon::CYAN)] {
        let (a, b, c, d) = rect(len, thick, true);
        gdi_fill(hdc, a, b, c, d, col);
        let (a, b, c, d) = rect(len, thick, false);
        gdi_fill(hdc, a, b, c, d, col);
    }
}

fn neon_brand(name: &str) -> (u32, u32) {
    match name {
        "Claude" => neon::CLAUDE,
        "Codex"  => neon::CODEX,
        _        => neon::CURSOR,
    }
}

/// Map the classic meta colour onto the neon palette.
fn neon_meta_color(classic: u32) -> u32 {
    match classic {
        WARN_MID => neon::WARN,
        STALE    => neon::STALE,
        _        => neon::DIM,
    }
}

/// Status chip text + colour for the header.
fn neon_status(state: &AppState) -> (&'static str, u32) {
    let st = &state.settings;
    let any_err = (st.show_claude && !state.claude_error.is_empty())
        || (st.show_codex && !state.codex_error.is_empty())
        || (st.show_cursor && !state.cursor_error.is_empty());
    if any_err { ("ERR", neon::WARN) }
    else if st.show_claude && state.claude_stale { ("STALE", neon::STALE) }
    else { ("LIVE", neon::CYAN) }
}

/// Short uppercase banner for the neon footer.
fn neon_banner(state: &AppState) -> Option<(String, u32)> {
    let (text, col) = banner_text(state)?;
    let col = neon_meta_color(col);
    if state.claude_stale && col == neon::DIM {
        let retry = state.claude_next_retry
            .map(|t| fmt_countdown(secs_until(Some(t))))
            .unwrap_or_else(|| "soon".to_string());
        return Some((format!("CLAUDE · RATE-LIMITED · RETRY {}", retry.to_uppercase()), neon::STALE));
    }
    // Errors: drop the parenthesised detail, uppercase the rest.
    let short = text.split(" (").next().unwrap_or(&text).replace(':', " ·");
    Some((short.to_uppercase(), col))
}

fn neon_sections(state: &AppState) -> Vec<SectionDef> {
    let mut sections = Vec::new();
    let st = &state.settings;
    if st.show_claude {
        let (meta, meta_col) = claude_meta(state);
        let (lo, hi) = neon::CLAUDE;
        sections.push(SectionDef {
            name: "Claude", hi, mid: hi, lo,
            rows: claude_rows(state),
            meta, meta_col: neon_meta_color(meta_col),
            dim: state.claude_stale, is_cursor: false,
        });
    }
    if st.show_codex {
        let (lo, hi) = neon::CODEX;
        sections.push(SectionDef {
            name: "Codex", hi, mid: hi, lo, rows: codex_rows(state),
            meta: String::new(), meta_col: neon::DIM, dim: false, is_cursor: false,
        });
    }
    if st.show_cursor {
        let (lo, hi) = neon::CURSOR;
        sections.push(SectionDef {
            name: "Cursor", hi, mid: hi, lo, rows: cursor_rows(state),
            meta: String::new(), meta_col: neon::DIM, dim: false, is_cursor: true,
        });
    }
    sections
}

// ─── Neon floating window ─────────────────────────────────────────────────────
unsafe fn paint_floating_neon(hwnd: HWND, state: &AppState) {
    use neon::*;
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    GetClientRect(hwnd, &mut rc).ok();
    let w = rc.right;
    let h = rc.bottom;

    let fn_mono = mono_face().to_vec();
    let fn_disp = display_face().to_vec();
    SetBkMode(hdc, TRANSPARENT);

    // ── Base: background, scanlines every 4px, dot grid every 8px ────────────
    gdi_fill(hdc, 0, 0, w, h, BG);
    let scan = blend(BG, 0x000000, 0.10);
    let mut y = 0;
    while y < h { gdi_fill(hdc, 0, y, w, 1, scan); y += 4; }
    let mut gy = 0;
    while gy < h {
        let mut gx = 0;
        while gx < w { gdi_fill(hdc, gx, gy, 1, 1, GRID); gx += 8; }
        gy += 8;
    }

    // ── Header: cyan wash fading to the right, 1px base line ─────────────────
    let hh = NEON_HEADER_H;
    gdi_grad_bar(hdc, 0, 0, w * 6 / 10, hh - 1, blend(BG, CYAN, 0.10), blend(BG, CYAN, 0.05), BG);
    gdi_hline(hdc, 0, hh - 1, w, LINE);

    let hft = make_font_weight(&fn_disp, -13, 700);
    let old = SelectObject(hdc, hft);
    SetTextCharacterExtra(hdc, 2);
    glow_text(hdc, "USAGE//MONITOR", 16, 0, w - 90, hh - 1,
              DT_LEFT | DT_SINGLELINE | DT_VCENTER, CYAN, BG);
    SetTextCharacterExtra(hdc, 0);
    SelectObject(hdc, old);
    DeleteObject(hft);

    // Status chip (hex glyph + LIVE / STALE / ERR), right-aligned at the 16px pad.
    let (chip, chip_col) = neon_status(state);
    // No character extra here: DrawText right-aligns without it but paints with it,
    // which clipped the last glyph ("LIVI").
    let hfc = make_font_weight(&fn_mono, -9, 600);
    let old = SelectObject(hdc, hfc);
    let chip_w = 40;
    glow_text(hdc, chip, w - 16 - chip_w, 0, chip_w, hh - 1,
              DT_RIGHT | DT_SINGLELINE | DT_VCENTER, chip_col, BG);
    SelectObject(hdc, old);
    DeleteObject(hfc);
    draw_hex(hdc, w - 16 - chip_w - 10, (hh - 1) / 2, 5, chip_col, BG);

    // ── Frame: 1px border + four glowing corner brackets ─────────────────────
    gdi_hline(hdc, 0, 0, w, LINE);
    gdi_hline(hdc, 0, h - 1, w, LINE);
    gdi_vline(hdc, 0, 0, h, LINE);
    gdi_vline(hdc, w - 1, 0, h, LINE);
    neon_corner(hdc, 0,     0,     1,  1);
    neon_corner(hdc, w - 1, 0,     -1, 1);
    neon_corner(hdc, 0,     h - 1, 1,  -1);
    neon_corner(hdc, w - 1, h - 1, -1, -1);

    // ── Sections ─────────────────────────────────────────────────────────────
    let sections = neon_sections(state);
    let mut sy = hh;
    for (idx, sec) in sections.iter().enumerate() {
        if idx > 0 {
            let dy = sy - NEON_DIVIDER_GAP / 2;
            gdi_hline(hdc, 16, dy, w - 32, LINE);
            gdi_fill(hdc, 16, dy - 1, 16, 3, blend(CYAN, BG, 0.7));
            gdi_fill(hdc, 16, dy, 16, 1, CYAN);
        }
        let sec_h = neon_section_height(sec.rows.len());
        draw_neon_section(hdc, &fn_disp, &fn_mono, 0, sy, w, sec);
        sy += sec_h + NEON_DIVIDER_GAP;
    }

    // ── Banner ───────────────────────────────────────────────────────────────
    if let Some((text, col)) = neon_banner(state) {
        let by = h - NEON_BOTTOM_PAD - NEON_BANNER_H;
        draw_diamond(hdc, 16 + 3, by + NEON_BANNER_H / 2, 3, col);
        let hfb = make_font_weight(&fn_mono, -9, 600);
        let old = SelectObject(hdc, hfb);
        glow_text(hdc, &text, 28, by, w - 44, NEON_BANNER_H,
                  DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS, col, BG);
        SelectObject(hdc, old);
        DeleteObject(hfb);
    }

    EndPaint(hwnd, &ps);
}

/// One neon section: glowing brand stripe + uppercase name + meta, then N 16px rows.
unsafe fn draw_neon_section(hdc: HDC, fn_disp: &[u16], fn_mono: &[u16],
                            sx: i32, sy: i32, sw: i32, sec: &SectionDef) {
    use neon::*;
    let pad = 16;
    let x0 = sx + pad;
    let cw = sw - pad * 2;
    let mut y = sy + NEON_SEC_PAD;

    // Heading
    glow_fill(hdc, x0, y + 2, 4, 12, sec.hi, BG);
    let hfh = make_font_weight(fn_disp, -12, 600);
    let old = SelectObject(hdc, hfh);
    SetTextCharacterExtra(hdc, 1);
    glow_text(hdc, &sec.name.to_uppercase(), x0 + 12, y, cw - 12 - 96, NEON_HEAD_H,
              DT_LEFT | DT_SINGLELINE | DT_VCENTER, sec.hi, BG);
    SetTextCharacterExtra(hdc, 0);
    SelectObject(hdc, old);
    DeleteObject(hfh);

    if !sec.meta.is_empty() {
        let hfm = make_font_weight(fn_mono, -9, 400);
        let old = SelectObject(hdc, hfm);
        SetTextColor(hdc, rgb(sec.meta_col));
        gdi_text(hdc, &sec.meta.to_uppercase(), x0 + cw - 110, y, 110, NEON_HEAD_H,
                 DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
        SelectObject(hdc, old);
        DeleteObject(hfm);
    }
    y += NEON_HEAD_H;

    // Rows: label 32 | bar (flex, 80 at 240 wide) | pct 32 | eta 40, gaps 8
    let label_w = 32;
    let pct_w   = 32;
    let eta_w   = 40;
    let gap     = 8;
    let bar_w   = (cw - label_w - gap - gap - pct_w - gap - eta_w).max(20);
    let bar_x   = x0 + label_w + gap;
    let pct_x   = bar_x + bar_w + gap;
    let eta_x   = pct_x + pct_w + gap;

    let hfl = make_font_weight(fn_mono, -10, 400);
    let hfp = make_font_weight(fn_mono, -11, 600);
    let hfe = make_font_weight(fn_mono, -9, 400);

    for r in &sec.rows {
        let old = SelectObject(hdc, hfl);
        SetTextColor(hdc, rgb(if sec.dim { blend(LABEL, BG, 0.35) } else { LABEL }));
        gdi_text(hdc, &r.label.to_uppercase(), x0, y, label_w, NEON_ROW_H,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        let warn = r.util.unwrap_or(0.0) >= 0.80;
        let (lo, hi) = if warn { (blend(WARN, BG, 0.3), WARN) } else { (sec.lo, sec.hi) };
        neon_seg_bar(hdc, bar_x, y + (NEON_ROW_H - 6) / 2, bar_w, 6, 4, 1, r.util, lo, hi, BG, sec.dim);

        SelectObject(hdc, hfp);
        let pct_col = if warn { WARN } else { PCT };
        if sec.dim {
            SetTextColor(hdc, rgb(blend(pct_col, BG, 0.4)));
            gdi_text(hdc, &fmt_pct(r.util), pct_x, y, pct_w, NEON_ROW_H,
                     DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
        } else {
            glow_text(hdc, &fmt_pct(r.util), pct_x, y, pct_w, NEON_ROW_H,
                      DT_RIGHT | DT_SINGLELINE | DT_VCENTER, pct_col, BG);
        }

        SelectObject(hdc, hfe);
        SetTextColor(hdc, rgb(DIM));
        gdi_text(hdc, &fmt_countdown(r.secs), eta_x, y, eta_w, NEON_ROW_H,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        SelectObject(hdc, old);
        y += NEON_ROW_H;
    }
    DeleteObject(hfl);
    DeleteObject(hfp);
    DeleteObject(hfe);
}

// ─── Neon AppBar (taskbar band) ───────────────────────────────────────────────
unsafe fn paint_appbar_neon(hwnd: HWND, state: &AppState) {
    use neon::*;
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    GetClientRect(hwnd, &mut rc).ok();
    let w = rc.right;
    let h = rc.bottom;

    let scale = (h.max(1) as f32) / 40.0;
    let fn_mono = mono_face().to_vec();

    let mem_dc  = CreateCompatibleDC(hdc);
    let mem_bmp = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
    let old_bmp = SelectObject(mem_dc, mem_bmp);
    SetBkMode(mem_dc, TRANSPARENT);

    // Transparent plate: paint the sampled taskbar colour so the band blends in.
    let bg = sample_taskbar_color(hwnd).unwrap_or(BAR_BG);
    gdi_fill(mem_dc, 0, 0, w, h, bg);

    let widths = appbar_col_widths(&state.settings);
    let sections = neon_sections(state);
    let n_cols = sections.len();
    let mut x_off = 0i32;
    for (i, sec) in sections.iter().enumerate() {
        let is_last = i + 1 == n_cols;
        let col_w = if is_last {
            (w - x_off).max(1)
        } else {
            (widths.get(i).copied().unwrap_or(176) as f32 * scale).round() as i32
        };
        draw_neon_dock_col(mem_dc, &fn_mono, x_off, col_w, h, sec, bg);
        x_off += col_w;
    }

    let _ = BitBlt(hdc, 0, 0, w, h, mem_dc, 0, 0, SRCCOPY);
    SelectObject(mem_dc, old_bmp);
    DeleteObject(mem_bmp);
    DeleteDC(mem_dc);
    EndPaint(hwnd, &ps);
}

/// One AppBar column: full-height glowing brand stripe (also the divider) + stacked rows.
/// Row anatomy at 96 dpi: label 28 | 14-segment bar 55 | pct 28 | eta 32, gap 4; rows 9px.
unsafe fn draw_neon_dock_col(hdc: HDC, fn_mono: &[u16], x: i32, col_w: i32, h: i32,
                             sec: &SectionDef, bg: u32) {
    use neon::*;
    let scale = (h.max(1) as f32) / 40.0;
    let sc = |v: i32| ((v as f32) * scale).round().max(1.0) as i32;

    // Stripe with a two-step halo on its right.
    let stripe_w = sc(4);
    gdi_fill(hdc, x + stripe_w, 0, sc(2), h, blend(sec.hi, bg, 0.72));
    gdi_fill(hdc, x + stripe_w + sc(2), 0, sc(1), h, blend(sec.hi, bg, 0.88));
    neon_vgrad(hdc, x, 0, stripe_w, h, sec.hi, sec.lo);

    let label_w = sc(28);
    let pct_w   = sc(28);
    let eta_w   = sc(32);
    let gap     = sc(4);
    let row_h   = sc(9);
    let bar_h   = sc(5);
    let seg_w   = sc(3);
    let seg_gap = sc(1);
    let n       = sec.rows.len() as i32;
    let rgap    = if n >= 3 { sc(3) } else { sc(7) };

    let rx    = x + stripe_w + sc(9);
    let right = x + col_w - sc(8);
    let bar_x = rx + label_w + gap;
    let bar_w = (right - bar_x - gap - pct_w - gap - eta_w).max(sc(16));
    let pct_x = bar_x + bar_w + gap;
    let eta_x = pct_x + pct_w + gap;

    let total = n * row_h + (n - 1).max(0) * rgap;
    let mut y = (h - total) / 2;

    let hfl = make_font_weight(fn_mono, -sc(9), 400);
    let hfp = make_font_weight(fn_mono, -sc(9), 600);
    let hfe = make_font_weight(fn_mono, -sc(8), 400);

    for r in &sec.rows {
        let old = SelectObject(hdc, hfl);
        SetTextColor(hdc, rgb(if sec.dim { blend(LABEL, bg, 0.35) } else { LABEL }));
        gdi_text(hdc, &r.label.to_uppercase(), rx, y - 1, label_w, row_h + 2,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);

        let warn = r.util.unwrap_or(0.0) >= 0.80;
        let (lo, hi) = if warn { (blend(WARN, bg, 0.3), WARN) } else { (sec.lo, sec.hi) };
        neon_seg_bar(hdc, bar_x, y + (row_h - bar_h) / 2, bar_w, bar_h, seg_w, seg_gap,
                     r.util, lo, hi, bg, sec.dim);

        SelectObject(hdc, hfp);
        let pct_col = if warn { WARN } else { PCT };
        if sec.dim {
            SetTextColor(hdc, rgb(blend(pct_col, bg, 0.4)));
            gdi_text(hdc, &fmt_pct(r.util), pct_x, y - 1, pct_w, row_h + 2,
                     DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
        } else {
            glow_text(hdc, &fmt_pct(r.util), pct_x, y - 1, pct_w, row_h + 2,
                      DT_RIGHT | DT_SINGLELINE | DT_VCENTER, pct_col, bg);
        }

        SelectObject(hdc, hfe);
        SetTextColor(hdc, rgb(DIM));
        gdi_text(hdc, &fmt_countdown(r.secs), eta_x, y - 1, eta_w, row_h + 2,
                 DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        SelectObject(hdc, old);
        y += row_h + rgap;
    }
    DeleteObject(hfl);
    DeleteObject(hfp);
    DeleteObject(hfe);
}

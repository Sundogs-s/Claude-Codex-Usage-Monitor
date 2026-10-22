//! Compact taskbar summary with measured typography, separate from quota rows.
use super::*;
use windows::Win32::{Foundation::SIZE, Graphics::Gdi::GetTextExtentPoint32W};

#[derive(Clone)]
pub(super) struct DockSummary {
    tokens: String,
    date: String,
    credits: String,
}

pub(super) fn dock_rows(rows: &[RowDef]) -> Vec<RowDef> {
    let token = rows.iter().position(|r| matches!(r.label.as_str(), "DAY" | "LAST") && r.text.is_some());
    let credit = rows.iter().position(|r| r.label == "CR" && r.text.is_some());
    let (Some(t), Some(c)) = (token, credit) else { return rows.to_vec(); };
    let value = rows[t].text.as_deref().unwrap_or_default();
    let (tokens, date) = match value.split_once(" tok") {
        Some((tokens, date)) => (tokens.to_owned(), if date.trim().is_empty() { "today".into() }
            else { date.trim().replace('-', "/") }),
        None => ("--".into(), "no data".into()),
    };
    let credits = rows[c].text.as_deref().unwrap_or_default();
    let credits = if credits.contains('.') { credits.trim_end_matches('0').trim_end_matches('.') } else { credits };
    rows.iter().enumerate().filter(|(i, _)| *i != c).map(|(i, r)| {
        let mut r = r.clone();
        if i == t {
            r.label.clear();
            r.summary = Some(DockSummary { tokens: tokens.clone(), date: date.clone(), credits: credits.into() });
        }
        r
    }).collect()
}

struct Fragment { text: String, px: i32, weight: i32, color: u32 }

/// Coordinates shared with the quota painter; never estimate alignment from text.
#[derive(Clone, Copy)]
pub(super) struct DockColumns {
    pub bar_x: i32,
    pub bar_w: i32,
    pub pct_x: i32,
    pub pct_w: i32,
    pub eta_x: i32,
    pub eta_w: i32,
}

unsafe fn measure(hdc: HDC, font: &[u16], part: &Fragment, scale: f32) -> i32 {
    let hf = make_font_weight(font, -((part.px as f32 * scale).round() as i32), part.weight);
    let old = SelectObject(hdc, hf);
    let text: Vec<u16> = part.text.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &text, &mut size);
    SelectObject(hdc, old);
    let _ = DeleteObject(hf);
    size.cx
}

unsafe fn paint_part(hdc: HDC, font: &[u16], part: &Fragment,
    x: i32, y: i32, width: i32, height: i32, scale: f32, flags: DRAW_TEXT_FORMAT) {
    let hf = make_font_weight(font, -((part.px as f32 * scale).round() as i32), part.weight);
    let old = SelectObject(hdc, hf);
    SetTextColor(hdc, rgb(part.color));
    gdi_text(hdc, &part.text, x, y, width.max(0), height,
        flags | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);
    let _ = DeleteObject(hf);
}

unsafe fn draw_group(hdc: HDC, font: &[u16], value: &str, unit: &str,
    x: i32, y: i32, width: i32, height: i32, scale: f32,
    main: i32, weight: i32, color: u32, muted: u32, right: bool) {
    let gap = (2.0 * scale).round().max(1.0) as i32;
    let unit = Fragment { text: unit.into(), px: 8, weight: 400, color: muted };
    let unit_w = measure(hdc, font, &unit, scale);
    let mut number = Fragment { text: value.into(), px: main, weight, color };
    let mut number_w = 0;
    for px in (main - 2..=main).rev() {
        number.px = px;
        number_w = measure(hdc, font, &number, scale);
        if number_w + gap + unit_w <= width { break; }
    }
    let total = number_w + gap + unit_w;
    if total > width {
        // Exceptional long values stay inside their column with visible truncation.
        number.text = format!("{} {}", value, unit.text);
        paint_part(hdc, font, &number, x, y, width, height, scale,
            (if right { DT_RIGHT } else { DT_LEFT }) | DT_END_ELLIPSIS);
        return;
    }
    let start = if right { x + width - total } else { x };
    paint_part(hdc, font, &number, start, y, number_w, height, scale, DT_LEFT);
    paint_part(hdc, font, &unit, start + number_w + gap, y, unit_w, height, scale, DT_LEFT);
}

pub(super) unsafe fn draw(hdc: HDC, font: &[u16], s: &DockSummary,
    columns: DockColumns, y: i32, height: i32, scale: f32, dim: bool, bg: u32) {
    let primary = if dim { blend(TEXT_PRIMARY, bg, 0.4) } else { TEXT_PRIMARY };
    let secondary = if dim { blend(0xA9BDCE, bg, 0.4) } else { 0xA9BDCE };
    let muted = if dim { blend(TEXT_DIM, bg, 0.4) } else { TEXT_DIM };
    draw_group(hdc, font, &s.tokens, "tok", columns.bar_x, y, columns.bar_w,
        height, scale, 12, 700, primary, muted, false);
    draw_group(hdc, font, &s.credits, "cr", columns.pct_x, y, columns.pct_w,
        height, scale, 11, 600, secondary, muted, true);
    let date = Fragment { text: s.date.clone(), px: 8, weight: 400, color: muted };
    paint_part(hdc, font, &date, columns.eta_x, y, columns.eta_w, height, scale,
        DT_RIGHT | DT_END_ELLIPSIS);
}

pub(super) unsafe fn draw_classic_dock(hdc: HDC, font: &[u16], rows: &[RowDef],
    x: i32, width: i32, height: i32, hi: u32, mid: u32, lo: u32, stripe: u32, dim: bool) {
    let scale = height.max(1) as f32 / 40.0;
    let sc = |n: i32| ((n as f32 * scale).round() as i32).max(1);
    let left = x + sc(10);
    let right = x + width - sc(5);
    let row_h = sc(14);
    let pitch = sc(18);
    let top = (height - row_h - pitch * (rows.len() as i32 - 1)) / 2;
    let bx = left + sc(25);
    let bw = (right - bx - sc(76)).max(sc(16));
    let columns = DockColumns { bar_x: bx, bar_w: bw,
        pct_x: bx + bw + sc(3), pct_w: sc(38),
        eta_x: bx + bw + sc(44), eta_w: sc(32) };
    gdi_fill(hdc, x, 0, sc(4), height, stripe);
    for (i, r) in rows.iter().enumerate() {
        let y = top + i as i32 * pitch;
        if let Some(s) = &r.summary { draw(hdc, font, s, columns, y, row_h, scale, dim, BG); continue; }
        let small = make_font_weight(font, -sc(9), 400);
        let bold = make_font_weight(font, -sc(12), 700);
        let old = SelectObject(hdc, small);
        SetTextColor(hdc, rgb(TEXT_DIM));
        gdi_text(hdc, &r.label, left, y, sc(22), row_h, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        let by = y + (row_h - sc(5)) / 2;
        gdi_rrect(hdc, bx, by, bw, sc(5), 2, BAR_TRACK);
        let fw = (r.util.unwrap_or(0.0) * bw as f32) as i32;
        if fw > 0 { let (a,b,c) = row_colors(r.util, hi, mid, lo, dim); gdi_grad_bar(hdc, bx, by, fw, sc(5), c, b, a); }
        SelectObject(hdc, bold);
        let color = if r.util.unwrap_or(0.0) >= 0.8 { WARN_HI } else { TEXT_PRIMARY };
        SetTextColor(hdc, rgb(if dim { blend(color, BG, 0.4) } else { color }));
        gdi_text(hdc, &fmt_pct(r.util), columns.pct_x, y, columns.pct_w, row_h, DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
        SelectObject(hdc, small);
        SetTextColor(hdc, rgb(TEXT_DIM));
        gdi_text(hdc, &fmt_countdown(r.secs), columns.eta_x, y, columns.eta_w, row_h, DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
        SelectObject(hdc, old);
        let _ = DeleteObject(small); let _ = DeleteObject(bold);
    }
}

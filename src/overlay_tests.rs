//! Offscreen GDI integration checks; never captures the user's desktop.
use super::*;
use crate::state::{CodexUsage, CodexWindow, DailyTokens};
use windows::Win32::Graphics::Gdi::{CreateDIBSection, GdiFlush, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};

#[test]
fn codex_numeric_rows_render_in_both_themes() {
    for scenario in ["pro", "plus", "empty", "stale"] {
        for theme in [Theme::Neon, Theme::Classic] {
            for dock in [false, true] {
                let mut state = AppState::default();
                state.settings.show_claude = false;
                state.settings.show_cursor = false;
                state.settings.theme = theme;
                if scenario != "empty" {
                    let mut windows = vec![CodexWindow {
                        label: "7d".into(), minutes: 10080, utilization: 0.07,
                        reset: Some(chrono::Utc::now() + chrono::Duration::days(5)),
                    }];
                    if scenario == "plus" { windows.insert(0, CodexWindow {
                        label: "5h".into(), minutes: 300, utilization: 0.42,
                        reset: Some(chrono::Utc::now() + chrono::Duration::hours(2)),
                    }); }
                    state.codex = CodexUsage {
                        plan: Some(if scenario == "plus" { "plus" } else { "prolite" }.into()),
                        windows, credits: Some("100.00".into()), reset_count: Some(3),
                        daily: Some(vec![DailyTokens { date: "2020-10-09".into(), tokens: 25_562_024 }]),
                        fetched_at: Some(chrono::Utc::now()), ..Default::default()
                    };
                }
                if scenario == "stale" { state.codex_error = "Codex: network error".into(); }
                let width = match (theme, dock) {
                    (Theme::Neon, false) => 240, (Theme::Classic, false) => 320,
                    (Theme::Neon, true) => 176, (Theme::Classic, true) => 180,
                    (Theme::ChoseWay,_)=>unreachable!(),
                };
                let height = if dock { 40 } else { crate::state::calc_window_height(&state.settings, state.layout()) };
                let rows = codex_rows(&state);
                assert!((2..=3).contains(&rows.len()));
                unsafe {
                    let dc = CreateCompatibleDC(None);
                    assert!(!dc.is_invalid());
                    let info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: width, biHeight: -height, biPlanes: 1, biBitCount: 32,
                        biCompression: BI_RGB.0, ..Default::default()
                    }, ..Default::default() };
                    let mut bits = std::ptr::null_mut();
                    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
                    let old = SelectObject(dc, bitmap);
                    SetBkMode(dc, TRANSPARENT);
                    gdi_fill(dc, 0, 0, width, height, BG);
                    let mono = mono_face().to_vec();
                    let sec = SectionDef { name: "Codex", hi: CODEX_HI, mid: CODEX_MID, lo: CODEX_LO,
                        rows, meta: codex_meta(&state), meta_col: TEXT_DIM,
                        dim: scenario == "stale", is_cursor: false };
                    match (theme, dock) {
                        (Theme::Neon, false) => draw_neon_section(dc, display_face(), &mono, 0, 24, width, &sec),
                        (Theme::Neon, true) => draw_neon_dock_col(dc, &mono, 0, width, height, &sec, BG),
                        (Theme::Classic, false) => draw_float_section(dc, &wstr("Segoe UI Semibold"), &mono,
                            -16, -12, 0, 28, width, section_height(sec.rows.len()), &sec),
                        (Theme::Classic, true) => draw_dock_side(dc, &mono, -10, -10, 0, 0,
                            width, height, sec.hi, sec.mid, sec.lo, sec.hi, &sec.rows, sec.dim),
                        (Theme::ChoseWay,_)=>unreachable!(),
                    }
                    let _ = GdiFlush();
                    let pixels = std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize);
                    assert!(pixels.chunks_exact(4).filter(|p| p[0] != 0x17 || p[1] != 0x11 || p[2] != 0x0f).count() > 100);
                    if let Some(dir) = std::env::var_os("USAGE_MONITOR_PREVIEW_DIR") {
                        let dir = std::path::PathBuf::from(dir);
                        std::fs::create_dir_all(&dir).unwrap();
                        let mut bmp = vec![0u8; 54];
                        bmp[0..2].copy_from_slice(b"BM");
                        bmp[2..6].copy_from_slice(&(54u32 + pixels.len() as u32).to_le_bytes());
                        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
                        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
                        bmp[18..22].copy_from_slice(&width.to_le_bytes());
                        bmp[22..26].copy_from_slice(&(-height).to_le_bytes());
                        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
                        bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
                        bmp.extend_from_slice(pixels);
                        std::fs::write(dir.join(format!("{}-{:?}-{}.bmp", scenario, theme, if dock { "dock" } else { "float" })), bmp).unwrap();
                    }
                    SelectObject(dc, old);
                    let _ = DeleteObject(bitmap);
                    let _ = DeleteDC(dc);
                }
            }
        }
    }
}

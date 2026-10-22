//! Version detection and monitor DPI without relying on the application manifest's OS version lie.
use std::sync::OnceLock;
use windows::Win32::{Foundation::HWND, UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::FindWindowW}};
use windows::core::{w, PCWSTR};

pub fn windows_build() -> u32 {
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| unsafe {
        #[repr(C)]
        struct Version { size: u32, major: u32, minor: u32, build: u32, platform: u32, description: [u16; 128] }
        #[link(name = "ntdll")]
        extern "system" { fn RtlGetVersion(version: *mut Version) -> i32; }
        let mut v = Version { size: std::mem::size_of::<Version>() as u32,
            major: 0, minor: 0, build: 0, platform: 0, description: [0; 128] };
        if RtlGetVersion(&mut v) == 0 && v.major >= 10 { v.build } else { 0 }
    })
}

pub fn windows11() -> bool { windows_build() >= 22000 }

pub fn dpi(hwnd: HWND) -> u32 {
    let value = unsafe { GetDpiForWindow(hwnd) };
    if value == 0 { 96 } else { value }
}

pub fn initial_dpi() -> u32 {
    let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default() };
    dpi(taskbar)
}

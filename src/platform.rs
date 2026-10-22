//! Platform paths and native CLI discovery without invoking a shell.
use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home_dir);
    #[cfg(target_os = "macos")]
    let base = home_dir().join("Library/Application Support");
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    base.join("UsageMonitor")
}

pub fn executable_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<_> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    #[cfg(not(windows))]
    dirs.extend([
        home_dir().join(".local/bin"), home_dir().join(".npm-global/bin"),
        PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"), PathBuf::from("/bin"),
    ]);
    dirs.retain(|p| p.is_absolute());
    dirs
}

pub fn executable(path: &std::path::Path) -> bool {
    if !path.is_file() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    }
    #[cfg(not(unix))]
    true
}

pub fn child_path() -> Option<std::ffi::OsString> {
    std::env::join_paths(executable_dirs()).ok()
}

/// Protect locally managed cookie/settings files on Unix; never copies platform credentials.
pub fn write_private(path: &std::path::Path, value: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(value)
}

/// Child coordinates inside the taskbar, directly left of the notification area.
pub fn taskbar_child_rect(taskbar: (i32, i32, i32, i32), tray_left: i32, desired_width: i32) -> (i32, i32, i32, i32) {
    let available = (tray_left.clamp(taskbar.0, taskbar.2) - taskbar.0).max(1);
    let width = desired_width.max(1).min(available);
    (available - width, 0, width, (taskbar.3 - taskbar.1).max(1))
}

/// Screen coordinates for a separate surface confined to a horizontal taskbar.
/// A mostly offscreen auto-hide taskbar must not leave a bar floating on the desktop.
pub fn taskbar_surface_rect(taskbar: (i32, i32, i32, i32), monitor: (i32, i32, i32, i32),
    tray_left: i32, desired_width: i32) -> Option<(i32, i32, i32, i32)> {
    let top = taskbar.1.max(monitor.1);
    let bottom = taskbar.3.min(monitor.3);
    let height = taskbar.3 - taskbar.1;
    if height <= 0 || bottom - top < (height / 2).max(1) { return None; }
    let left = taskbar.0.max(monitor.0);
    let right = tray_left.min(taskbar.2).min(monitor.2);
    if right <= left { return None; }
    let width = desired_width.max(1).min(right - left);
    Some((right - width, top, width, bottom - top))
}

/// Fallback summary rect, including negative-coordinate external monitors.
pub fn summary_rect(work: (i32, i32, i32, i32), desired_width: i32, dpi: u32) -> (i32, i32, i32, i32) {
    let scale = dpi.max(96) as f32 / 96.0;
    let gap = (8.0 * scale).round() as i32;
    let width = desired_width.min((work.2 - work.0 - 2 * gap).max(1)).max(1);
    let height = ((40.0 * scale).round() as i32).min((work.3 - work.1).max(1));
    ((work.2 - gap - width).max(work.0), (work.3 - gap - height).max(work.1), width, height)
}

#[cfg(target_os = "macos")]
pub fn claude_keychain_credentials() -> Result<Option<String>, String> {
    use std::{io::Read, process::{Command, Stdio}, time::{Duration, Instant}};
    let service = match std::env::var("USAGE_MONITOR_CLAUDE_KEYCHAIN_SERVICE") {
        Ok(s) if !s.is_empty() && s.len() <= 256 => s,
        Ok(_) => return Err("Invalid Claude Keychain service configuration".into()),
        Err(_) if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() => {
            // The CLI hashes custom config paths into service names. Never read a different account.
            return Ok(None);
        }
        Err(_) => "Claude Code-credentials".into(),
    };
    let mut child = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", &service, "-w"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|_| "Cannot access macOS Keychain")?;
    let stdout = child.stdout.take().ok_or("Keychain reader unavailable")?;
    let reader = std::thread::spawn(move || {
        let mut value = Vec::new();
        stdout.take(65537).read_to_end(&mut value).map(|_| value)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => { let _ = child.kill(); let _ = child.wait(); break None; }
        }
    };
    let data = reader.join().map_err(|_| "Keychain read failed")?
        .map_err(|_| "Keychain read failed")?;
    match status {
        Some(s) if s.success() && data.len() <= 65536 => String::from_utf8(data)
            .map(|s| Some(s.trim().to_owned())).map_err(|_| "Invalid Keychain credentials".into()),
        Some(s) if s.code() == Some(44) => Ok(None), // errSecItemNotFound
        _ => Err("Claude Keychain locked, denied or timed out; open Claude Code once".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_taskbar_surface_stays_in_band_and_hides_with_taskbar() {
        for (taskbar, monitor) in [
            ((0, 1032, 1920, 1080), (0, 0, 1920, 1080)),
            ((-1920, 1032, 0, 1080), (-1920, 0, 0, 1080)),
            ((0, 0, 2560, 72), (0, 0, 2560, 1440)),
        ] {
            let rect = taskbar_surface_rect(taskbar, monitor, taskbar.2 - 240, 560).unwrap();
            assert_eq!(rect.0 + rect.2, taskbar.2 - 240);
            assert_eq!(rect.1, taskbar.1);
            assert_eq!(rect.3, taskbar.3 - taskbar.1);
        }
        assert_eq!(taskbar_surface_rect((0, 1078, 1920, 1126), (0, 0, 1920, 1080), 1700, 560), None);
        assert_eq!(taskbar_surface_rect((0, -46, 1920, 2), (0, 0, 1920, 1080), 1700, 560), None);
        assert_eq!(taskbar_surface_rect((0, 1032, 320, 1080), (0, 0, 320, 1080), 220, 560), Some((0, 1032, 220, 48)));
    }

    #[test]
    fn taskbar_band_uses_parent_origin_and_full_height() {
        for taskbar in [(0, 1032, 1920, 1080), (-1920, 1032, 0, 1080), (0, 0, 2560, 72)] {
            let tray_left = taskbar.2 - 240;
            let (x, y, w, h) = taskbar_child_rect(taskbar, tray_left, 560);
            assert_eq!(taskbar.0 + x + w, tray_left);
            assert_eq!(y, 0, "must be inside the taskbar, not above it");
            assert_eq!(h, taskbar.3 - taskbar.1);
        }
        assert_eq!(taskbar_child_rect((0, 1032, 320, 1080), 220, 560), (0, 0, 220, 48));
    }

    #[test]
    fn summary_stays_inside_small_or_negative_monitor_work_area() {
        for work in [(-1920, 0, 0, 1040), (0, 0, 320, 200), (0, 0, 1920, 1040)] {
            for dpi in [96, 120, 144, 192] {
                let (x, y, w, h) = summary_rect(work, 1000, dpi);
                assert!(x >= work.0 && y >= work.1 && x + w <= work.2 && y + h <= work.3);
            }
        }
    }
}

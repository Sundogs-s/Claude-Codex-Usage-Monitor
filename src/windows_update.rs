//! Local updater helper. It runs as the member, and only NSIS requests elevation.
use crate::updater::{self, Prepared};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::{Path, PathBuf}, process::{Command, Stdio}, time::Duration};
use std::os::windows::{process::CommandExt, ffi::OsStrExt};
use windows::{core::{w, PCWSTR}, Win32::{
    Foundation::{CloseHandle, WAIT_OBJECT_0, GetLastError, ERROR_ALREADY_EXISTS},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject, GetExitCodeProcess, CreateMutexW},
    UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOCLOSEPROCESS},
    UI::WindowsAndMessaging::SW_SHOWNORMAL,
}};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan { package: Prepared, target: PathBuf, parent_pid: u32 }

fn local_target(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    path.file_name().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("usage-monitor.exe")).unwrap_or(false)
        && matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
}

pub fn launch(package: &Prepared) -> Result<()> {
    updater::validate_cached(package)?;
    let target = std::env::current_exe()?.canonicalize()?;
    if !local_target(&target) { return Err("请将便携版以 usage-monitor.exe 名称复制到本机目录后更新".into()); }
    let folder = updater::cache_root().join(format!("helper-{}", std::process::id()));
    fs::create_dir_all(&folder)?;
    let helper = folder.join("usage-monitor-updater.exe");
    fs::copy(&target, &helper)?;
    let plan = Plan { package: package.clone(), target, parent_pid: std::process::id() };
    let plan_path = folder.join("plan.json");
    crate::platform::write_private(&plan_path, &serde_json::to_vec(&plan)?)?;
    Command::new(helper).arg("--apply-update").arg(plan_path)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(0x08000000).spawn()?;
    Ok(())
}

fn start_app(target: &Path) -> Result<()> {
    let mut child = Command::new(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .creation_flags(0x08000000).spawn()?;
    for _ in 0..12 {
        std::thread::sleep(Duration::from_millis(250));
        if child.try_wait()?.is_some() { return Err("Updated app exited during startup".into()); }
    }
    Ok(())
}

fn replace_portable(source: &Path, target: &Path, expected: &str, start: impl FnOnce() -> Result<()>) -> Result<()> {
    let incoming = target.with_file_name("usage-monitor.incoming.exe");
    let backup = target.with_file_name("usage-monitor.previous.exe");
    // A stale transaction is kept for manual inspection instead of overwriting it.
    if incoming.exists() { return Err("Previous update is incomplete".into()); }
    let mut from = fs::File::open(source)?;
    let mut to = fs::OpenOptions::new().write(true).create_new(true).open(&incoming)?;
    if let Err(error) = std::io::copy(&mut from, &mut to).and_then(|_| to.sync_all()) {
        drop(to); let _ = fs::remove_file(&incoming); return Err(error.into());
    }
    drop(to);
    let transaction = (|| -> Result<()> {
        if !updater::sha256(&incoming)?.eq_ignore_ascii_case(expected) { return Err("Update checksum mismatch".into()); }
        if backup.exists() { fs::remove_file(&backup)?; }
        fs::rename(target, &backup)?;
        let installed = fs::rename(&incoming, target).map_err(Into::into).and_then(|_| start());
        if installed.is_err() {
            // Restore only these exact program-owned paths; keep config/Cookie untouched.
            if target.exists() { fs::remove_file(target)?; }
            fs::rename(&backup, target)?;
        }
        installed
    })();
    if incoming.exists() { let _ = fs::remove_file(&incoming); }
    transaction
}

fn install_nsis(plan: &Plan, package: &Path) -> Result<()> {
    let target_dir = plan.target.parent().ok_or("No installation directory")?;
    let dir = target_dir.to_string_lossy();
    let dir = dir.strip_prefix(r"\\?\").unwrap_or(&dir);
    let file: Vec<u16> = package.as_os_str().encode_wide().chain(Some(0)).collect();
    let args: Vec<u16> = format!("/S /UPDATE /D={dir}").encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut execute = SHELLEXECUTEINFOW { cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS, lpVerb: w!("runas"), lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(args.as_ptr()), nShow: SW_SHOWNORMAL.0, ..Default::default() };
        ShellExecuteExW(&mut execute)?;
        if execute.hProcess.is_invalid() { return Err("Installer did not start".into()); }
        let waited = WaitForSingleObject(execute.hProcess, 15 * 60 * 1000);
        let mut code = 1;
        let result = GetExitCodeProcess(execute.hProcess, &mut code);
        let _ = CloseHandle(execute.hProcess);
        if waited != WAIT_OBJECT_0 || result.is_err() || code != 0 { return Err("Installer failed or timed out".into()); }
    }
    if !updater::sha256(&plan.target)?.eq_ignore_ascii_case(&plan.package.target_sha256) {
        return Err("Installed app checksum mismatch; previous executable retained".into());
    }
    start_app(&plan.target)
}

fn apply(plan_path: &Path) -> Result<()> {
    let canonical = plan_path.canonicalize()?;
    if !canonical.starts_with(updater::cache_root().canonicalize()?) { return Err("Invalid update plan location".into()); }
    let mut data = Vec::new(); fs::File::open(canonical)?.take(32769).read_to_end(&mut data)?;
    if data.len() > 32768 { return Err("Invalid update plan".into()); }
    let plan: Plan = serde_json::from_slice(&data)?;
    if !local_target(&plan.target) || !plan.target.is_file() || plan.parent_pid == std::process::id() {
        return Err("Invalid update target".into());
    }
    // The named kernel mutex releases automatically if a helper crashes.
    unsafe {
        let mutex = CreateMutexW(None, true, w!("Local\\ChoseWay.UsageMonitor.Update"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS { let _ = CloseHandle(mutex); return Err("Update already running".into()); }
        struct Guard(windows::Win32::Foundation::HANDLE);
        impl Drop for Guard { fn drop(&mut self) { unsafe { let _ = CloseHandle(self.0); } } }
        let _guard = Guard(mutex);
        if let Ok(parent) = OpenProcess(PROCESS_SYNCHRONIZE, false, plan.parent_pid) {
            let result = WaitForSingleObject(parent, 60000); let _ = CloseHandle(parent);
            if result != WAIT_OBJECT_0 { return Err("Original app has not exited".into()); }
        }
        let result = (|| -> Result<()> {
            let package = updater::validate_cached(&plan.package)?;
            if plan.package.kind == "installer" { install_nsis(&plan, &package) }
            else { replace_portable(&package, &plan.target, &plan.package.sha256, || start_app(&plan.target)) }
        })();
        if result.is_err() {
            let _ = crate::platform::write_private(&updater::cache_root().join("last-result.json"),
                br#"{"message":"Update cancelled or failed; previous executable/configuration retained"}"#);
            let _ = start_app(&plan.target);
        }
        result
    }
}

pub fn dispatch_helper() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).and_then(|s| s.to_str()) != Some("--apply-update") { return false; }
    let result = if args.len() == 3 { apply(Path::new(&args[2])) } else { Err("Invalid update helper arguments".into()) };
    if result.is_err() { std::process::exit(1); }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_transaction_preserves_backup_and_rolls_back_failed_launch() {
        let dir = std::env::temp_dir().join(format!("usage-monitor-transaction-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("new.exe"); let target = dir.join("usage-monitor.exe");
        fs::write(&source, b"new executable").unwrap(); fs::write(&target, b"old executable").unwrap();
        let hash = updater::sha256(&source).unwrap();
        assert!(replace_portable(&source, &target, &hash, || Err("simulated startup failure".into())).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old executable");
        assert!(replace_portable(&source, &target, &"0".repeat(64), || Ok(())).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old executable");
        replace_portable(&source, &target, &hash, || Ok(())).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new executable");
        assert_eq!(fs::read(dir.join("usage-monitor.previous.exe")).unwrap(), b"old executable");
        for file in [source, target, dir.join("usage-monitor.previous.exe")] { fs::remove_file(file).unwrap(); }
        fs::remove_dir(dir).unwrap();
    }
}

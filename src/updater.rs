//! Windows team updater. SMB I/O runs in a bounded disposable worker process.
use crate::state::SharedState;
use serde::{Deserialize, Serialize};
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}, time::Duration};
use windows::{core::PCWSTR, Win32::Security::Cryptography::*};

const MAX_PACKAGE: u64 = 128 * 1024 * 1024;
pub static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
pub fn request_check() { WAKE.notify_one(); }

#[derive(Clone, Debug, Default)]
pub struct UpdateState {
    pub message: String,
    pub ready: Option<Prepared>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact { pub path: String, pub sha256: String, pub size: u64 }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub schema: u32, pub platform: String, pub version: String,
    pub installer: Artifact, pub portable: Artifact,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    pub version: String, pub kind: String, pub path: PathBuf, pub sha256: String,
    pub target_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply { Current, Ready { package: Prepared }, Failed }

pub fn default_root() -> String { r"\\qianwang\work\PROJECTS\AI\UsageMointor".into() }
pub fn cache_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
        .join("UsageMonitor/updates")
}
pub fn version(value: &str) -> Result<[u32; 3], &'static str> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || p.len() > 8 || !p.bytes().all(|c| c.is_ascii_digit())
        || (p.len() > 1 && p.starts_with('0'))) { return Err("Invalid version"); }
    Ok([parts[0].parse().map_err(|_| "Invalid version")?, parts[1].parse().map_err(|_| "Invalid version")?,
        parts[2].parse().map_err(|_| "Invalid version")?])
}
fn validate(feed: &Feed, kind: &str) -> Result<Artifact, &'static str> {
    version(&feed.version)?;
    if feed.schema != 1 || feed.platform != "windows-x64" { return Err("Unsupported update platform"); }
    let artifact = match kind { "installer" => &feed.installer, "portable" => &feed.portable,
        _ => return Err("Unsupported update type") };
    let expected = if kind == "installer" { format!("UsageMonitor-{}-Setup.exe", feed.version) }
        else { "usage-monitor.exe".into() };
    if artifact.path != format!("v{}/{}", feed.version, expected)
        || artifact.size == 0 || artifact.size > MAX_PACKAGE
        || artifact.sha256.len() != 64 || !artifact.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid update artifact");
    }
    Ok(artifact.clone())
}

/// Stream SHA-256 through Windows CNG; no shell, crypto crate, or large allocation.
pub fn sha256(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    struct Algorithm(BCRYPT_ALG_HANDLE);
    impl Drop for Algorithm { fn drop(&mut self) { unsafe { let _ = BCryptCloseAlgorithmProvider(self.0, 0); } } }
    struct Hash(BCRYPT_HASH_HANDLE);
    impl Drop for Hash { fn drop(&mut self) { unsafe { let _ = BCryptDestroyHash(self.0); } } }
    unsafe {
        let mut raw = BCRYPT_ALG_HANDLE::default();
        BCryptOpenAlgorithmProvider(&mut raw, BCRYPT_SHA256_ALGORITHM, PCWSTR::null(), BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0)).ok()?;
        let algorithm = Algorithm(raw);
        let mut raw_hash = BCRYPT_HASH_HANDLE::default();
        BCryptCreateHash(algorithm.0, &mut raw_hash, None, None, 0).ok()?;
        let hash = Hash(raw_hash);
        let mut input = fs::File::open(path)?;
        let mut buffer = [0u8; 65536];
        let mut size = 0u64;
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 { break; }
            size += n as u64;
            if size > MAX_PACKAGE { return Err("Update exceeds size limit".into()); }
            BCryptHashData(hash.0, &buffer[..n], 0).ok()?;
        }
        let mut digest = [0u8; 32];
        BCryptFinishHash(hash.0, &mut digest, 0).ok()?;
        Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
    }
}

fn prepare(root: &Path, preview: bool, kind: &str) -> Result<Reply, Box<dyn std::error::Error>> {
    if !root.is_absolute() { return Err("Release root must be absolute".into()); }
    let root = root.canonicalize()?;
    let name = if preview { "preview-windows.json" } else { "update-windows.json" };
    let mut bytes = Vec::new();
    fs::File::open(root.join(name))?.take(32769).read_to_end(&mut bytes)?;
    if bytes.len() > 32768 { return Err("Update feed too large".into()); }
    let feed: Feed = serde_json::from_slice(&bytes)?;
    let artifact = validate(&feed, kind)?;
    let portable = validate(&feed, "portable")?;
    if version(&feed.version)? <= version(env!("CARGO_PKG_VERSION"))? { return Ok(Reply::Current); }
    let source = root.join(&artifact.path).canonicalize()?;
    if !source.starts_with(&root) || fs::metadata(&source)?.len() != artifact.size {
        return Err("Update source is invalid".into());
    }
    let dir = cache_root().join(&feed.version).join(artifact.sha256.to_ascii_lowercase());
    fs::create_dir_all(&dir)?;
    let target = dir.join(if kind == "installer" { "setup.exe" } else { "usage-monitor.exe" });
    if !target.is_file() || !sha256(&target).map(|h| h.eq_ignore_ascii_case(&artifact.sha256)).unwrap_or(false) {
        let partial = dir.join(format!("download-{}.partial", std::process::id()));
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let mut input = fs::File::open(&source)?.take(MAX_PACKAGE + 1);
            let mut output = fs::OpenOptions::new().create_new(true).write(true).open(&partial)?;
            let size = std::io::copy(&mut input, &mut output)?;
            output.flush()?; output.sync_all()?; drop(output);
            if size != artifact.size || !sha256(&partial)?.eq_ignore_ascii_case(&artifact.sha256) {
                return Err("Update checksum mismatch".into());
            }
            if target.exists() { fs::remove_file(&target)?; }
            fs::rename(&partial, &target)?;
            Ok(())
        })();
        if result.is_err() { let _ = fs::remove_file(&partial); }
        result?;
    }
    Ok(Reply::Ready { package: Prepared { version: feed.version, kind: kind.into(), path: target, sha256: artifact.sha256, target_sha256: portable.sha256 } })
}

/// Run before any window/logger/collector initialization; errors never echo feed contents.
pub fn dispatch_worker() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).and_then(|s| s.to_str()) != Some("--update-worker") { return false; }
    let reply = if args.len() == 5 {
        prepare(Path::new(&args[2]), args[3] == "preview", args[4].to_str().unwrap_or("")).unwrap_or(Reply::Failed)
    } else { Reply::Failed };
    let _ = serde_json::to_writer(std::io::stdout(), &reply);
    true
}

async fn worker(settings: &crate::state::Settings) -> Reply {
    use tokio::{io::AsyncReadExt, process::Command};
    let Ok(exe) = std::env::current_exe() else { return Reply::Failed; };
    let installed = exe.parent().map(|p| p.join("Uninstall.exe").is_file()).unwrap_or(false);
    let mut command = Command::new(exe);
    command.args(["--update-worker", &settings.update_root,
        if settings.update_preview { "preview" } else { "stable" }, if installed { "installer" } else { "portable" }])
        .stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).stdout(std::process::Stdio::piped())
        .creation_flags(0x08000000).kill_on_drop(true);
    let Ok(mut child) = command.spawn() else { return Reply::Failed; };
    let Some(stdout) = child.stdout.take() else { return Reply::Failed; };
    let work = async {
        let mut bytes = Vec::new();
        stdout.take(32769).read_to_end(&mut bytes).await.ok()?;
        if bytes.len() > 32768 || !child.wait().await.ok()?.success() { return None; }
        serde_json::from_slice(&bytes).ok()
    };
    tokio::time::timeout(Duration::from_secs(45), work).await.ok().flatten().unwrap_or(Reply::Failed)
}

pub async fn poll(shared: SharedState, notify: std::sync::Arc<dyn Fn() + Send + Sync>) {
    let mut delay = Duration::from_secs(15);
    loop {
        let manual = tokio::select! { _ = WAKE.notified() => true, _ = tokio::time::sleep(delay) => false };
        delay = Duration::from_secs(3600);
        let settings = shared.lock().settings.clone();
        if !manual && !settings.auto_update_check { continue; }
        { shared.lock().update.message = "正在检查并准备更新…".into(); }
        notify();
        let reply = worker(&settings).await;
        {
            let mut s = shared.lock();
            // Ignore results for a channel/source that the user changed mid-download.
            if s.settings.update_root != settings.update_root || s.settings.update_preview != settings.update_preview { continue; }
            s.update = match reply {
                Reply::Current => UpdateState { message: "当前已是该通道的最新版本".into(), ready: None },
                Reply::Ready { package } => UpdateState { message: format!("v{} 已准备，安装并重启", package.version), ready: Some(package) },
                Reply::Failed => UpdateState { message: "更新源不可达或校验未通过，稍后重试".into(), ready: None },
            };
        }
        notify();
    }
}

pub fn validate_cached(package: &Prepared) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if version(&package.version)? <= version(env!("CARGO_PKG_VERSION"))?
        || package.target_sha256.len() != 64 || !package.target_sha256.bytes().all(|c| c.is_ascii_hexdigit())
        || !matches!(package.kind.as_str(), "installer" | "portable") { return Err("Invalid update".into()); }
    let path = package.path.canonicalize()?;
    if !path.starts_with(cache_root().canonicalize()?) || !sha256(&path)?.eq_ignore_ascii_case(&package.sha256) {
        return Err("Cached update is invalid".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_platform_and_artifact_paths_are_strict() {
        assert!(version("0.10.0").unwrap() > version("0.9.9").unwrap());
        for s in ["1.2", "1.2.3.4", "1.2.3-beta", "01.2.3", "1/2/3", "-1.2.3"] { assert!(version(s).is_err()); }
        let a = Artifact { path: "v1.0.0/usage-monitor.exe".into(), sha256: "a".repeat(64), size: 3 };
        let mut feed = Feed { schema: 1, platform: "windows-x64".into(), version: "1.0.0".into(), installer: a.clone(), portable: a };
        assert!(validate(&feed, "portable").is_ok());
        for path in ["../usage-monitor.exe", "v1.0.0/../../evil.exe", "C:/evil.exe", "v1.0.0/usage-monitor.exe:evil"] {
            feed.portable.path = path.into(); assert!(validate(&feed, "portable").is_err());
        }
        feed.portable.path = "v1.0.0/usage-monitor.exe".into();
        feed.platform = "macos-arm64".into(); assert!(validate(&feed, "portable").is_err());
    }
    #[test]
    fn cng_sha256_matches_published_test_vector() {
        let dir = std::env::temp_dir().join(format!("usage-monitor-hash-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap(); let path = dir.join("abc");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(sha256(&path).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        fs::remove_file(path).unwrap(); fs::remove_dir(dir).unwrap();
    }
}

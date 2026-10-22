"""Isolated Windows update workers and real portable-helper transactions; no live app/NAS/UAC."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary")
parser.add_argument("--with-helper", action="store_true")
args = parser.parse_args()
binary = str(Path(args.binary).resolve())
if os.name != "nt":
    raise SystemExit("Windows integration test only")

def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

with tempfile.TemporaryDirectory(prefix="usage-monitor-update-") as root:
    root = Path(root).resolve()
    env = dict(os.environ, LOCALAPPDATA=str(root / "local"), APPDATA=str(root / "roaming"), USERPROFILE=str(root))
    release = root / "release"
    release.mkdir()
    current = subprocess.check_output([binary, "--version"], env=env, timeout=10).decode().strip().split()[-1]

    def feed(version, preview=False, bad_hash=False):
        folder = release / ("v" + version)
        folder.mkdir(exist_ok=True)
        portable = folder / "usage-monitor.exe"
        setup = folder / f"UsageMonitor-{version}-Setup.exe"
        portable.write_bytes(b"fixture-only; never execute this package")
        setup.write_bytes(b"fixture installer; never execute")
        value = {"schema": 1, "platform": "windows-x64", "version": version}
        for key, path in [("portable", portable), ("installer", setup)]:
            value[key] = {"path": str(path.relative_to(release)).replace("\\", "/"),
                          "sha256": "0" * 64 if bad_hash else digest(path), "size": path.stat().st_size}
        name = "preview-windows.json" if preview else "update-windows.json"
        (release / name).write_text(json.dumps(value), encoding="utf-8")
        return value

    def check(channel="stable", kind="portable"):
        result = subprocess.run([binary, "--update-worker", str(release), channel, kind], env=env,
                                capture_output=True, timeout=10)
        assert result.returncode == 0 and not result.stderr
        return json.loads(result.stdout)

    feed(current)
    assert check()["status"] == "current"
    feed("99.0.0")
    ready = check()
    assert ready["status"] == "ready"
    cached = Path(ready["package"]["path"])
    assert digest(cached) == ready["package"]["sha256"]
    cached.write_bytes(b"corrupted local cache")
    assert check()["status"] == "ready" and digest(cached) == ready["package"]["sha256"]
    assert check(kind="installer")["package"]["kind"] == "installer"
    print("PASS: version gate, portable/installer selection, download hash, corrupt-cache recovery")

    feed("99.0.1", bad_hash=True)
    assert check()["status"] == "failed"
    assert not list((root / "local").rglob("*.partial"))
    value = feed("99.0.2")
    value["portable"]["path"] = "../outside.exe"
    (release / "update-windows.json").write_text(json.dumps(value), encoding="utf-8")
    assert check()["status"] == "failed"
    value = feed("99.0.3")
    value["platform"] = "macos-arm64"
    (release / "update-windows.json").write_text(json.dumps(value), encoding="utf-8")
    assert check()["status"] == "failed"
    print("PASS: hash failure, partial cleanup, path traversal and wrong-platform rejection")

    feed(current)
    feed("99.0.4", preview=True)
    assert check()["status"] == "current" and check("preview")["status"] == "ready"
    (release / "update-windows.json").write_bytes(b"x" * 40000)
    assert check()["status"] == "failed"
    print("PASS: stable/preview isolation and bounded feed")

    if args.with_helper:
        source = root / "fixture.rs"
        source.write_text('''#![windows_subsystem = "windows"]
fn main() {
    let dir = std::env::current_exe().unwrap().parent().unwrap().to_owned();
    std::fs::write(dir.join(concat!(env!("FIXTURE_NAME"), ".marker")), std::process::id().to_string()).unwrap();
    if cfg!(fixture_fail) { std::process::exit(1); }
    std::thread::sleep(std::time::Duration::from_secs(5));
}
''', encoding="utf-8")
        fixtures = {}
        for name in ("old", "new", "fail"):
            dest = root / (name + ".exe")
            command = ["rustc", "--crate-name", "update_fixture", "--edition=2021", str(source), "-o", str(dest)]
            if name == "fail": command += ["--cfg", "fixture_fail"]
            compiled = subprocess.run(command, env=dict(os.environ, FIXTURE_NAME=name), capture_output=True, timeout=60)
            assert compiled.returncode == 0, compiled.stderr.decode(errors="replace")
            fixtures[name] = dest

        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
        kernel.OpenProcess.restype = ctypes.c_void_p
        kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        for name in ("new", "fail"):
            local = root / ("portable case " + name)
            local.mkdir()
            target = local / "usage-monitor.exe"
            shutil.copyfile(fixtures["old"], target)
            settings = local / "settings.json"
            settings.write_bytes(b'{"member":"fixture-config"}')
            package = root / "local/UsageMonitor/updates" / name / "usage-monitor.exe"
            package.parent.mkdir(parents=True)
            shutil.copyfile(fixtures[name], package)
            plan = package.parent / "plan.json"
            plan.write_text(json.dumps({"package": {"version": "99.1.0", "kind": "portable", "path": str(package),
                "sha256": digest(package), "target_sha256": digest(package)}, "target": str(target),
                "parent_pid": 0}), encoding="utf-8") # parent is already gone in this isolated fixture
            result = subprocess.run([binary, "--apply-update", str(plan)], env=env, capture_output=True, timeout=15)
            assert result.returncode == (0 if name == "new" else 1)
            assert digest(target) == digest(fixtures["new"] if name == "new" else fixtures["old"])
            assert settings.read_bytes() == b'{"member":"fixture-config"}'
            marker = local / ("new.marker" if name == "new" else "old.marker")
            assert marker.exists()
            child_pid = int(marker.read_text())
            handle = kernel.OpenProcess(0x00100000, False, child_pid)
            if handle:
                assert kernel.WaitForSingleObject(handle, 7000) == 0
                kernel.CloseHandle(handle)
        print("PASS: real helper replacement/relaunch, failed-launch rollback/relaunch, settings preserved")

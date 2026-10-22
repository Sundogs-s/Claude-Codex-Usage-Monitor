"""Isolated stdio integration checks; no provider CLI/network or real credentials."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="usage-monitor-bridge-") as root:
    env = dict(os.environ, APPDATA=root, HOME=root, USERPROFILE=root, XDG_CONFIG_HOME=root)
    data = Path(root) / ("Library/Application Support/UsageMonitor" if sys.platform == "darwin" else "UsageMonitor")
    data.mkdir(parents=True)
    (data / "settings.json").write_text(json.dumps({
        "show_claude": False, "show_codex": False, "show_cursor": False
    }), encoding="utf-8")
    proc = subprocess.Popen([binary, "--stdio", "--fixture"], env=env,
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    commands = [
        {"action": "refresh", "prompt": "private-test-value"},
        {"action": "set_service", "service": "codex", "enabled": True},
        {"action": "set_refresh", "seconds": 300},
        {"action": "set_cookie", "cookie": "fixture-private-cookie"},
        {"action": "clear_cookie"},
        {"action": "quit"},
    ]
    wire = b"".join(json.dumps(c).encode() + b"\n" for c in commands)
    output, errors = proc.communicate(wire, timeout=10)
    assert proc.returncode == 0 and not errors, (proc.returncode, errors)
    views = [json.loads(line) for line in output.splitlines()]
    assert all(v["schema"] == 1 for v in views)
    assert any(v["message"] == "Invalid command" for v in views)
    assert any(v["services"][1]["enabled"] for v in views)
    assert any(v["refresh_seconds"] == 300 for v in views)
    assert any(v["cookie_configured"] for v in views)
    assert not views[-1]["cookie_configured"]
    assert not (data / "cursor_cookie.txt").exists()
    assert b"fixture-private-cookie" not in output and b"private-test-value" not in output
    print("PASS: command validation, service/interval changes, cookie save/clear, no secret echo")

    # Reject an oversized frame without an unbounded allocation or shutdown hang.
    result = subprocess.run([binary, "--stdio", "--fixture"], input=b"x" * 40000 + b"\n",
                            env=env, capture_output=True, timeout=10)
    assert result.returncode == 0 and b"Invalid command" in result.stdout
    print("PASS: bounded malformed frame")

    # Real monitor lifecycle with providers disabled: EOF cancels/reaps the worker.
    (data / "settings.json").write_text(json.dumps({
        "show_claude": False, "show_codex": False, "show_cursor": False
    }), encoding="utf-8")
    started = time.monotonic()
    result = subprocess.run([binary, "--stdio"], input=b"", env=env, capture_output=True, timeout=10)
    assert result.returncode == 0, result.stderr
    assert time.monotonic() - started < 8
    print("PASS: EOF cancels monitor in under 8 seconds")

"""Real Herdr/tmux cleanup lifecycle, using only an owned temporary server.

Usage: python3 tests/cleanup_real.py target/debug/herdr-scratch
TTL's injected-clock real-tmux cases are in cleanup::real_tests.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
herdr = shutil.which("herdr")
assert herdr and shutil.which("tmux")

with tempfile.TemporaryDirectory(prefix="sc-", dir="/tmp") as temporary:
    root = Path(temporary)
    state, config_dir = root / "s", root / "c"
    state.mkdir(); config_dir.mkdir()
    config_dir.joinpath("config.yaml").write_text(
        Path(__file__).resolve().parents[1].joinpath("config.default.yaml").read_text().replace("interval_seconds: 60", "interval_seconds: 5"))
    herdr_config = root / "herdr.toml"
    herdr_config.write_text('onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\n')
    env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_") and k not in ("TMUX", "TMUX_PANE")}
    env.update(XDG_CONFIG_HOME=str(root / "hc"), XDG_STATE_HOME=str(root / "hs"),
               HERDR_CONFIG_PATH=str(herdr_config), HERDR_PLUGIN_STATE_DIR=str(state),
               HERDR_PLUGIN_CONFIG_DIR=str(config_dir))
    tmux_socket = state / f"tmux-{os.geteuid()}" / "shadowfax-herdr-scratch"
    tmux_socket.parent.mkdir(mode=0o700)

    def cli(*args):
        result = subprocess.run([herdr, "--session", "cleanup-test", *args], env=env, text=True, capture_output=True, timeout=5)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout) if result.stdout.lstrip().startswith("{") else result.stdout

    def tmux(*args):
        result = subprocess.run(["tmux", "-S", str(tmux_socket), "-f", "/dev/null", *args], env=env, text=True, capture_output=True, timeout=5)
        assert result.returncode == 0, result.stderr
        return result.stdout

    def cleanup(*args):
        result = subprocess.run([binary, *args], env=env, text=True, capture_output=True, timeout=6)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout) if result.stdout.strip() else None

    def create_scratch(pane):
        name = f"hs/shell/{hashlib.sha256(socket.encode()).hexdigest()[:12]}/{pane.replace(':', '-')}"
        tmux("new-session", "-d", "-s", name, "sleep 300")
        tmux("set-option", "-t", name, "@herdr_source_pane", pane)
        tmux("set-option", "-t", name, "@herdr_env_version", "1")
        tmux("set-environment", "-t", name, "HERDR_SOCKET_PATH", socket)
        return name

    def wait_for(predicate, seconds=8):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if predicate(): return
            time.sleep(.05)
        raise AssertionError("condition did not become true")

    def worker_locked():
        with (state / "cleanup-worker.lock").open("a") as f:
            try:
                fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return True
            return False

    log = (root / "server.log").open("w")
    server = subprocess.Popen([herdr, "--session", "cleanup-test", "server"], env=env, stdout=log, stderr=log)
    try:
        wait_for(lambda: cli("status", "--json").get("server", {}).get("running"))
        socket = cli("status", "--json")["server"]["socket"]
        assert Path(socket).is_relative_to(root)
        source = cli("workspace", "create", "--cwd", str(root), "--no-focus")["result"]["root_pane"]["pane_id"]
        # Keep the server and private tmux alive after the last tested owner dies.
        keeper = cli("workspace", "create", "--cwd", str(root), "--no-focus")["result"]["root_pane"]["pane_id"]
        tmux("new-session", "-d", "-s", "foreign", "sleep 300")
        name = create_scratch(source)
        assert cleanup("cleanup")["candidates"] == []
        assert not (state / "cleanup-state.json").exists(), "preview wrote owner state"
        assert cleanup("cleanup", "--apply")["removed"] == 0
        moved = cli("pane", "move", source, "--new-workspace", "--no-focus")["result"]["move_result"]["pane"]["pane_id"]
        assert cleanup("cleanup", "--apply")["removed"] == 0
        # Legacy sessions with no cached terminal must follow the inherited alias.
        (state / "cleanup-state.json").unlink()
        assert cleanup("cleanup", "--apply")["removed"] == 0
        cli("pane", "close", moved)
        preview = cleanup("cleanup")
        assert preview["candidates"][0]["reason"] == "source_closed", preview
        report = cleanup("cleanup", "--apply")
        assert report["removed"] == 1, report
        tmux("has-session", "-t", "foreign")
        print("PASS: real source move, legacy alias recovery, closure, preview, foreign preservation")

        source = cli("pane", "split", keeper, "--direction", "right", "--cwd", str(root), "--no-focus")["result"]["pane"]["pane_id"]
        create_scratch(source)
        plugin = root / "plugin"
        plugin.mkdir()
        hook = json.dumps(["/usr/bin/env", f"HERDR_PLUGIN_STATE_DIR={state}", f"HERDR_PLUGIN_CONFIG_DIR={config_dir}", binary, "cleanup-start"])
        plugin.joinpath("herdr-plugin.toml").write_text(f'''id = "test.scratch-cleanup"
name = "Isolated cleanup hook test"
version = "0.1.0"
description = "Owns only test processes"
min_herdr_version = "0.9.0"
[[startup]]
command = {hook}
[[events]]
on = "pane.closed"
command = {hook}
''')
        cli("plugin", "link", str(plugin))
        cli("pane", "close", source)
        wait_for(worker_locked)
        cleanup("cleanup-start")  # Idempotent; the lifetime lock remains held.
        wait_for(lambda: "hs/" not in tmux("list-sessions", "-F", "#{session_name}"))
        cli("plugin", "disable", "test.scratch-cleanup")
        # A restart must revoke a stop even while the old process still holds
        # its lifetime lock. No plugin hook is available to mask a lost restart.
        cleanup("cleanup-stop")
        cleanup("cleanup-start")
        time.sleep(.5)
        assert worker_locked(), "rapid stop/start left cleanup stopped"
        cleanup("cleanup-stop")
        wait_for(lambda: not worker_locked())
        print("PASS: real plugin hook, idempotent start, rapid restart, autonomous cleanup and stop")

        # Status publication is advisory. Force rename to fail, then prove a
        # later sweep still reclaims a newly closed owner without another start.
        status = state / "cleanup-status.json"
        status.unlink(missing_ok=True)
        status.mkdir()
        cleanup("cleanup-start")
        wait_for(worker_locked)
        wait_for(lambda: "cleanup status:" in (state / "cleanup.log").read_text())
        assert worker_locked(), "status write failure killed cleanup"
        source = cli("pane", "split", keeper, "--direction", "right", "--cwd", str(root), "--no-focus")["result"]["pane"]["pane_id"]
        create_scratch(source)
        cli("pane", "close", source)
        wait_for(lambda: "hs/" not in tmux("list-sessions", "-F", "#{session_name}"))
        status.rmdir()
        wait_for(status.is_file)
        cleanup("cleanup-stop")
        wait_for(lambda: not worker_locked())
        print("PASS: status publication failure preserves future sweeps and recovers")

        source = cli("pane", "split", keeper, "--direction", "right", "--cwd", str(root), "--no-focus")["result"]["pane"]["pane_id"]
        create_scratch(source)
        cli("server", "stop"); server.wait(timeout=5)
        report = cleanup("cleanup", "--apply")
        assert report["removed"] == 0 and report["errors"], report
        print("PASS: disconnected Herdr is preserved as unknown ownership")
    finally:
        cleanup("cleanup-stop")
        wait_for(lambda: not worker_locked())
        subprocess.run(["tmux", "-S", str(tmux_socket), "kill-server"], env=env, capture_output=True, timeout=5)
        if server.poll() is None:
            cli("server", "stop"); server.wait(timeout=5)
        log.close()

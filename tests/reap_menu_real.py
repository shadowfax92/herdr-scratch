"""Exercise Menu confirmation -> real Herdr dispatch -> Scratch's owned sweep.

Usage: python3 tests/reap_menu_real.py ../herdr-menu/target/release/herdr-menu target/release/herdr-scratch
Owns a temporary Herdr server, PTYs, tmux servers, and all test sessions. The
production reap action is copied without lifecycle hooks so the background
worker cannot remove candidates before the menu action gets to them.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import tomllib

menu, scratch = (str(Path(arg).resolve()) for arg in sys.argv[1:3])
herdr = shutil.which("herdr")
assert herdr and shutil.which("tmux")
repo = Path(__file__).resolve().parents[1]
action = next(a for a in tomllib.loads((repo / "herdr-plugin.toml").read_text())["actions"] if a["id"] == "reap")
menu_item = '''[[items]]
key = "x"
label = "reap scratch sessions"
command = '"$HERDR_BIN_PATH" plugin action invoke shadowfax.scratch.reap'
confirm = "Reap stale Herdr Scratch sessions?"
'''


def wait_for(predicate, label, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError("Timed out: " + label)


with tempfile.TemporaryDirectory(prefix="sr-", dir="/tmp") as temporary:
    root = Path(temporary)
    herdr_config = root / "herdr.toml"
    herdr_config.write_text('onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\n')
    env = {k: v for k, v in os.environ.items() if not k.startswith(("HERDR_", "TMUX"))}
    env.update(XDG_CONFIG_HOME=str(root / "c"), XDG_STATE_HOME=str(root / "s"),
               HERDR_CONFIG_PATH=str(herdr_config), TERM="xterm-256color", SHELL="/bin/sh")
    session = "scratch-reap-test"
    sockets = []
    clients = []

    def cli(*args):
        result = subprocess.run([herdr, "--session", session, *args], env=env,
                                capture_output=True, text=True, timeout=5)
        assert result.returncode == 0, result.stderr or result.stdout
        return json.loads(result.stdout) if result.stdout.lstrip().startswith("{") else result.stdout.strip()

    def tmux(sock, *args, check=True):
        result = subprocess.run(["tmux", "-S", str(sock), "-f", "/dev/null", *args],
                                env=env, capture_output=True, text=True, timeout=5)
        if check:
            assert result.returncode == 0, result.stderr
        return result.stdout.strip()

    def create(sock, kind, pane):
        if sock not in sockets:
            sock.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
            sockets.append(sock)
            tmux(sock, "new-session", "-d", "-s", "foreign", "sleep 300")
        name = f"hs/{kind}/{hashlib.sha256(socket.encode()).hexdigest()[:12]}/{pane.replace(':', '-')}"
        tmux(sock, "new-session", "-d", "-s", name, "sleep 300")
        tmux(sock, "set-option", "-t", name, "@herdr_source_pane", pane)
        tmux(sock, "set-option", "-t", name, "@herdr_env_version", "1")
        tmux(sock, "set-environment", "-t", name, "HERDR_SOCKET_PATH", socket)
        return sock, name

    def exists(pair):
        sock, name = pair
        return name in tmux(sock, "list-sessions", "-F", "#{session_name}").splitlines()

    def logs():
        return cli("plugin", "log", "list", "--plugin", "shadowfax.scratch", "--limit", "50")["result"]["logs"]

    def run_menu(approve=True):
        # Deliberately give Menu its own plugin directories. Herdr dispatch must
        # replace them with Scratch's, rather than scanning Menu's decoy server.
        menu_env = env | dict(HERDR_BIN_PATH=herdr, HERDR_SOCKET_PATH=socket,
                             HERDR_PLUGIN_CONFIG_DIR=str(menu_config), HERDR_PLUGIN_STATE_DIR=str(menu_state),
                             HERDR_MENU_SOURCE_CWD=str(root), HERDR_MENU_SOURCE_PANE_ID=live)
        before = {log["log_id"] for log in logs()}
        pid, fd = pty.fork()
        if pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 110, 0, 0))
            os.execve(menu, [menu, "menu"], menu_env)
        output = bytearray()
        exited = False

        def read():
            if select.select([fd], [], [], .05)[0]:
                try:
                    chunk = os.read(fd, 65536)
                    output.extend(chunk)
                    if b"\x1b[6n" in chunk:
                        os.write(fd, b"\x1b[1;1R")
                except OSError:
                    pass

        def has_text(text):
            read()
            return text.encode() in output

        def done():
            result = os.waitpid(pid, os.WNOHANG)
            read()
            return result

        try:
            wait_for(lambda: has_text("reap scratch sessions"), "menu renders")
            os.write(fd, b"x")
            wait_for(lambda: has_text("Reap stale Herdr Scratch"), "confirmation renders")
            os.write(fd, b"y" if approve else b"n\x1b")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                child, status = done()
                if child:
                    exited = True
                    assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors="replace")
                    break
            assert exited, output.decode(errors="replace")
        finally:
            if not exited:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            os.close(fd)
        if not approve:
            assert {log["log_id"] for log in logs()} == before, "cancel dispatched cleanup"
            return None
        return wait_for(lambda: next((log for log in logs() if log["log_id"] not in before
                                      and log["status"] != "running"), None), "reap action finishes")

    with (root / "herdr.log").open("w") as log:
        server = subprocess.Popen([herdr, "--session", session, "server"], env=env, stdout=log, stderr=log)
        try:
            wait_for(lambda: cli("status", "--json").get("server", {}).get("running"), "Herdr starts")
            socket = cli("status", "--json")["server"]["socket"]
            assert Path(socket).is_relative_to(root)
            live = cli("workspace", "create", "--cwd", str(root), "--no-focus")["result"]["root_pane"]["pane_id"]
            closed = cli("pane", "split", live, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
            plugin = root / "plugin"
            plugin.mkdir()
            command = json.dumps([scratch, *action["command"][1:]])
            (plugin / "herdr-plugin.toml").write_text(f'''id = "shadowfax.scratch"
name = "Isolated Scratch reap test"
version = "0.1.0"
description = "Tests cross-plugin cleanup routing"
min_herdr_version = "0.9.0"
[[actions]]
id = "reap"
title = "Reap scratch sessions"
command = {command}
''')
            cli("plugin", "link", str(plugin))
            config = Path(cli("plugin", "config-dir", "shadowfax.scratch")) / "config.yaml"
            config.write_text((repo / "config.default.yaml").read_text())
            state = root / "s/herdr/plugins/shadowfax.scratch"
            menu_config, menu_state = root / "menu-config", root / "menu-state"
            menu_config.mkdir(); menu_state.mkdir()
            (menu_config / "config.toml").write_text(menu_item)
            minimal = state / f"tmux-{os.geteuid()}/shadowfax-herdr-scratch"
            workspace = state / f"tmux-{os.geteuid()}/shadowfax-herdr-workspace"
            stale = [create(minimal, "nvim", closed), create(workspace, "nvim", closed), create(workspace, "shell", closed)]
            survivors = [create(workspace, "nvim", live), create(workspace, "shell", live),
                         create(menu_state / f"tmux-{os.geteuid()}/shadowfax-herdr-workspace", "shell", closed)]
            client = subprocess.Popen(["tmux", "-S", str(workspace), "-C", "attach-session", "-t", survivors[0][1]],
                                      env=env, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            clients.append(client)
            wait_for(lambda: tmux(workspace, "display-message", "-p", "-t", survivors[0][1], "#{session_attached}") == "1", "live attachment")
            cli("pane", "close", closed)
            run_menu(approve=False)
            assert all(exists(pair) for pair in stale + survivors)
            result = run_menu()
            assert result["status"] == "succeeded", result
            report = json.loads(result["stdout"])
            assert report["removed"] == 3 and report["owned_sessions"] == 5 and not report["errors"], report
            assert not any(exists(pair) for pair in stale)
            assert all(exists(pair) for pair in survivors)
            for sock in sockets:
                tmux(sock, "has-session", "-t", "foreign")
            print("PASS: menu cancellation, confirmed dispatch, both servers reaped; live, attached, foreign and Menu-owned sessions preserved", flush=True)
            config.write_text(config.read_text().replace("enabled: true", "enabled: false"))
            stale = create(workspace, "nvim", closed)
            result = run_menu()
            assert result["status"] == "succeeded" and not json.loads(result["stdout"])["enabled"], result
            assert exists(stale)
            config.write_text("invalid: [")
            result = run_menu()
            assert result["status"] == "failed" and "invalid scratch config" in result["stderr"], result
            assert exists(stale)
            print("PASS: disabled cleanup and invalid config preserve sessions and report the correct action status", flush=True)
        finally:
            for client in clients:
                client.terminate(); client.wait(timeout=5)
            for sock in sockets:
                tmux(sock, "kill-server", check=False)
            # Only the owned server under the temporary XDG roots is stopped.
            if server.poll() is None:
                cli("server", "stop")
                server.wait(timeout=5)

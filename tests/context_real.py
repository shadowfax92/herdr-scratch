"""Verify Scratch Context through a real Herdr pane, tmux and Neovim RPC.

Usage: python3 tests/context_real.py target/release/herdr-scratch <worktree-a> <worktree-b>
Creates one unfocused tab in the default session's ft workspace. All tmux
servers, configs and RPC sockets are temporary; only that test pane is closed.
"""

import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time


def run(*args, env=None, check=True):
    result = subprocess.run(args, env=env, capture_output=True, text=True, timeout=10)
    if check:
        assert result.returncode == 0, (args, result.stdout, result.stderr)
    return result.stdout.strip()


def wait_for(predicate, label):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError("Timed out: " + label)


def main():
    assert os.environ.get("HERDR_ENV") == "1", "run inside Herdr"
    binary = str(Path(sys.argv[1]).resolve())
    roots = [str(Path(arg).resolve()) for arg in sys.argv[2:4]]
    assert len(roots) == 2 and roots[0] != roots[1]
    assert all(Path(root).is_dir() and len(root) <= 80 for root in roots)
    repo = Path(__file__).resolve().parents[1]
    cli = lambda *args: run("herdr", "--session", "default", *args)
    workspace = next(w["workspace_id"] for w in json.loads(cli("workspace", "list"))["result"]["workspaces"] if w["label"] == "ft")
    pane = json.loads(cli("tab", "create", "--workspace", workspace, "--cwd", str(repo), "--label", "scratch-context-test", "--no-focus"))["result"]["root_pane"]["pane_id"]
    pane_open = True
    try:
        with tempfile.TemporaryDirectory(prefix="hs-ctx-", dir="/tmp") as temporary:
            root = Path(temporary)
            state, config = root / "s", root / "c"
            state.mkdir()
            config.mkdir()
            sockets = state / f"tmux-{os.geteuid()}"
            sockets.mkdir(mode=0o700)
            outer = root / "outer"
            workspace_socket = sockets / "shadowfax-herdr-workspace"
            minimal_socket = sockets / "shadowfax-herdr-scratch"
            rpc = root / "nvim"
            init = root / "init.lua"
            herdr_wrapper = root / "herdr"
            herdr_calls = root / "herdr-calls"
            herdr_wrapper.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$HERDR_TEST_CALLS"\nexec "$HERDR_TEST_BIN" "$@"\n')
            herdr_wrapper.chmod(0o700)
            init.write_text(
                "vim.opt.rtp:append(" + json.dumps(str(repo / "nvim")) + ")\n"
                "_G.context_events = {}\n"
                'vim.api.nvim_create_autocmd("User", { pattern = "HerdrScratchContext", callback = function(ev) table.insert(_G.context_events, ev.data) end })\n'
                # The runtime plugin is sourced automatically after this init.
            )
            config.joinpath("config.yaml").write_text(
                "cleanup: { enabled: true, ttl_hours: 24, interval_seconds: 60 }\n"
                'default_popup: { width: "80%", height: "80%" }\n'
                "scratches:\n"
                "  nvim:\n    command: " + json.dumps(["nvim", "-u", str(init), "--listen", str(rpc)]) + "\n"
                "    tmux_mode: workspace\n    tmux_prefix: ctrl+a\n"
                "  shell:\n    command: [\"/bin/sh\"]\n    tmux_mode: workspace\n    tmux_prefix: ctrl+a\n"
                "  minimal:\n    command: [\"sleep\", \"300\"]\n    tmux_mode: minimal\n"
            )
            env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_PLUGIN_") and k not in ("TMUX", "TMUX_PANE", "HERDR_SCRATCH_CONTEXT")}
            env.update(HERDR_PLUGIN_STATE_DIR=str(state), HERDR_PLUGIN_CONFIG_DIR=str(config),
                       HERDR_SCRATCH_SOURCE_PANE=pane, HERDR_SCRATCH_SOURCE_CWD="/wrong/snapshot",
                       HERDR_SCRATCH_NAME="nvim", HERDR_SCRATCH_PREFIX="C-a", TERM="xterm-256color", SHELL="/bin/sh",
                       HERDR_BIN_PATH=str(herdr_wrapper), HERDR_TEST_BIN=shutil.which("herdr"), HERDR_TEST_CALLS=str(herdr_calls))
            herdr_socket = env["HERDR_SOCKET_PATH"]
            prefix = hashlib.sha256(herdr_socket.encode()).hexdigest()[:12]
            name = lambda kind: f"hs/{kind}/{prefix}/{pane.replace(':', '-')}"
            context = lambda kind: state / "context" / (name(kind).replace("/", "-") + ".json")
            tmux = lambda socket, *args, **kwargs: run("tmux", "-S", str(socket), "-f", "/dev/null", *args, env=env, **kwargs)
            rpc_eval = lambda expression: run("nvim", "--server", str(rpc), "--remote-expr", expression, env=env)
            observed = lambda: json.loads(rpc_eval("luaeval('vim.json.encode({cwd=vim.fn.getcwd(), events=_G.context_events, pid=vim.fn.getpid()})')"))
            set_token = lambda value: cli("pane", "report-metadata", pane, "--source", "grove", "--token", "grove_worktree=" + value)
            outer_panes = {}

            def show(kind):
                previous_calls = herdr_calls.read_text().splitlines() if herdr_calls.exists() else []
                outer_panes[kind] = tmux(outer, "new-window", "-d", "-n", kind, "-P", "-F", "#{pane_id}",
                                          shlex.join(["env", "HERDR_SCRATCH_NAME=" + kind, binary, "run-popup"]))
                socket = minimal_socket if kind == "minimal" else workspace_socket
                wait_for(lambda: tmux(socket, "list-clients", "-t", "=" + name(kind), check=False), "attached " + kind)
                assert herdr_calls.read_text().splitlines() == previous_calls + ["pane get " + pane], "each open must resolve once"

            def hide(kind):
                socket = minimal_socket if kind == "minimal" else workspace_socket
                tmux(socket, "detach-client", "-s", "=" + name(kind))

            def reopen(expected_root, expected_source, changed):
                before = len(observed()["events"])
                show("nvim")
                wait_for(lambda: len(observed()["events"]) > before, "context User event")
                result = observed()
                assert result["pid"] == nvim_pid, "workspace replaced its running Neovim"
                assert result["events"][-1] == {"root": expected_root, "source_pane": pane, "changed": changed}, result
                data = json.loads(context("nvim").read_text())
                assert data["root"] == expected_root and data["root_source"] == expected_source, data
                hide("nvim")
                return result

            def new_window_root(kind, expected_root):
                before = set(tmux(workspace_socket, "list-windows", "-t", "=" + name(kind), "-F", "#{window_id}").splitlines())
                # Exercise the owner's actual prefix-c binding. An external
                # new-window CLI supplies its own caller cwd to tmux.
                tmux(outer, "send-keys", "-t", outer_panes[kind], "C-a", "c")
                window = wait_for(lambda: next(iter(set(tmux(workspace_socket, "list-windows", "-t", "=" + name(kind), "-F", "#{window_id}").splitlines()) - before), None), "new " + kind + " window")
                cwd = tmux(workspace_socket, "display-message", "-p", "-t", window, "#{pane_current_path}")
                assert cwd == expected_root, {"kind": kind, "cwd": cwd, "expected": expected_root}
                tmux(workspace_socket, "kill-window", "-t", window)

            try:
                # Prestart private servers without loading the owner's tmux config.
                for socket in (outer, workspace_socket, minimal_socket):
                    tmux(socket, "new-session", "-d", "-s", "fixture", "sleep 300")
                set_token(roots[0])
                show("nvim")
                wait_for(lambda: rpc.exists(), "Neovim RPC socket")
                result = observed()
                assert result["cwd"] == roots[0], result
                nvim_pid = result["pid"]
                data = json.loads(context("nvim").read_text())
                assert data == {"version": 1, "source_pane": pane, "root": roots[0], "root_source": "grove", "shown_at_ms": data["shown_at_ms"]}, data
                assert abs(data["shown_at_ms"] - time.time() * 1000) < 10000, data
                assert tmux(workspace_socket, "show-environment", "-t", "=" + name("nvim"), "HERDR_SCRATCH_CONTEXT") == "HERDR_SCRATCH_CONTEXT=" + str(context("nvim"))
                assert tmux(workspace_socket, "show-options", "-v", "focus-events") == "off"
                hide("nvim")
                print("OBSERVED: " + json.dumps({"context": data, "nvim": result}), flush=True)
                print("PASS: new Neovim starts at grove worktree; v1 context and session environment published; focus-events off", flush=True)

                set_token(roots[1])
                result = reopen(roots[1], "grove", True)
                assert result["cwd"] == roots[1]
                print("OBSERVED: " + json.dumps({"nvim_pid": result["pid"], "cwd": result["cwd"], "event": result["events"][-1]}), flush=True)
                # New windows consume attach-session's root for both scratches.
                show("shell")
                hide("shell")
                for kind in ("nvim", "shell"):
                    show(kind)
                    new_window_root(kind, roots[1])
                    hide(kind)
                set_token(roots[0])
                show("shell")
                assert tmux(workspace_socket, "display-message", "-p", "-t", "=" + name("shell") + ":", "#{pane_current_path}") == roots[1]
                new_window_root("shell", roots[0])
                hide("shell")
                set_token(roots[1])
                print("PASS: reopen changes cwd in the same Neovim; changed=true event; new windows in both scratches use root", flush=True)

                rpc_eval("luaeval('vim.api.nvim_set_current_dir(_A)', " + json.dumps(roots[0]) + ")")
                assert reopen(roots[1], "grove", False)["cwd"] == roots[0]
                before = len(observed()["events"])
                repeated = context("nvim").with_suffix(".tmp")
                repeated.write_bytes(context("nvim").read_bytes())
                repeated.replace(context("nvim"))
                wait_for(lambda: len(observed()["events"]) > before, "identical JSON rewrite event")
                assert observed()["events"][-1]["changed"] is False
                assert observed()["cwd"] == roots[0]
                print("PASS: manual cd survives same-root publication; changed=false event still fires", flush=True)

                # A shell predating HERDR_SCRATCH_CONTEXT discovers it via tmux.
                legacy_rpc = root / "legacy"
                legacy_init = root / "legacy.lua"
                legacy_init.write_text(init.read_text() + "vim.api.nvim_set_current_dir(" + json.dumps(roots[0]) + ")\n")
                legacy_window = tmux(workspace_socket, "new-window", "-d", "-t", "=" + name("nvim") + ":", "-P", "-F", "#{window_id}",
                                     shlex.join(["env", "-u", "HERDR_SCRATCH_CONTEXT", "nvim", "-u", str(legacy_init), "--listen", str(legacy_rpc)]))
                wait_for(lambda: legacy_rpc.exists(), "legacy Neovim socket")
                legacy_eval = lambda expr: run("nvim", "--server", str(legacy_rpc), "--remote-expr", expr, env=env)
                assert legacy_eval("getcwd()") == roots[0], "VimEnter must preserve startup cwd"
                cli("pane", "report-metadata", pane, "--source", "grove", "--clear-token", "grove_worktree")
                pane_cwd = json.loads(cli("pane", "get", pane))["result"]["pane"]["foreground_cwd"]
                assert reopen(pane_cwd, "cwd", True)["cwd"] == pane_cwd
                wait_for(lambda: legacy_eval("getcwd()") == pane_cwd, "tmux environment fallback watcher")
                tmux(workspace_socket, "kill-window", "-t", legacy_window)
                set_token(str(root / "missing"))
                assert reopen(pane_cwd, "cwd", False)["cwd"] == pane_cwd
                set_token(str(init))
                assert reopen(pane_cwd, "cwd", False)["cwd"] == pane_cwd
                print("PASS: missing, stale and file tokens fall back to live pane cwd; legacy environment lookup; VimEnter preserves cwd", flush=True)

                set_token(roots[0])
                show("minimal")
                first_pid = tmux(minimal_socket, "display-message", "-p", "-t", "=" + name("minimal") + ":", "#{pane_pid}")
                hide("minimal")
                set_token(roots[1])
                show("minimal")
                assert tmux(minimal_socket, "display-message", "-p", "-t", "=" + name("minimal") + ":", "#{pane_pid}") != first_pid
                assert json.loads(context("minimal").read_text())["root"] == roots[1]
                hide("minimal")
                print("PASS: minimal mode recreates on resolved-root change", flush=True)

                cli("pane", "close", pane)
                pane_open = False
                report = json.loads(run(binary, "cleanup", "--apply", env=env))
                assert report["removed"] == 3, report
                assert all(not context(kind).exists() for kind in ("nvim", "shell", "minimal"))
                print("PASS: cleanup reaps all three test sessions and their context files", flush=True)
            finally:
                for socket in (outer, workspace_socket, minimal_socket):
                    tmux(socket, "kill-server", check=False)
    finally:
        if pane_open:
            cli("pane", "close", pane)


if __name__ == "__main__":
    main()

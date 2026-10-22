//! Exercise the local/remote process boundary without an SSH server or live
//! Herdr. Each invocation has a private environment, fake CLI peers and state.

use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(ssh: Option<Value>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "scratch-ssh-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for directory in ["config", "state", "bin", "home", "xdg"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(
            root.join("config/config.yaml"),
            r#"
default_popup: { width: "70%", height: "95%" }
cleanup: { enabled: false, interval_seconds: 5 }
scratches:
  shell:
    command: ["/bin/sh"]
    tmux_mode: workspace
    tmux_prefix: ctrl+a
    key: alt+0
"#,
        )
        .unwrap();
        let mut pane = json!({"pane_id":"w2:p3", "cwd":"/remote/project", "foreground_cwd":"/remote/foreground", "tokens":{"grove_worktree":"f".repeat(64)}});
        if let Some(ssh) = ssh {
            pane["ssh"] = ssh;
        }
        fs::write(
            root.join("pane.json"),
            json!({"result":{"pane":pane}}).to_string(),
        )
        .unwrap();
        for (name, script) in [
            (
                "herdr",
                r#"#!/bin/sh
case "$1 $2" in
  'pane current'|'pane get') cat "$FIXTURE_ROOT/pane.json" ;;
  'pane layout') printf '%s\n' '{"result":{"layout":{"area":{"x":30,"width":482}}}}' ;;
  'plugin pane') printf '%s\n' "$@" > "$FIXTURE_ROOT/popup.args" ;;
  'ssh run')
    printf '%s\n' "$@" > "$FIXTURE_ROOT/ssh.args"
    printf '%s\n' "$$" > "$FIXTURE_ROOT/exec.pid"
    exit 37 ;;
  *) exit 91 ;;
esac
"#,
            ),
            (
                "tmux",
                r#"#!/bin/sh
printf '%s\n' "$@" >> "$FIXTURE_ROOT/tmux.args"
for arg in "$@"; do
  if [ "$arg" = has-session ]; then exit 1; fi
done
"#,
            ),
        ] {
            let path = root.join("bin").join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr-scratch"));
        command
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("xdg"))
            .env("FIXTURE_ROOT", &self.root)
            .env("HERDR_BIN_PATH", self.root.join("bin/herdr"))
            .env("HERDR_PLUGIN_CONFIG_DIR", self.root.join("config"))
            .env("HERDR_PLUGIN_STATE_DIR", self.root.join("state"))
            .env("HERDR_SOCKET_PATH", "/fixture/herdr.sock");
        command
    }

    fn args(&self, file: &str) -> Vec<String> {
        fs::read_to_string(self.root.join(file))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::write(self.root.join("state/cleanup.stop"), []);
        // Remote startup owns an independent worker. Let it release its lock
        // before deleting this fixture's private state, never any live worker.
        if let Ok(lock) = fs::File::open(self.root.join("state/cleanup-worker.lock")) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                // SAFETY: this owned descriptor only addresses the test lock.
                if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn toggle_preserves_local_popup_argv_and_routes_live_and_stale_sources_from_both_entrypoints() {
    let local = [
        "plugin",
        "pane",
        "open",
        "--plugin",
        "shadowfax.scratch",
        "--entrypoint",
        "scratch",
        "--placement",
        "popup",
        "--width",
        "70%",
        "--height",
        "95%",
        "--env",
        "HERDR_SCRATCH_NAME=shell",
        "--env",
        "HERDR_SCRATCH_SOURCE_PANE=w2:p3",
        "--env",
        "HERDR_SCRATCH_SOURCE_CWD=/remote/foreground",
        "--env",
        "HERDR_SCRATCH_PREFIX=C-a",
        "--focus",
    ];
    for (metadata, remote) in [
        (None, false),
        (
            Some(json!({"host":"studio", "state":"live", "link_id":"link1", "local_cwd":"/local"})),
            true,
        ),
        (Some(json!({"host":"studio", "state":"stale"})), true),
        (Some(json!({"host":"studio", "state":"unknown"})), false),
    ] {
        let fixture = Fixture::new(metadata);
        for context in [false, true] {
            let mut command = fixture.command(&["toggle", "--scratch", "shell"]);
            if context {
                command.env(
                    "HERDR_PLUGIN_CONTEXT_JSON",
                    r#"{"focused_pane_id":"w2:p3","focused_pane_cwd":"/remote/foreground"}"#,
                );
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut expected = local.map(str::to_owned).to_vec();
            if remote {
                expected.extend(["--env".into(), "HERDR_SCRATCH_SSH_HOST=studio".into()]);
            }
            assert_eq!(fixture.args("popup.args"), expected);
        }
    }
}

#[test]
fn local_popup_replaces_itself_with_ssh_and_propagates_remote_exit_status() {
    let fixture = Fixture::new(None);
    // Invalid local configuration must never be read by the remote dispatcher.
    fs::write(fixture.root.join("config/config.yaml"), "invalid: [").unwrap();
    let mut command = fixture.command(&["run-popup"]);
    command
        .env("HERDR_SCRATCH_SSH_HOST", "studio")
        .env("HERDR_SCRATCH_NAME", "shell")
        .env("HERDR_SCRATCH_SOURCE_PANE", "w2:p3")
        .env("HERDR_SCRATCH_FUTURE", "a=b 'quoted' $value")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(37),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("exec.pid"))
            .unwrap()
            .trim(),
        pid.to_string()
    );
    assert_eq!(
        fixture.args("ssh.args"),
        [
            "ssh",
            "run",
            "studio",
            "--env",
            "HERDR_SCRATCH_FUTURE=a=b 'quoted' $value",
            "--env",
            "HERDR_SCRATCH_NAME=shell",
            "--env",
            "HERDR_SCRATCH_SOURCE_PANE=w2:p3",
            "--env",
            "HERDR_PLUGIN_STATE_DIR=@state/shadowfax.scratch",
            "--env",
            "HERDR_PLUGIN_CONFIG_DIR=@root/plugin-config/shadowfax.scratch",
            "--",
            "herdr-scratch",
            "run-popup",
        ]
    );
    assert!(!fixture.root.join("state/cleanup-worker.lock").exists());
}

#[test]
fn cleanup_worker_detaches_its_session_only_on_the_remote() {
    for remote in [false, true] {
        let fixture = Fixture::new(None);
        // Give this worker a unique executable path so process inspection
        // cannot confuse it with workers belonging to another test or user.
        let binary = fixture.root.join("bin/worker-scratch");
        fs::copy(env!("CARGO_BIN_EXE_herdr-scratch"), &binary).unwrap();
        let command = fixture.command(&["cleanup-start"]);
        let mut launcher = Command::new(&binary);
        launcher
            .args(["cleanup-start"])
            .env_clear()
            .envs(command.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))));
        if remote {
            launcher.env("HERDR_SSH_PANES", "1");
        }
        assert!(launcher.status().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(5);
        let worker = loop {
            let output = Command::new("ps")
                .args(["-axo", "pid=,command="])
                .output()
                .unwrap();
            let processes = String::from_utf8_lossy(&output.stdout);
            if let Some(pid) = processes.lines().find_map(|line| {
                let line = line.trim();
                let (pid, argv) = line.split_once(char::is_whitespace)?;
                (argv.trim() == format!("{} cleanup-worker", binary.display()))
                    .then(|| pid.parse::<i32>().unwrap())
            }) {
                break pid;
            }
            assert!(Instant::now() < deadline, "worker did not start");
            thread::sleep(Duration::from_millis(10));
        };
        // SAFETY: these read-only calls query this fixture's worker and the
        // test process. Remote workers own a session; local workers inherit it.
        let session = unsafe { libc::getsid(worker) };
        let local_session = unsafe { libc::getsid(0) };
        assert!(session > 0);
        if remote {
            assert_eq!(session, worker);
            assert_ne!(session, local_session);
        } else {
            assert_eq!(session, local_session);
        }
    }
}

#[test]
fn remote_popup_resolves_missing_grove_handles_on_remote_cwd_and_starts_cleanup() {
    for remote in [false, true] {
        let fixture = Fixture::new(Some(json!({"host":"studio", "state":"live"})));
        let mut command = fixture.command(&["run-popup"]);
        command
            .env("HERDR_SCRATCH_NAME", "shell")
            .env("HERDR_SCRATCH_SOURCE_PANE", "w2:p3");
        if remote {
            command
                .env("HERDR_SSH_PANES", "1")
                .env("HERDR_SCRATCH_SSH_HOST", "studio")
                .env("HERDR_SSH_PANES_SERVER_ID", "server-1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!fixture.root.join("ssh.args").exists());
        let publications: Vec<_> = fs::read_dir(fixture.root.join("state/context"))
            .unwrap()
            .collect();
        assert_eq!(publications.len(), 1);
        let publication: Value =
            serde_json::from_slice(&fs::read(publications[0].as_ref().unwrap().path()).unwrap())
                .unwrap();
        assert_eq!(publication["root"], "/remote/foreground");
        assert_eq!(publication["root_source"], "cwd");
        assert_eq!(publication["source_pane"], "w2:p3");
        assert!(fixture
            .args("tmux.args")
            .iter()
            .any(|arg| arg == "/remote/foreground"));
        if remote {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !fixture.root.join("state/cleanup-status.json").exists()
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                fixture.root.join("state/cleanup-status.json").exists(),
                "remote worker did not start"
            );
        } else {
            assert!(!fixture.root.join("state/cleanup-worker.lock").exists());
        }
    }
}

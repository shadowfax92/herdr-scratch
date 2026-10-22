//! Exercise the local/remote process boundary without an SSH server or live
//! Herdr. Each invocation has a private environment, fake CLI peers and state.

use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
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
        let root = PathBuf::from("/tmp").join(format!(
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
    if [ "${FIXTURE_SSH_EXIT:-37}" != 0 ]; then printf '%s\n' 'hint: SSH link down' >&2; fi
    exit "${FIXTURE_SSH_EXIT:-37}" ;;
  *) exit 91 ;;
esac
"#,
            ),
            (
                "tmux",
                r#"#!/bin/sh
printf '%s\n' "$@" >> "$FIXTURE_ROOT/tmux.args"
printf '%s\n' "$TMUX_TMPDIR" >> "$FIXTURE_ROOT/tmux.tmpdir"
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

    fn stop_worker(&self, state: &std::path::Path) {
        let _ = fs::write(state.join("cleanup.stop"), []);
        if let Ok(lock) = fs::File::open(state.join("cleanup-worker.lock")) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                // SAFETY: this descriptor addresses only a fixture lock.
                if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Include alternate long state roots used by the socket regressions.
        for entry in fs::read_dir(&self.root).unwrap().flatten() {
            if entry.path().is_dir() {
                self.stop_worker(&entry.path());
            }
        }
        if let Ok(args) = fs::read_to_string(self.root.join("tmux.args")) {
            let args: Vec<_> = args.lines().collect();
            for pair in args.windows(2).filter(|pair| pair[0] == "-S") {
                if let Some(directory) = std::path::Path::new(pair[1]).parent() {
                    // Empty fixture directories only; never remove a socket.
                    let _ = fs::remove_dir(directory);
                }
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
fn local_popup_waits_for_ssh_and_reports_remote_failure() {
    let fixture = Fixture::new(None);
    // Invalid local configuration must never be read by the remote dispatcher.
    fs::write(fixture.root.join("config/config.yaml"), "invalid: [").unwrap();
    let mut command = fixture.command(&["run-popup"]);
    command
        .env("HERDR_SCRATCH_SSH_HOST", "studio")
        .env("HERDR_SCRATCH_NAME", "shell")
        .env("HERDR_SCRATCH_SOURCE_PANE", "w2:p3")
        .env("HERDR_SCRATCH_FUTURE", "a=b 'quoted' $value")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("scratch on studio failed (exit 37)"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("hint: SSH link down"));
    assert_ne!(
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
fn remote_popup_uses_short_sockets_and_cleanup_scans_the_same_servers() {
    let fixture = Fixture::new(None);
    let state = fixture.root.join("a".repeat(100));
    let output = fixture
        .command(&["run-popup"])
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .env("HERDR_SSH_PANES", "1")
        .env("HERDR_SCRATCH_NAME", "shell")
        .env("HERDR_SCRATCH_SOURCE_PANE", "w2:p3")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args = fixture.args("tmux.args");
    let sockets: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "-S")
        .map(|pair| &pair[1])
        .collect();
    assert!(
        !sockets.is_empty(),
        "remote tmux must use a short explicit socket: {args:?}"
    );
    let socket = PathBuf::from(sockets[0]);
    assert!(socket.starts_with("/tmp"));
    assert!(socket.as_os_str().as_encoded_bytes().len() <= 103);
    assert!(sockets.iter().all(|path| *path == sockets[0]));
    let directory = socket.parent().unwrap();
    assert_eq!(
        fs::metadata(directory).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    // A socket-shaped fixture makes discovery observable without launching a
    // tmux server. Cleanup and reap must query the same endpoint as the popup.
    fs::write(&socket, []).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !state.join("cleanup-status.json").exists() {
        assert!(Instant::now() < deadline, "worker did not start");
        thread::sleep(Duration::from_millis(10));
    }
    fixture.stop_worker(&state);
    let config = fixture.root.join("config/config.yaml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("enabled: false", "enabled: true"),
    )
    .unwrap();
    for action in ["cleanup", "reap"] {
        assert!(fixture
            .command(&[action])
            .env("HERDR_PLUGIN_STATE_DIR", &state)
            .env("HERDR_SSH_PANES", "1")
            .output()
            .unwrap()
            .status
            .success());
    }
    let args = fixture.args("tmux.args");
    assert_eq!(args.iter().filter(|arg| *arg == "list-sessions").count(), 2);
    fs::remove_file(state.join("cleanup-status.json")).unwrap();
    assert!(fixture
        .command(&["cleanup-start"])
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .env("HERDR_SSH_PANES", "1")
        .output()
        .unwrap()
        .status
        .success());
    let deadline = Instant::now() + Duration::from_secs(3);
    while !state.join("cleanup-status.json").exists() {
        assert!(Instant::now() < deadline, "worker did not scan");
        thread::sleep(Duration::from_millis(10));
    }
    fixture.stop_worker(&state);
    assert_eq!(
        fixture
            .args("tmux.args")
            .iter()
            .filter(|arg| *arg == "list-sessions")
            .count(),
        3
    );
    fs::remove_file(&socket).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
fn successful_remote_popup_closes_without_an_extra_prompt() {
    let fixture = Fixture::new(None);
    let output = fixture
        .command(&["run-popup"])
        .env("HERDR_SCRATCH_SSH_HOST", "studio")
        .env("FIXTURE_SSH_EXIT", "0")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn popup_errors_remain_visible_in_a_tty_until_enter() {
    for mode in ["local", "remote", "ssh"] {
        let fixture = Fixture::new(None);
        let mut command = fixture.command(&["run-popup"]);
        let expected = if mode == "ssh" {
            command.env("HERDR_SCRATCH_SSH_HOST", "studio");
            "scratch on studio failed (exit 37)"
        } else {
            if mode == "remote" {
                command.env("HERDR_SSH_PANES", "1");
            }
            "HERDR_SCRATCH_NAME is missing"
        };
        let (mut master, slave) = open_pty();
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut text = String::new();
        while !text.contains("Press Enter to close") {
            let mut buffer = [0; 4096];
            if let Ok(count) = master.read(&mut buffer) {
                text.push_str(&String::from_utf8_lossy(&buffer[..count]));
            }
            assert!(Instant::now() < deadline, "{mode}: {text}");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(text.contains(expected), "{mode}: {text}");
        assert!(
            child.try_wait().unwrap().is_none(),
            "popup exited before Enter"
        );
        master.write_all(b"\n").unwrap();
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("popup did not exit after Enter");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(child.wait().unwrap().code(), Some(1));
    }
}

fn open_pty() -> (fs::File, fs::File) {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty writes two descriptors; optional name/termios/size are
    // null. Each returned descriptor is immediately transferred to one File.
    unsafe {
        assert_eq!(
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ),
            0
        );
        assert_ne!(libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC), -1);
        assert_ne!(libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC), -1);
        assert_ne!(libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK), -1);
        (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave))
    }
}

#[test]
fn local_socket_path_is_unchanged_and_overlong_paths_fail_before_tmux() {
    let fixture = Fixture::new(None);
    let popup = || {
        let mut command = fixture.command(&["run-popup"]);
        command
            .env("HERDR_SCRATCH_NAME", "shell")
            .env("HERDR_SCRATCH_SOURCE_PANE", "w2:p3");
        command
    };
    assert!(popup().output().unwrap().status.success());
    assert_eq!(
        fixture.args("tmux.tmpdir")[0],
        fixture.root.join("state").to_str().unwrap()
    );
    assert_eq!(
        &fixture.args("tmux.args")[..2],
        ["-L", "shadowfax-herdr-workspace"]
    );
    fs::remove_file(fixture.root.join("tmux.args")).unwrap();
    let state = fixture.root.join("é".repeat(50));
    let output = popup()
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tmux socket path"), "{stderr}");
    assert!(
        stderr.contains("bytes") && stderr.contains("103"),
        "{stderr}"
    );
    assert!(stderr.contains(state.to_str().unwrap()), "{stderr}");
    assert!(!fixture.root.join("tmux.args").exists());
}

#[test]
fn valid_local_minimal_socket_is_not_rejected_for_the_unused_workspace_path() {
    let fixture = Fixture::new(None);
    // SAFETY: geteuid has no pointers or side effects.
    let uid = unsafe { libc::geteuid() };
    let suffix = format!("/tmux-{uid}/shadowfax-herdr-scratch");
    let canonical_root = fs::canonicalize(&fixture.root).unwrap();
    let state = fixture.root.join(
        "x".repeat(103 - suffix.len() - canonical_root.as_os_str().as_encoded_bytes().len() - 1),
    );
    let config = fixture.root.join("config/config.yaml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("tmux_mode: workspace", "tmux_mode: minimal"),
    )
    .unwrap();
    let output = fixture
        .command(&["run-popup"])
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .env("HERDR_SCRATCH_NAME", "shell")
        .env("HERDR_SCRATCH_SOURCE_PANE", "w2:p3")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.args("tmux.tmpdir")[0], state.to_str().unwrap());
    let directory = state.join(format!("tmux-{uid}"));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("shadowfax-herdr-scratch"), []).unwrap();
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("enabled: false", "enabled: true"),
    )
    .unwrap();
    assert!(fixture
        .command(&["cleanup"])
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(
        fixture
            .args("tmux.args")
            .iter()
            .filter(|arg| *arg == "list-sessions")
            .count(),
        1
    );
}

#[test]
fn local_socket_guard_measures_the_canonical_tmux_tmpdir() {
    let fixture = Fixture::new(None);
    // A short symlink reproduces macOS /tmp -> /private/tmp on every Unix.
    // SAFETY: geteuid has no pointers or side effects.
    let suffix = format!("/tmux-{}/shadowfax-herdr-workspace", unsafe {
        libc::geteuid()
    });
    let canonical_root = fs::canonicalize(&fixture.root).unwrap();
    let target = canonical_root.join(
        "x".repeat(104 - suffix.len() - canonical_root.as_os_str().as_encoded_bytes().len() - 1),
    );
    fs::create_dir(&target).unwrap();
    let alias = fixture.root.join("s");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let output = fixture
        .command(&["run-popup"])
        .env("HERDR_PLUGIN_STATE_DIR", &alias)
        .env("HERDR_SCRATCH_NAME", "shell")
        .env("HERDR_SCRATCH_SOURCE_PANE", "w2:p3")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("104 bytes") && stderr.contains(target.to_str().unwrap()),
        "{stderr}"
    );
    assert!(!fixture.root.join("tmux.args").exists());
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

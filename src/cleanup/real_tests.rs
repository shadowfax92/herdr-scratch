//! Opt-in real tmux checks. The clock is injected at the same sweep interface
//! used by the worker so TTL tests exercise real enumeration and guarded removal
//! without waiting a day or exposing a test-clock override in the application.

use super::*;
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    socket: PathBuf,
    name: String,
    remote: bool,
}

impl Fixture {
    fn new() -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(format!(
            "/tmp/sc-{}-{suffix}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let [socket, _] = crate::tmux::server_socket_paths(&root).unwrap();
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let source = root.join("missing-herdr.sock");
        let name = crate::tmux::session_name("shell", "w1:p1", source.to_str().unwrap());
        let fixture = Self {
            root,
            socket,
            name,
            remote: false,
        };
        fixture.tmux(&["new-session", "-d", "-s", "foreign", "sleep 300"]);
        fixture.tmux(&["new-session", "-d", "-s", &fixture.name, "sleep 300"]);
        fixture.tmux(&[
            "set-option",
            "-t",
            &fixture.name,
            "@herdr_source_pane",
            "w1:p1",
        ]);
        fixture.tmux(&["set-option", "-t", &fixture.name, "@herdr_env_version", "1"]);
        fixture.tmux(&[
            "set-environment",
            "-t",
            &fixture.name,
            "HERDR_SOCKET_PATH",
            source.to_str().unwrap(),
        ]);
        fixture
    }

    fn remote() -> Self {
        let mut fixture = Self::new();
        let name = crate::tmux::session_name("shell", "w1:p1", "test-server");
        fixture.tmux(&["rename-session", "-t", &fixture.name, &name]);
        fixture.name = name;
        fixture.remote = true;
        fixture.tmux(&[
            "set-environment",
            "-t",
            &fixture.name,
            "HERDR_SSH_PANES_SERVER_ID",
            "test-server",
        ]);
        fixture
    }

    fn command(&self) -> Command {
        Self::command_at(&self.socket)
    }

    fn add_scratch(&self, source: &str) -> String {
        let socket = self.root.join("missing-herdr.sock");
        let name = crate::tmux::session_name("shell", source, socket.to_str().unwrap());
        self.tmux(&[
            "new-session",
            "-d",
            "-s",
            &name,
            "sleep 300",
            ";",
            "set-option",
            "-t",
            &name,
            "@herdr_source_pane",
            source,
            ";",
            "set-option",
            "-t",
            &name,
            "@herdr_env_version",
            "1",
            ";",
            "set-environment",
            "-t",
            &name,
            "HERDR_SOCKET_PATH",
            socket.to_str().unwrap(),
        ]);
        name
    }

    fn command_at(socket: &Path) -> Command {
        let mut command = Command::new("tmux");
        command
            .arg("-S")
            .arg(socket)
            .args(["-f", "/dev/null"])
            .env_remove("TMUX");
        command
    }

    fn tmux(&self, args: &[&str]) {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn attach(&self) -> Child {
        let mut child = self
            .command()
            .args(["-C", "attach-session", "-t", &self.name])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if backend::sessions(&self.root).unwrap()[0].attached {
                return child;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("test client failed to attach");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for socket in crate::tmux::server_socket_paths(&self.root).unwrap() {
            let _ = Self::command_at(&socket).arg("kill-server").output();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn reaping_one_mode_preserves_context_for_the_other_mode() {
    let fixture = Fixture::new();
    let [_, workspace] = crate::tmux::server_socket_paths(&fixture.root).unwrap();
    let source = fixture.root.join("missing-herdr.sock");
    let output = Fixture::command_at(&workspace)
        .args([
            "new-session",
            "-d",
            "-s",
            "foreign",
            "sleep 300",
            ";",
            "new-session",
            "-d",
            "-s",
            &fixture.name,
            "sleep 300",
            ";",
            "set-option",
            "-t",
            &fixture.name,
            "@herdr_source_pane",
            "w1:p1",
            ";",
            "set-option",
            "-t",
            &fixture.name,
            "@herdr_env_version",
            "1",
            ";",
            "set-environment",
            "-t",
            &fixture.name,
            "HERDR_SOCKET_PATH",
            source.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let context = crate::context::context_path(&fixture.root, &fixture.name);
    fs::create_dir_all(context.parent().unwrap()).unwrap();
    fs::write(&context, b"{\"version\":1}").unwrap();
    let mut client = fixture.attach();
    let now = now().unwrap();
    let policy = CleanupConfig::default();
    let first = sweep(&fixture.root, &policy, true, false, now + 86400).unwrap();
    // Reap the hidden workspace, while the same-name minimal session remains
    // attached. Both modes intentionally share C2's filename contract.
    let preserved = context.exists();
    client.kill().unwrap();
    client.wait().unwrap();
    assert_eq!(first.removed, 1, "{:?}", first.errors);
    assert!(preserved, "removed the still-owned shared context");
    let second = sweep(&fixture.root, &policy, true, false, now + 172800).unwrap();
    assert_eq!(second.removed, 1, "{:?}", second.errors);
    assert!(!context.exists(), "last owner must remove its context");
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn real_ttl_sweep_preserves_recent_and_foreign_sessions() {
    let fixture = Fixture::new();
    let now = now().unwrap();
    let policy = CleanupConfig::default();
    assert_eq!(
        sweep(&fixture.root, &policy, true, false, now)
            .unwrap()
            .removed,
        0
    );
    let preview = sweep(&fixture.root, &policy, false, false, now + 86400).unwrap();
    assert_eq!(preview.candidates.len(), 1);
    assert_eq!(preview.removed, 0);
    assert_eq!(backend::sessions(&fixture.root).unwrap().len(), 1);
    let report = sweep(&fixture.root, &policy, true, false, now + 86400).unwrap();
    assert_eq!(report.removed, 1, "{:?}", report.errors);
    assert!(backend::sessions(&fixture.root).unwrap().is_empty());
    fixture.tmux(&["has-session", "-t", "foreign"]);
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn unavailable_private_server_does_not_block_the_other_server() {
    let fixture = Fixture::new();
    let [_, unavailable] = crate::tmux::server_socket_paths(&fixture.root).unwrap();
    fs::write(unavailable, b"stale socket").unwrap();
    let report = sweep(
        &fixture.root,
        &CleanupConfig::default(),
        true,
        false,
        now().unwrap() + 86400,
    )
    .unwrap();
    assert_eq!(report.removed, 1, "{:?}", report.errors);
    assert!(!report.errors.is_empty());
    fixture.tmux(&["has-session", "-t", "foreign"]);
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn real_attachment_renews_ttl_and_blocks_stale_removal() {
    let fixture = Fixture::new();
    let old = backend::sessions(&fixture.root).unwrap().remove(0);
    let mut client = fixture.attach();
    assert!(!backend::remove(&old, Reason::Expired).unwrap());
    let now = now().unwrap();
    let policy = CleanupConfig::default();
    assert_eq!(
        sweep(&fixture.root, &policy, true, false, now + 86400)
            .unwrap()
            .removed,
        0
    );
    client.kill().unwrap();
    client.wait().unwrap();
    for _ in 0..100 {
        if !backend::sessions(&fixture.root).unwrap()[0].attached {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        sweep(&fixture.root, &policy, true, false, now + 86401)
            .unwrap()
            .removed,
        0
    );
    let report = sweep(&fixture.root, &policy, true, false, now + 172800).unwrap();
    assert_eq!(report.removed, 1, "{:?}", report.errors);
}

/// Contract-only peer: the cleanup worker must not depend on the fork's build.
/// Changing its pane snapshot models a cold restore across sweeps that reload
/// persistent ownership state, just as a newly started worker does.
struct HerdrFixture {
    panes: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    next_panes:
        std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Vec<serde_json::Value>>>>,
    remote: bool,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HerdrFixture {
    fn new(fixture: &Fixture) -> Self {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;
        let listener = UnixListener::bind(fixture.root.join("missing-herdr.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let panes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let next_panes =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (snapshot, stopping) = (panes.clone(), stop.clone());
        let next = next_panes.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut line = String::new();
                        BufReader::new(&stream).read_line(&mut line).unwrap();
                        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                        let mut reply = serde_json::json!({"id": request["id"]});
                        if request["method"] == "pane.list" {
                            let panes = next
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or_else(|| snapshot.lock().unwrap().clone());
                            reply["result"] = serde_json::json!({"panes": panes});
                        } else {
                            assert_eq!(request["method"], "pane.current");
                            reply["error"] = serde_json::json!({"code": "pane_not_found"});
                        }
                        writeln!(stream, "{reply}").unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("Herdr fixture: {error}"),
                }
            }
        });
        Self {
            panes,
            next_panes,
            remote: fixture.remote,
            stop,
            worker: Some(worker),
        }
    }

    fn set_panes(&self, terminals: &[(&str, &str)]) {
        *self.panes.lock().unwrap() = self.pane_values(terminals);
    }

    fn pane_values(&self, terminals: &[(&str, &str)]) -> Vec<serde_json::Value> {
        terminals
            .iter()
            .map(|(pane, terminal)| {
                let mut value = serde_json::json!({"pane_id": pane, "terminal_id": terminal});
                if self.remote {
                    value["ssh"] =
                        serde_json::json!({"host":"studio", "link_id":"new-link", "state":"live"});
                }
                value
            })
            .collect()
    }

    fn queue_panes(&self, snapshots: &[&[(&str, &str)]]) {
        *self.next_panes.lock().unwrap() = snapshots
            .iter()
            .map(|panes| self.pane_values(panes))
            .collect();
    }
}

impl Drop for HerdrFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn restart_readopts_restored_source_and_later_close_reaps() {
    for remote in [false, true] {
        let fixture = if remote {
            Fixture::remote()
        } else {
            Fixture::new()
        };
        let herdr = HerdrFixture::new(&fixture);
        let policy = CleanupConfig::default();
        let now = now().unwrap();
        herdr.set_panes(&[("w1:p1", "old-owner"), ("w1:p2", "old-keeper")]);
        assert_eq!(
            sweep(&fixture.root, &policy, true, false, now)
                .unwrap()
                .removed,
            0
        );
        herdr.set_panes(&[("w1:p1", "new-owner"), ("w1:p2", "new-keeper")]);
        let restarted = sweep(&fixture.root, &policy, true, false, now + 1).unwrap();
        assert!(
            restarted.candidates.is_empty(),
            "restored scratch is not closed"
        );
        fixture.tmux(&["has-session", "-t", &fixture.name]);
        // Each sweep reloads the cache, as an independently restarted worker does.
        assert_eq!(
            sweep(&fixture.root, &policy, true, false, now + 2)
                .unwrap()
                .removed,
            0
        );
        herdr.set_panes(&[("w1:p2", "new-keeper")]);
        let closed = sweep(&fixture.root, &policy, true, false, now + 3).unwrap();
        assert_eq!(closed.removed, 1, "{:?}", closed.errors);
        assert_eq!(closed.candidates[0].reason, Reason::SourceClosed);
    }
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn restart_missing_source_stays_unknown_across_sweeps_until_ttl() {
    let fixture = Fixture::new();
    let herdr = HerdrFixture::new(&fixture);
    let policy = CleanupConfig::default();
    let now = now().unwrap();
    herdr.set_panes(&[("w1:p1", "old-owner"), ("w1:p2", "old-keeper")]);
    sweep(&fixture.root, &policy, true, false, now).unwrap();
    herdr.set_panes(&[("w1:p2", "new-keeper")]);
    for offset in [1, 2, 86399] {
        let report = sweep(&fixture.root, &policy, true, false, now + offset).unwrap();
        assert!(report.candidates.is_empty());
        fixture.tmux(&["has-session", "-t", &fixture.name]);
    }
    let expired = sweep(&fixture.root, &policy, true, false, now + 86400).unwrap();
    assert_eq!(expired.removed, 1);
    assert_eq!(expired.candidates[0].reason, Reason::Expired);
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn same_instance_last_scratch_owner_closes_and_unreachable_host_is_unknown() {
    let fixture = Fixture::new();
    let herdr = HerdrFixture::new(&fixture);
    let policy = CleanupConfig::default();
    let now = now().unwrap();
    herdr.set_panes(&[("w1:p1", "owner"), ("w1:p2", "keeper")]);
    sweep(&fixture.root, &policy, true, false, now).unwrap();
    drop(herdr);
    let offline = sweep(&fixture.root, &policy, true, false, now + 1).unwrap();
    assert!(offline.candidates.is_empty());
    assert!(!offline.errors.is_empty());
    fs::remove_file(fixture.root.join("missing-herdr.sock")).unwrap();
    let herdr = HerdrFixture::new(&fixture);
    herdr.set_panes(&[("w1:p2", "keeper")]);
    let closed = sweep(&fixture.root, &policy, true, false, now + 2).unwrap();
    assert_eq!(closed.removed, 1);
    assert_eq!(closed.candidates[0].reason, Reason::SourceClosed);
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn restart_during_source_closure_recheck_keeps_and_readopts_scratch() {
    let fixture = Fixture::new();
    let herdr = HerdrFixture::new(&fixture);
    let policy = CleanupConfig::default();
    let now = now().unwrap();
    herdr.set_panes(&[("w1:p1", "old-owner"), ("w1:p2", "old-keeper")]);
    sweep(&fixture.root, &policy, true, false, now).unwrap();
    herdr.set_panes(&[("w1:p1", "new-owner"), ("w1:p2", "new-keeper")]);
    herdr.queue_panes(&[&[("w1:p2", "old-keeper")]]);
    let report = sweep(&fixture.root, &policy, true, false, now + 1).unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.candidates.len(), 1, "must reach the closure recheck");
    fixture.tmux(&["has-session", "-t", &fixture.name]);
    assert!(sweep(&fixture.root, &policy, true, false, now + 2)
        .unwrap()
        .candidates
        .is_empty());
    herdr.set_panes(&[("w1:p2", "new-keeper")]);
    assert_eq!(
        sweep(&fixture.root, &policy, true, false, now + 3)
            .unwrap()
            .removed,
        1
    );
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn legacy_cache_restart_and_delayed_restore_readopt_without_losing_preview_state() {
    let fixture = Fixture::new();
    let herdr = HerdrFixture::new(&fixture);
    let policy = CleanupConfig::default();
    let now = now().unwrap();
    herdr.set_panes(&[("w1:p1", "old-owner"), ("w1:p2", "old-keeper")]);
    sweep(&fixture.root, &policy, true, false, now).unwrap();
    let path = fixture.root.join("cleanup-state.json");
    let mut legacy: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("hosts");
    for record in legacy["records"].as_object_mut().unwrap().values_mut() {
        record.as_object_mut().unwrap().remove("generation");
    }
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(&path, &bytes).unwrap();
    herdr.set_panes(&[("w1:p2", "new-keeper")]);
    assert!(sweep(&fixture.root, &policy, false, false, now + 1)
        .unwrap()
        .candidates
        .is_empty());
    assert_eq!(fs::read(&path).unwrap(), bytes, "preview changed the cache");
    for offset in [2, 3] {
        assert!(sweep(&fixture.root, &policy, true, false, now + offset)
            .unwrap()
            .candidates
            .is_empty());
    }
    herdr.set_panes(&[("w1:p1", "new-owner"), ("w1:p2", "new-keeper")]);
    assert!(sweep(&fixture.root, &policy, true, false, now + 4)
        .unwrap()
        .candidates
        .is_empty());
    herdr.set_panes(&[("w1:p2", "new-keeper")]);
    assert_eq!(
        sweep(&fixture.root, &policy, true, false, now + 5)
            .unwrap()
            .removed,
        1
    );
}

#[test]
#[ignore = "requires tmux; allocates only isolated private sessions"]
fn restart_keeps_owner_first_observed_during_outage_until_ttl() {
    let fixture = Fixture::new();
    let herdr = HerdrFixture::new(&fixture);
    let policy = CleanupConfig::default();
    let now = now().unwrap();
    herdr.set_panes(&[("w1:p1", "old-owner"), ("w1:p2", "old-keeper")]);
    sweep(&fixture.root, &policy, true, false, now).unwrap();
    drop(herdr);
    let pending = fixture.add_scratch("w1:p3");
    assert!(sweep(&fixture.root, &policy, true, false, now + 1)
        .unwrap()
        .candidates
        .is_empty());
    fs::remove_file(fixture.root.join("missing-herdr.sock")).unwrap();
    let herdr = HerdrFixture::new(&fixture);
    let unscanned = fixture.add_scratch("w1:p4");
    herdr.set_panes(&[("w1:p1", "new-owner"), ("w1:p2", "new-keeper")]);
    for offset in [2, 3] {
        let report = sweep(&fixture.root, &policy, true, false, now + offset).unwrap();
        assert!(
            report.candidates.is_empty(),
            "an outage-discovered owner is unknown after restart"
        );
        fixture.tmux(&["has-session", "-t", &pending]);
        fixture.tmux(&["has-session", "-t", &unscanned]);
    }
    // Once the new generation is known, newly discovered scratches whose
    // sources really closed must retain the ordinary closure behavior.
    let fresh = fixture.add_scratch("w1:p5");
    let closed = sweep(&fixture.root, &policy, true, false, now + 4).unwrap();
    assert_eq!(closed.removed, 1);
    assert_eq!(closed.candidates[0].session, fresh);
    assert_eq!(closed.candidates[0].reason, Reason::SourceClosed);
    assert!(sweep(&fixture.root, &policy, true, false, now + 86399)
        .unwrap()
        .candidates
        .is_empty());
    let expired = sweep(&fixture.root, &policy, true, false, now + 86402).unwrap();
    assert_eq!(expired.removed, 3);
    assert!(expired
        .candidates
        .iter()
        .all(|candidate| candidate.reason == Reason::Expired));
}

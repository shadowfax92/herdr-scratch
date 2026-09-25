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
        let [socket, _] = crate::tmux::server_socket_paths(&root);
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let source = root.join("missing-herdr.sock");
        let name = crate::tmux::session_name("shell", "w1:p1", source.to_str().unwrap());
        let fixture = Self { root, socket, name };
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

    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command
            .arg("-S")
            .arg(&self.socket)
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
        let _ = self.command().arg("kill-server").output();
        let _ = fs::remove_dir_all(&self.root);
    }
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
    let [_, unavailable] = crate::tmux::server_socket_paths(&fixture.root);
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

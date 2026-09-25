//! The menu dispatches asynchronously. These CLI checks ensure the child reports
//! its actual cleanup result, including failures, through the Herdr notification
//! boundary. Each process has private configuration, state, and a fake Herdr.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "scratch-reap-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("config")).unwrap();
        fs::create_dir(root.join("state")).unwrap();
        fs::write(
            root.join("config/config.yaml"),
            include_str!("../config.default.yaml"),
        )
        .unwrap();
        let herdr = root.join("herdr");
        fs::write(
            &herdr,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.notification\"\n",
        )
        .unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }

    fn run(&self, command: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_herdr-scratch"))
            .args(command)
            .env("HERDR_PLUGIN_CONFIG_DIR", self.root.join("config"))
            .env("HERDR_PLUGIN_STATE_DIR", self.root.join("state"))
            .env("HERDR_BIN_PATH", self.root.join("herdr"))
            .output()
            .unwrap()
    }

    fn notification(&self) -> String {
        fs::read_to_string(self.root.join("herdr.notification")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn empty_reap_reports_completion_but_preview_does_not_notify() {
    let fixture = Fixture::new();
    assert!(fixture.run(&["cleanup"]).status.success());
    assert!(!fixture.root.join("herdr.notification").exists());
    let output = fixture.run(&["reap"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["removed"], 0);
    assert_eq!(
        fixture.notification(),
        "notification\nshow\nHerdr Scratch\n--body\nReaped 0 scratch sessions (checked 0/0).\n"
    );
}

#[test]
fn disabled_cleanup_and_invalid_configuration_are_visible() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("config/config.yaml"),
        include_str!("../config.default.yaml").replace("enabled: true", "enabled: false"),
    )
    .unwrap();
    assert!(fixture.run(&["reap"]).status.success());
    assert!(fixture.notification().contains("cleanup is disabled"));
    fs::write(fixture.root.join("config/config.yaml"), "invalid: [").unwrap();
    let output = fixture.run(&["reap"]);
    assert!(!output.status.success());
    assert!(fixture.notification().contains("Could not reap"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid scratch config"));
}

#[test]
fn unreadable_private_server_is_reported_as_a_failed_action() {
    let fixture = Fixture::new();
    // A stale endpoint must produce an explicit error, not a successful zero
    // count that would make the menu appear to have cleaned everything.
    let sockets = fixture.root.join(format!("state/tmux-{}", unsafe {
        // SAFETY: geteuid has no pointer arguments or side effects.
        libc::geteuid()
    }));
    fs::create_dir(&sockets).unwrap();
    fs::write(sockets.join("shadowfax-herdr-workspace"), "stale").unwrap();
    let output = fixture.run(&["reap"]);
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["errors"].as_array().unwrap().len(), 1);
    assert!(fixture.notification().contains("1 cleanup errors"));
}

#[test]
fn failed_notification_does_not_change_successful_cleanup_exit_status() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("herdr"), "#!/bin/sh\nexit 9\n").unwrap();
    let output = fixture.run(&["reap"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reap notification"));
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_ok());
}

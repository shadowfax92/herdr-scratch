//! Background reclamation for Scratch's private tmux servers. Popup commands
//! never call the scanner; lifecycle hooks own its separate worker process.
//! Explicit reaping shares the same policy, sweep lock, and identity checks.

mod backend;
mod policy;
#[cfg(test)]
mod real_tests;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{CleanupConfig, LoadedConfig};
use policy::{Owner, Reason};

const SWEEP_BUDGET: Duration = Duration::from_secs(2);
const MAX_REMOVALS: usize = 16;

/// Only the sweeper writes this cache. Atomic replacement lets preview readers
/// observe a complete generation without taking a lock on any popup operation.
#[derive(Default, Serialize, Deserialize)]
struct State {
    cursor: usize,
    records: HashMap<String, Record>,
}

#[derive(Default, Serialize, Deserialize)]
struct Record {
    terminal: Option<String>,
    last_used: u64,
}

#[derive(Default, Serialize)]
struct Report {
    enabled: bool,
    scanned: usize,
    owned_sessions: usize,
    removed: usize,
    candidates: Vec<Candidate>,
    errors: Vec<String>,
}

#[derive(Serialize)]
struct Candidate {
    session: String,
    reason: Reason,
    last_used: u64,
    removed: bool,
}

fn state_dir() -> Result<PathBuf> {
    let root = PathBuf::from(
        std::env::var_os("HERDR_PLUGIN_STATE_DIR")
            .context("HERDR_PLUGIN_STATE_DIR is required for cleanup")?,
    );
    fs::create_dir_all(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    Ok(root)
}

/// Startup hooks only launch/check the separate worker. They never enumerate
/// tmux or Herdr sessions, and no popup command calls even this launcher.
pub fn start() -> Result<()> {
    let root = state_dir()?;
    let restarting = root.join("cleanup.stop").exists();
    let owner = lock(&root.join("cleanup-worker.lock"))?;
    if owner.is_none() && !restarting {
        return Ok(());
    }
    let _ = fs::remove_file(root.join("cleanup.stop"));
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(root.join("cleanup.log"))?;
    Command::new(std::env::current_exe()?)
        .arg("cleanup-worker")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()?;
    // The worker waits for this short launcher lock to be released before
    // taking its lifetime lock. Concurrent launchers may spawn only losers.
    drop(owner);
    Ok(())
}

pub fn stop() -> Result<()> {
    fs::write(state_dir()?.join("cleanup.stop"), [])?;
    Ok(())
}

pub fn worker() -> Result<()> {
    let root = state_dir()?;
    let mut owner = None;
    // A start immediately after stop may find the old worker still unwinding.
    // The launcher returns promptly; its replacement waits off the UI path.
    for _ in 0..100 {
        owner = lock(&root.join("cleanup-worker.lock"))?;
        if owner.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let Some(_owner) = owner else { return Ok(()) };
    while !root.join("cleanup.stop").exists() {
        let config = LoadedConfig::load();
        let interval = match config {
            Ok(config) => {
                if let Some(_sweep) = lock(&root.join("cleanup-sweep.lock"))? {
                    match sweep(&root, config.cleanup(), true, true, now()?) {
                        Ok(report) => {
                            // Status is observability, not worker ownership. A
                            // failed publication must not cancel future sweeps.
                            if let Err(error) =
                                atomic_json(&root.join("cleanup-status.json"), &report)
                            {
                                eprintln!("cleanup status: {error:#}");
                            }
                        }
                        Err(error) => eprintln!("cleanup: {error:#}"),
                    }
                }
                config.cleanup().interval_seconds
            }
            Err(error) => {
                // Invalid config suspends deletion; it never falls back to a
                // potentially more aggressive default retention policy.
                eprintln!("cleanup suspended: {error:#}");
                60
            }
        };
        let next = Instant::now() + Duration::from_secs(interval);
        while Instant::now() < next && !root.join("cleanup.stop").exists() {
            thread::sleep(Duration::from_millis(200));
        }
    }
    Ok(())
}

pub fn inspect(apply: bool) -> Result<()> {
    let report = inspect_report(apply)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Herdr dispatches plugin actions asynchronously, so a menu's successful
/// invocation only means the child started. Report completion from that child,
/// after the shared sweep finishes, including disabled policy and failures.
pub fn reap() -> Result<()> {
    let result = inspect_report(true);
    let message = match &result {
        Ok(report) => report.summary(),
        Err(error) => format!("Could not reap scratch sessions: {error:#}"),
    };
    if let Err(error) = crate::herdr::Herdr::from_env().show_notification(&message) {
        // Notification failure cannot undo removals or turn a completed sweep
        // into a retryable failure; the full result remains in the plugin log.
        eprintln!("reap notification: {error:#}");
    }
    let report = result?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.errors.is_empty() {
        bail!("scratch reaping completed with errors; see the report above");
    }
    Ok(())
}

impl Report {
    fn summary(&self) -> String {
        if !self.enabled {
            return "Scratch cleanup is disabled in config.yaml; no sessions removed.".into();
        }
        let mut message = format!(
            "Reaped {} scratch sessions (checked {}/{}).",
            self.removed, self.scanned, self.owned_sessions
        );
        if self.scanned < self.owned_sessions {
            message.push_str(" Run again to continue.");
        }
        if !self.errors.is_empty() {
            message.push_str(&format!(
                " {} cleanup errors; see the Scratch plugin log.",
                self.errors.len()
            ));
        }
        message
    }
}

fn inspect_report(apply: bool) -> Result<Report> {
    let root = state_dir()?;
    let _lock = if apply {
        Some(
            lock(&root.join("cleanup-sweep.lock"))?
                .context("a cleanup sweep is already running")?,
        )
    } else {
        None
    };
    sweep(&root, LoadedConfig::load()?.cleanup(), apply, false, now()?)
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn sweep(
    root: &Path,
    policy: &CleanupConfig,
    apply: bool,
    cancellable: bool,
    now: u64,
) -> Result<Report> {
    let mut report = Report {
        enabled: policy.enabled,
        ..Report::default()
    };
    if !policy.enabled {
        return Ok(report);
    }
    let mut state: State = match fs::read(root.join("cleanup-state.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .context("invalid cleanup state; refusing to discard terminal identities")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
        Err(e) => return Err(e.into()),
    };
    let deadline = Instant::now() + SWEEP_BUDGET;
    let snapshot = backend::session_snapshot(root);
    let sessions = snapshot.sessions;
    report.errors = snapshot.errors;
    report.owned_sessions = sessions.len();
    let keys: HashSet<_> = sessions.iter().map(backend::Session::key).collect();
    state.records.retain(|key, _| {
        keys.contains(key)
            || snapshot
                .unavailable
                .iter()
                .any(|socket| key.starts_with(&format!("{}:", socket.display())))
    });
    let mut hosts = HashMap::new();
    for offset in 0..sessions.len() {
        if Instant::now() >= deadline
            || report.removed >= MAX_REMOVALS
            || (cancellable && root.join("cleanup.stop").exists())
        {
            break;
        }
        // Advance the cursor even across errors so one unavailable server or
        // large backlog cannot starve later sessions in repeated bounded sweeps.
        let index = (state.cursor + offset) % sessions.len();
        let session = &sessions[index];
        report.scanned += 1;
        let record = state.records.entry(session.key()).or_default();
        record.last_used = record.last_used.max(session.last_used());
        if session.attached {
            record.last_used = record.last_used.max(now)
        }
        let host =
            hosts
                .entry(session.herdr_socket.clone())
                .or_insert_with(|| match backend::panes(&session.herdr_socket) {
                    Ok(panes) => Some(panes),
                    Err(error) => {
                        report
                            .errors
                            .push(format!("{}: {error:#}", session.herdr_socket.display()));
                        None
                    }
                });
        let (owner, terminal) =
            backend::resolve_owner(session, record.terminal.as_deref(), host.as_ref());
        if terminal.is_some() {
            record.terminal = terminal
        }
        let Some(reason) = policy::decide(
            owner,
            session.attached,
            record.last_used,
            now,
            policy.ttl_hours * 3600,
        ) else {
            continue;
        };
        let mut removed = false;
        if apply {
            // Revalidate source closure just before mutation. A successful
            // transport read is required; a dropped/restarting host is kept.
            let revalidated = reason != Reason::SourceClosed
                || backend::panes(&session.herdr_socket)
                    .ok()
                    .is_some_and(|panes| {
                        backend::resolve_owner(session, record.terminal.as_deref(), Some(&panes)).0
                            == Owner::Closed
                    });
            if revalidated && !(cancellable && root.join("cleanup.stop").exists()) {
                match backend::remove(session, reason) {
                    Ok(did_remove) => removed = did_remove,
                    Err(error) => report.errors.push(format!("{}: {error:#}", session.name)),
                }
            }
        }
        if removed {
            // A minimal/workspace migration can leave two same-name sessions
            // sharing this publication. Never remove the surviving mode's file.
            match backend::context_in_use(root, &session.name) {
                Ok(false) => {
                    let path = crate::context::context_path(root, &session.name);
                    if let Err(error) = fs::remove_file(&path) {
                        if error.kind() != std::io::ErrorKind::NotFound {
                            report.errors.push(format!("{}: {error}", path.display()));
                        }
                    }
                }
                Ok(true) => {}
                Err(error) => report
                    .errors
                    .push(format!("{} context ownership: {error:#}", session.name)),
            }
        }
        report.removed += usize::from(removed);
        report.candidates.push(Candidate {
            session: session.name.clone(),
            reason,
            last_used: record.last_used,
            removed,
        });
    }
    state.cursor = if sessions.is_empty() {
        0
    } else {
        (state.cursor + report.scanned) % sessions.len()
    };
    if apply {
        atomic_json(&root.join("cleanup-state.json"), &state)?
    }
    Ok(report)
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn lock(path: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    // SAFETY: flock only operates on the owned file descriptor. File keeps the
    // lock alive, and Rust's close-on-exec descriptors cannot leak it to tmux.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        bail!("cleanup lock: {error}")
    }
}

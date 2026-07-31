use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::config::ResolvedScratch;

const SERVER_NAME: &str = "shadowfax-herdr-scratch";
const ENV_VERSION: &str = "1";

pub fn run(
    scratch: &ResolvedScratch,
    pane_id: &str,
    cwd: &Path,
    server_identity: &str,
    state_dir: &Path,
    prefix: &str,
    hide_keys: &[String],
) -> Result<()> {
    ensure_state_dir(state_dir)?;
    configure_server(state_dir, prefix, hide_keys)?;

    let name = session_name(&scratch.name, pane_id, server_identity);
    if session_needs_recreation(state_dir, &name, cwd)? {
        kill_session(state_dir, &name)?;
        create_session(state_dir, &name, scratch, pane_id, cwd)?;
    }

    let mut command = tmux_command(state_dir);
    command.args(["attach-session", "-t", &exact_target(&name)]);
    let _status = command
        .status()
        .context("failed to attach tmux scratch session")?;
    Ok(())
}

fn ensure_state_dir(state_dir: &Path) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to secure {}", state_dir.display()))
}

fn configure_server(state_dir: &Path, prefix: &str, hide_keys: &[String]) -> Result<()> {
    let args = server_configuration_args(prefix, hide_keys);
    checked_output(
        tmux_command(state_dir).args(args),
        "configure tmux scratch server",
    )?;
    Ok(())
}

fn session_needs_recreation(state_dir: &Path, name: &str, cwd: &Path) -> Result<bool> {
    if !session_exists(state_dir, name)? {
        return Ok(true);
    }
    let stored_cwd = session_option(state_dir, name, "@herdr_source_cwd")?;
    let version = session_option(state_dir, name, "@herdr_env_version")?;
    Ok(stored_cwd != cwd.to_string_lossy() || version != ENV_VERSION)
}

fn create_session(
    state_dir: &Path,
    name: &str,
    scratch: &ResolvedScratch,
    pane_id: &str,
    cwd: &Path,
) -> Result<()> {
    let mut command = tmux_command(state_dir);
    command.args([
        "new-session",
        "-d",
        "-s",
        name,
        "-c",
        &cwd.to_string_lossy(),
        "-e",
        "TMX_SCRATCH=1",
        "-e",
        &format!("TMX_SCRATCH_TYPE={}", scratch.tmx_type),
        "-e",
        &format!("TMX_PARENT_PANE={pane_id}"),
        "-e",
        &format!("HERDR_SCRATCH_KIND={}", scratch.name),
        "-e",
        &format!("HERDR_SCRATCH_SOURCE_PANE={pane_id}"),
    ]);
    command.args(&scratch.command);
    checked_output(&mut command, "create tmux scratch session")?;
    set_session_option(state_dir, name, "@herdr_source_cwd", &cwd.to_string_lossy())?;
    set_session_option(state_dir, name, "@herdr_source_pane", pane_id)?;
    set_session_option(state_dir, name, "@herdr_env_version", ENV_VERSION)
}

fn kill_session(state_dir: &Path, name: &str) -> Result<()> {
    if !session_exists(state_dir, name)? {
        return Ok(());
    }
    checked_output(
        tmux_command(state_dir).args(["kill-session", "-t", &exact_target(name)]),
        "replace stale tmux scratch session",
    )?;
    Ok(())
}

fn session_exists(state_dir: &Path, name: &str) -> Result<bool> {
    let output = tmux_command(state_dir)
        .args(["has-session", "-t", &exact_target(name)])
        .output()
        .context("failed to inspect tmux scratch session")?;
    Ok(output.status.success())
}

fn session_option(state_dir: &Path, name: &str, key: &str) -> Result<String> {
    let output = checked_output(
        tmux_command(state_dir).args(["show-options", "-qv", "-t", name, key]),
        "read tmux scratch session metadata",
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn set_session_option(state_dir: &Path, name: &str, key: &str, value: &str) -> Result<()> {
    checked_output(
        tmux_command(state_dir).args(["set-option", "-t", name, key, value]),
        "write tmux scratch session metadata",
    )?;
    Ok(())
}

fn checked_output(command: &mut Command, operation: &str) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("failed to {operation}"))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to {operation}\nstdout: {}\nstderr: {}",
            stdout.trim(),
            stderr.trim()
        );
    }
    Ok(output)
}

fn tmux_command(state_dir: &Path) -> Command {
    let mut command = Command::new("tmux");
    command
        .args(["-L", SERVER_NAME, "-f", "/dev/null"])
        .env("TMUX_TMPDIR", state_dir)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    command
}

fn server_configuration_args(prefix: &str, hide_keys: &[String]) -> Vec<String> {
    let mut args = Vec::new();
    for command in [
        vec!["start-server"],
        vec!["set-option", "-g", "status", "off"],
        vec!["set-option", "-g", "prefix", prefix],
        vec!["set-option", "-g", "prefix2", "None"],
        vec!["set-option", "-g", "mouse", "off"],
        vec!["set-option", "-s", "escape-time", "0"],
        vec!["set-option", "-g", "default-terminal", "tmux-256color"],
        vec!["set-option", "-g", "remain-on-exit", "off"],
        vec!["unbind-key", "-a", "-T", "prefix"],
        vec![
            "bind-key",
            "-T",
            "prefix",
            "x",
            "confirm-before",
            "-p",
            "Kill scratch session? (y/n)",
            "kill-session",
        ],
        vec!["bind-key", "-T", "prefix", prefix, "send-prefix"],
    ] {
        if !args.is_empty() {
            args.push(";".into());
        }
        args.extend(command.into_iter().map(str::to_owned));
    }
    for key in hide_keys {
        args.push(";".into());
        args.extend([
            "bind-key".into(),
            "-n".into(),
            key.clone(),
            "detach-client".into(),
        ]);
    }
    args
}

fn session_name(scratch_name: &str, pane_id: &str, server_identity: &str) -> String {
    format!(
        "hs/{}/{}/{}",
        scratch_name,
        short_hash(server_identity),
        sanitize(pane_id)
    )
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn sanitize(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "pane".into()
    } else {
        trimmed.into()
    }
}

fn exact_target(name: &str) -> String {
    format!("={name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_identity_is_per_kind_pane_and_server() {
        let nvim = session_name("nvim", "w2:p1", "/tmp/herdr.sock");
        let shell = session_name("shell", "w2:p1", "/tmp/herdr.sock");
        let other_pane = session_name("nvim", "w2:p2", "/tmp/herdr.sock");
        let other_server = session_name("nvim", "w2:p1", "/tmp/other.sock");

        assert_eq!(nvim, "hs/nvim/0cb0c5e4a746/w2-p1");
        assert_ne!(nvim, shell);
        assert_ne!(nvim, other_pane);
        assert_ne!(nvim, other_server);
    }

    #[test]
    fn nested_tmux_uses_the_same_toggle_keys() {
        let args = server_configuration_args("C-a", &["M-i".into(), "M-o".into(), "M-t".into()]);

        assert!(args
            .windows(4)
            .any(|part| part == ["bind-key", "-n", "M-i", "detach-client"]));
        assert!(args
            .windows(4)
            .any(|part| part == ["bind-key", "-n", "M-o", "detach-client"]));
        assert!(args
            .windows(4)
            .any(|part| part == ["bind-key", "-n", "M-t", "detach-client"]));
        assert!(args
            .windows(5)
            .any(|part| part == ["bind-key", "-T", "prefix", "C-a", "send-prefix"]));
    }
}

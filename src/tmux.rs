use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::config::{ResolvedScratch, TmuxMode};

const MINIMAL_SERVER_NAME: &str = "shadowfax-herdr-scratch";
const WORKSPACE_SERVER_NAME: &str = "shadowfax-herdr-workspace";
const WORKSPACE_HOOK_INDEX: u16 = 999;
const ENV_VERSION: &str = "1";

struct TmuxServer<'a> {
    state_dir: &'a Path,
    mode: TmuxMode,
}

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
    let server = TmuxServer {
        state_dir,
        mode: scratch.tmux_mode,
    };

    let name = session_name(&scratch.name, pane_id, server_identity);
    if server.session_needs_recreation(&name, cwd)? {
        server.kill_session(&name)?;
        server.create_session(&name, scratch, pane_id, cwd, prefix, hide_keys)?;
    } else {
        server.configure(prefix, hide_keys)?;
    }

    let mut command = server.command();
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

impl TmuxServer<'_> {
    fn configure(&self, prefix: &str, hide_keys: &[String]) -> Result<()> {
        let args = server_configuration_args(self.mode, prefix, hide_keys);
        checked_output(self.command().args(args), "configure tmux scratch server")?;
        Ok(())
    }

    fn session_needs_recreation(&self, name: &str, cwd: &Path) -> Result<bool> {
        if !self.session_exists(name)? {
            return Ok(true);
        }
        let stored_cwd = self.session_option(name, "@herdr_source_cwd")?;
        let version = self.session_option(name, "@herdr_env_version")?;
        Ok(session_metadata_needs_recreation(
            self.mode,
            &stored_cwd,
            cwd,
            &version,
        ))
    }

    fn create_session(
        &self,
        name: &str,
        scratch: &ResolvedScratch,
        pane_id: &str,
        cwd: &Path,
        prefix: &str,
        hide_keys: &[String],
    ) -> Result<()> {
        let mut command = self.command();
        command.args(configured_session_args(
            scratch, name, pane_id, cwd, prefix, hide_keys,
        ));
        checked_output(&mut command, "create tmux scratch session")?;
        self.set_session_option(name, "@herdr_source_cwd", &cwd.to_string_lossy())?;
        self.set_session_option(name, "@herdr_source_pane", pane_id)?;
        self.set_session_option(name, "@herdr_env_version", ENV_VERSION)
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        if !self.session_exists(name)? {
            return Ok(());
        }
        checked_output(
            self.command()
                .args(["kill-session", "-t", &exact_target(name)]),
            "replace stale tmux scratch session",
        )?;
        Ok(())
    }

    fn session_exists(&self, name: &str) -> Result<bool> {
        let output = self
            .command()
            .args(["has-session", "-t", &exact_target(name)])
            .output()
            .context("failed to inspect tmux scratch session")?;
        Ok(output.status.success())
    }

    fn session_option(&self, name: &str, key: &str) -> Result<String> {
        let output = checked_output(
            self.command()
                .args(["show-options", "-qv", "-t", name, key]),
            "read tmux scratch session metadata",
        )?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn set_session_option(&self, name: &str, key: &str, value: &str) -> Result<()> {
        checked_output(
            self.command().args(["set-option", "-t", name, key, value]),
            "write tmux scratch session metadata",
        )?;
        Ok(())
    }

    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command
            .args(tmux_args(self.mode))
            .env("TMUX_TMPDIR", self.state_dir)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
    }
}

fn session_metadata_needs_recreation(
    mode: TmuxMode,
    stored_cwd: &str,
    current_cwd: &Path,
    version: &str,
) -> bool {
    version != ENV_VERSION
        || (mode == TmuxMode::Minimal && stored_cwd != current_cwd.to_string_lossy())
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

fn tmux_args(mode: TmuxMode) -> Vec<&'static str> {
    match mode {
        TmuxMode::Minimal => vec!["-L", MINIMAL_SERVER_NAME, "-f", "/dev/null"],
        TmuxMode::Workspace => vec!["-L", WORKSPACE_SERVER_NAME],
    }
}

fn server_configuration_args(mode: TmuxMode, prefix: &str, hide_keys: &[String]) -> Vec<String> {
    match mode {
        TmuxMode::Minimal => minimal_server_configuration_args(prefix, hide_keys),
        TmuxMode::Workspace => workspace_server_configuration_args(prefix, hide_keys),
    }
}

fn minimal_server_configuration_args(prefix: &str, hide_keys: &[String]) -> Vec<String> {
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

fn workspace_server_configuration_args(prefix: &str, hide_keys: &[String]) -> Vec<String> {
    let overlay = workspace_overlay_command(prefix, hide_keys);
    let mut commands = vec![vec!["start-server".into()]];
    commands.extend(workspace_overlay_commands(prefix, hide_keys));
    for hook in ["after-set-option", "after-bind-key", "after-unbind-key"] {
        commands.push(vec![
            "set-hook".into(),
            "-g".into(),
            format!("{hook}[{WORKSPACE_HOOK_INDEX}]"),
            overlay.clone(),
        ]);
    }
    flatten_commands(commands)
}

fn configured_session_args(
    scratch: &ResolvedScratch,
    name: &str,
    pane_id: &str,
    cwd: &Path,
    prefix: &str,
    hide_keys: &[String],
) -> Vec<String> {
    let mut args = server_configuration_args(scratch.tmux_mode, prefix, hide_keys);
    args.push(";".into());
    args.extend([
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.into(),
        "-c".into(),
        cwd.to_string_lossy().into_owned(),
        "-e".into(),
        "TMX_SCRATCH=1".into(),
        "-e".into(),
        format!("TMX_SCRATCH_TYPE={}", scratch.tmx_type),
        "-e".into(),
        format!("TMX_PARENT_PANE={pane_id}"),
        "-e".into(),
        format!("HERDR_SCRATCH_KIND={}", scratch.name),
        "-e".into(),
        format!("HERDR_SCRATCH_SOURCE_PANE={pane_id}"),
    ]);
    args.extend(scratch.command.iter().cloned());
    args
}

fn workspace_overlay_command(prefix: &str, hide_keys: &[String]) -> String {
    workspace_overlay_commands(prefix, hide_keys)
        .into_iter()
        .map(|command| command.join(" "))
        .collect::<Vec<_>>()
        .join(" ; ")
}

fn workspace_overlay_commands(prefix: &str, hide_keys: &[String]) -> Vec<Vec<String>> {
    let mut commands = vec![
        vec![
            "set-option".into(),
            "-g".into(),
            "@herdr_scratch_workspace".into(),
            "1".into(),
        ],
        vec![
            "set-option".into(),
            "-g".into(),
            "prefix".into(),
            prefix.into(),
        ],
        vec![
            "set-option".into(),
            "-g".into(),
            "prefix2".into(),
            "None".into(),
        ],
        vec![
            "bind-key".into(),
            "-T".into(),
            "prefix".into(),
            prefix.into(),
            "send-prefix".into(),
        ],
    ];
    commands.extend(hide_keys.iter().map(|key| {
        vec![
            "bind-key".into(),
            "-n".into(),
            key.clone(),
            "detach-client".into(),
        ]
    }));
    commands
}

fn flatten_commands(commands: Vec<Vec<String>>) -> Vec<String> {
    let mut args = Vec::new();
    for command in commands {
        if !args.is_empty() {
            args.push(";".into());
        }
        args.extend(command);
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
    fn minimal_mode_keeps_the_stripped_down_server() {
        let args = server_configuration_args(
            crate::config::TmuxMode::Minimal,
            "C-a",
            &["M-i".into(), "M-o".into()],
        );

        assert!(args
            .windows(4)
            .any(|part| part == ["bind-key", "-n", "M-i", "detach-client"]));
        assert!(args
            .windows(4)
            .any(|part| part == ["bind-key", "-n", "M-o", "detach-client"]));
        assert!(args
            .windows(5)
            .any(|part| part == ["bind-key", "-T", "prefix", "C-a", "send-prefix"]));
        assert!(args
            .windows(4)
            .any(|part| part == ["set-option", "-g", "status", "off"]));
        assert_eq!(
            tmux_args(crate::config::TmuxMode::Minimal),
            ["-L", MINIMAL_SERVER_NAME, "-f", "/dev/null"]
        );
    }

    #[test]
    fn workspace_mode_keeps_the_full_config_and_reapplies_its_overlay() {
        let hide_keys = ["M-i".into(), "M-o".into()];
        let args = server_configuration_args(crate::config::TmuxMode::Workspace, "C-g", &hide_keys);
        let overlay = workspace_overlay_command("C-g", &hide_keys);

        assert_eq!(
            tmux_args(crate::config::TmuxMode::Workspace),
            ["-L", WORKSPACE_SERVER_NAME]
        );
        assert!(args
            .windows(4)
            .any(|part| part == ["set-option", "-g", "prefix", "C-g"]));
        assert!(args
            .windows(4)
            .any(|part| part == ["bind-key", "-n", "M-o", "detach-client"]));
        assert!(!args
            .windows(4)
            .any(|part| part == ["set-option", "-g", "status", "off"]));
        assert!(!args.iter().any(|part| part == "unbind-key"));

        for hook in [
            "after-set-option[999]",
            "after-bind-key[999]",
            "after-unbind-key[999]",
        ] {
            assert!(args
                .windows(4)
                .any(|part| part == ["set-hook", "-g", hook, &overlay]));
        }
    }

    #[test]
    fn new_session_is_created_in_the_configured_server_queue() {
        let scratch = ResolvedScratch {
            name: "shell".into(),
            command: vec!["fish".into(), "-l".into()],
            tmx_type: "sh".into(),
            clear_tmux_env: false,
            tmux_mode: TmuxMode::Workspace,
            tmux_prefix: Some("C-g".into()),
            key: Some("alt+o".into()),
        };
        let args = configured_session_args(
            &scratch,
            "hs/shell/server/pane",
            "pane",
            Path::new("/tmp/project"),
            "C-g",
            &["M-i".into(), "M-o".into()],
        );

        let start = args.iter().position(|part| part == "start-server").unwrap();
        let prefix = args
            .windows(4)
            .position(|part| part == ["set-option", "-g", "prefix", "C-g"])
            .unwrap();
        let session = args.iter().position(|part| part == "new-session").unwrap();

        assert!(start < prefix);
        assert!(prefix < session);
        assert_eq!(&args[args.len() - 2..], ["fish", "-l"]);
    }

    #[test]
    fn workspace_survives_source_cwd_changes() {
        assert!(session_metadata_needs_recreation(
            TmuxMode::Minimal,
            "/old/project",
            Path::new("/new/project"),
            ENV_VERSION,
        ));
        assert!(!session_metadata_needs_recreation(
            TmuxMode::Workspace,
            "/old/project",
            Path::new("/new/project"),
            ENV_VERSION,
        ));
        assert!(session_metadata_needs_recreation(
            TmuxMode::Workspace,
            "/old/project",
            Path::new("/old/project"),
            "outdated",
        ));
    }
}

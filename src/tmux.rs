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

pub struct HerdrEnvironment {
    socket_path: Option<String>,
    workspace_id: Option<String>,
    tab_id: Option<String>,
}

impl HerdrEnvironment {
    pub fn from_process() -> Self {
        Self {
            socket_path: nonempty_process_environment("HERDR_SOCKET_PATH"),
            workspace_id: nonempty_process_environment("HERDR_WORKSPACE_ID"),
            tab_id: nonempty_process_environment("HERDR_TAB_ID"),
        }
    }

    fn server_identity(&self) -> &str {
        self.socket_path.as_deref().unwrap_or("default")
    }
}

struct SessionSpec<'a> {
    name: &'a str,
    scratch: &'a ResolvedScratch,
    pane_id: &'a str,
    cwd: &'a Path,
    prefix: &'a str,
    hide_keys: &'a [String],
    herdr_environment: &'a HerdrEnvironment,
}

struct TmuxServer<'a> {
    state_dir: &'a Path,
    mode: TmuxMode,
}

pub fn run(
    scratch: &ResolvedScratch,
    pane_id: &str,
    cwd: &Path,
    herdr_environment: &HerdrEnvironment,
    state_dir: &Path,
    prefix: &str,
    hide_keys: &[String],
) -> Result<()> {
    ensure_state_dir(state_dir)?;
    let server = TmuxServer {
        state_dir,
        mode: scratch.tmux_mode,
    };

    let name = session_name(&scratch.name, pane_id, herdr_environment.server_identity());
    let session = SessionSpec {
        name: &name,
        scratch,
        pane_id,
        cwd,
        prefix,
        hide_keys,
        herdr_environment,
    };
    if server.session_needs_recreation(session.name, session.cwd)? {
        server.kill_session(session.name)?;
        server.create_session(&session)?;
    } else {
        server.configure(prefix, hide_keys)?;
    }
    server.update_session_metadata(&session)?;
    server.update_session_environment(&session)?;

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

    fn create_session(&self, session: &SessionSpec<'_>) -> Result<()> {
        let mut command = self.command();
        command.args(configured_session_args(session));
        checked_output(&mut command, "create tmux scratch session")?;
        Ok(())
    }

    fn update_session_metadata(&self, session: &SessionSpec<'_>) -> Result<()> {
        self.set_session_option(
            session.name,
            "@herdr_source_cwd",
            &session.cwd.to_string_lossy(),
        )?;
        self.set_session_option(session.name, "@herdr_source_pane", session.pane_id)?;
        self.set_session_option(session.name, "@herdr_env_version", ENV_VERSION)
    }

    fn update_session_environment(&self, session: &SessionSpec<'_>) -> Result<()> {
        for (key, value) in session_environment(session) {
            checked_output(
                self.command().args([
                    "set-environment",
                    "-t",
                    &exact_target(session.name),
                    &key,
                    &value,
                ]),
                "update tmux scratch session environment",
            )?;
        }
        Ok(())
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
    mode == TmuxMode::Minimal
        && (version != ENV_VERSION || stored_cwd != current_cwd.to_string_lossy())
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
    for hook in ["after-set-option", "after-bind-key", "after-unbind-key"] {
        commands.push(vec![
            "set-hook".into(),
            "-g".into(),
            format!("{hook}[{WORKSPACE_HOOK_INDEX}]"),
            overlay.clone(),
        ]);
    }
    commands.extend(workspace_overlay_commands(prefix, hide_keys));
    flatten_commands(commands)
}

fn configured_session_args(session: &SessionSpec<'_>) -> Vec<String> {
    let mut args =
        server_configuration_args(session.scratch.tmux_mode, session.prefix, session.hide_keys);
    args.push(";".into());
    args.extend([
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        session.name.into(),
        "-c".into(),
        session.cwd.to_string_lossy().into_owned(),
    ]);
    for (key, value) in session_environment(session) {
        args.extend(["-e".into(), format!("{key}={value}")]);
    }
    args.extend(session.scratch.command.iter().cloned());
    args
}

fn session_environment(session: &SessionSpec<'_>) -> Vec<(String, String)> {
    let mut environment = vec![
        ("HERDR_ENV".into(), "1".into()),
        ("HERDR_PANE_ID".into(), session.pane_id.into()),
        ("HERDR_SCRATCH_KIND".into(), session.scratch.name.clone()),
        ("HERDR_SCRATCH_NAME".into(), session.scratch.name.clone()),
        ("HERDR_SCRATCH_PREFIX".into(), session.prefix.into()),
        (
            "HERDR_SCRATCH_SOURCE_CWD".into(),
            session.cwd.to_string_lossy().into_owned(),
        ),
        ("HERDR_SCRATCH_SOURCE_PANE".into(), session.pane_id.into()),
        ("TMX_PARENT_PANE".into(), session.pane_id.into()),
        ("TMX_SCRATCH".into(), "1".into()),
        ("TMX_SCRATCH_TYPE".into(), session.scratch.tmx_type.clone()),
    ];
    for (key, value) in [
        ("HERDR_SOCKET_PATH", &session.herdr_environment.socket_path),
        (
            "HERDR_WORKSPACE_ID",
            &session.herdr_environment.workspace_id,
        ),
        ("HERDR_TAB_ID", &session.herdr_environment.tab_id),
    ] {
        if let Some(value) = value {
            environment.push((key.into(), value.clone()));
        }
    }
    environment
}

fn nonempty_process_environment(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
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
        let args = server_configuration_args(crate::config::TmuxMode::Workspace, "C-a", &hide_keys);
        let overlay = workspace_overlay_command("C-a", &hide_keys);

        assert_eq!(
            tmux_args(crate::config::TmuxMode::Workspace),
            ["-L", WORKSPACE_SERVER_NAME]
        );
        assert!(args
            .windows(4)
            .any(|part| part == ["set-option", "-g", "prefix", "C-a"]));
        assert!(args
            .windows(4)
            .any(|part| part == ["bind-key", "-n", "M-o", "detach-client"]));
        assert!(!args
            .windows(4)
            .any(|part| part == ["set-option", "-g", "status", "off"]));
        assert!(!args.iter().any(|part| part == "unbind-key"));

        let first_hook = args
            .iter()
            .position(|part| part == "after-set-option[999]")
            .unwrap();
        let prefix = args
            .windows(4)
            .position(|part| part == ["set-option", "-g", "prefix", "C-a"])
            .unwrap();
        assert!(first_hook < prefix);

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
            tmux_prefix: Some("C-a".into()),
            key: Some("alt+o".into()),
        };
        let herdr_environment = HerdrEnvironment {
            socket_path: Some("/tmp/herdr.sock".into()),
            workspace_id: Some("w2".into()),
            tab_id: Some("w2:t4".into()),
        };
        let hide_keys = ["M-i".into(), "M-o".into()];
        let session = SessionSpec {
            name: "hs/shell/server/pane",
            scratch: &scratch,
            pane_id: "pane",
            cwd: Path::new("/tmp/project"),
            prefix: "C-a",
            hide_keys: &hide_keys,
            herdr_environment: &herdr_environment,
        };
        let args = configured_session_args(&session);

        let start = args.iter().position(|part| part == "start-server").unwrap();
        let prefix = args
            .windows(4)
            .position(|part| part == ["set-option", "-g", "prefix", "C-a"])
            .unwrap();
        let session = args.iter().position(|part| part == "new-session").unwrap();

        assert!(start < prefix);
        assert!(prefix < session);
        for variable in [
            "HERDR_ENV=1",
            "HERDR_PANE_ID=pane",
            "HERDR_SCRATCH_NAME=shell",
            "HERDR_SCRATCH_PREFIX=C-a",
            "HERDR_SCRATCH_SOURCE_CWD=/tmp/project",
            "HERDR_SOCKET_PATH=/tmp/herdr.sock",
            "HERDR_WORKSPACE_ID=w2",
            "HERDR_TAB_ID=w2:t4",
        ] {
            assert!(
                args.iter().any(|part| part == variable),
                "missing {variable}"
            );
        }
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
        assert!(!session_metadata_needs_recreation(
            TmuxMode::Workspace,
            "/old/project",
            Path::new("/old/project"),
            "outdated",
        ));
        assert!(session_metadata_needs_recreation(
            TmuxMode::Minimal,
            "/old/project",
            Path::new("/old/project"),
            "outdated",
        ));
    }
}

pub mod cleanup;
mod config;
mod context;
mod herdr;
mod prefix;
mod tmux;

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use config::LoadedConfig;
use context::SourcePane;
use herdr::{Herdr, PopupRequest};

pub fn toggle(name: &str) -> Result<()> {
    let herdr = Herdr::from_env();
    let source = match SourcePane::from_env()? {
        Some(mut source) => {
            // Plugin context pins the focused pane, but older Herdr versions
            // do not include SSH metadata there. Query that pane, preserving
            // the existing context/cwd path if this optional read fails.
            source.ssh_host = herdr.pane_ssh_host(&source.pane_id).ok().flatten();
            source
        }
        None => herdr.current_pane()?,
    };
    let config = LoadedConfig::load()?;
    let scratch = config.scratch(name)?;
    let popup = config.popup(name, herdr.client_width(&source.pane_id).ok())?;
    let tmux_prefix = match scratch.tmux_prefix {
        Some(prefix) => prefix,
        None => prefix::tmux_prefix_from_user_config()?,
    };
    herdr.open_popup(PopupRequest {
        scratch_name: name.into(),
        source_pane_id: source.pane_id,
        source_cwd: source.cwd,
        ssh_host: source.ssh_host,
        tmux_prefix,
        width: popup.size.width,
        height: popup.size.height,
    })
}

pub fn run_popup() -> Result<()> {
    // The native popup owns the local PTY; replace its process with the side
    // channel before resolving paths or loading any local scratch state. A
    // remote detach/exit then closes the popup without an extra local wrapper.
    if let Some(mut command) = remote_popup_command(std::env::vars_os()) {
        return Err(command.exec()).context("failed to execute remote scratch popup");
    }
    let scratch_name =
        std::env::var("HERDR_SCRATCH_NAME").context("HERDR_SCRATCH_NAME is missing")?;
    let source_pane_id = std::env::var("HERDR_SCRATCH_SOURCE_PANE")
        .context("HERDR_SCRATCH_SOURCE_PANE is missing")?;
    let context = Herdr::from_env().scratch_context(&source_pane_id)?;
    let state_dir = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .context("HERDR_PLUGIN_STATE_DIR is missing")?;
    let herdr_environment = tmux::HerdrEnvironment::from_process();
    let tmux_prefix = std::env::var("HERDR_SCRATCH_PREFIX").unwrap_or_else(|_| "C-b".into());
    let config = LoadedConfig::load()?;
    let scratch = config.scratch(&scratch_name)?;
    let hide_keys = config
        .keys()
        .map(prefix::normalize_tmux_key)
        .collect::<Result<Vec<_>>>()?;

    if std::env::var("HERDR_SSH_PANES").as_deref() == Ok("1") {
        // Remote plugin startup hooks do not run through `ssh run`. The
        // independent worker uses the forwarded API; link loss is not closure.
        cleanup::start()?;
    }

    tmux::run(
        &scratch,
        &context,
        &herdr_environment,
        &state_dir,
        &tmux_prefix,
        &hide_keys,
    )
}

fn remote_popup_command(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Option<Command> {
    let mut environment: Vec<_> = environment.into_iter().collect();
    if environment.iter().any(|(key, _)| key == "HERDR_SSH_PANES") {
        return None;
    }
    let host = &environment
        .iter()
        .find(|(key, _)| key == "HERDR_SCRATCH_SSH_HOST")?
        .1;
    let bin = environment
        .iter()
        .find(|(key, _)| key == "HERDR_BIN_PATH")
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| "herdr".into());
    let mut command = Command::new(bin);
    command.args([OsString::from("ssh"), OsString::from("run"), host.clone()]);
    environment.sort();
    for (key, value) in environment {
        if key.as_encoded_bytes().starts_with(b"HERDR_SCRATCH_") {
            let mut assignment = key;
            assignment.push("=");
            assignment.push(value);
            command.arg("--env").arg(assignment);
        }
    }
    // These tokens belong to `herdr ssh run`, which expands them remotely.
    // Never forward the popup's local plugin directories onto the remote host.
    command.args([
        "--env",
        "HERDR_PLUGIN_STATE_DIR=@state/shadowfax.scratch",
        "--env",
        "HERDR_PLUGIN_CONFIG_DIR=@root/plugin-config/shadowfax.scratch",
        "--",
        "herdr-scratch",
        "run-popup",
    ]);
    Some(command)
}

pub fn show_config() -> Result<()> {
    let config = LoadedConfig::load()?;
    let herdr = Herdr::from_env();
    let client_width = herdr
        .current_pane()
        .and_then(|pane| herdr.client_width(&pane.pane_id))
        .ok();

    println!("config: {}", config.path.display());
    println!(
        "cleanup: {} ({} hours since last use, {} second interval)",
        if config.cleanup().enabled {
            "enabled"
        } else {
            "disabled"
        },
        config.cleanup().ttl_hours,
        config.cleanup().interval_seconds
    );
    println!(
        "client_width: {}",
        client_width
            .map(|width| width.to_string())
            .unwrap_or_else(|| "unavailable".into())
    );
    for name in config.names() {
        let scratch = config.scratch(name)?;
        let popup = config.popup(name, client_width)?;
        let profile = popup.profile.as_deref().unwrap_or("default");
        let key = scratch.key.as_deref().unwrap_or("unbound");
        println!(
            "{name}: {} x {} ({profile}, {key})",
            popup.size.width, popup.size.height
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use toml_edit::DocumentMut;

    #[test]
    fn remote_popup_exec_forwards_scratch_values_as_individual_arguments() {
        let command = remote_popup_command(
            [
                ("HERDR_SCRATCH_SSH_HOST", "studio"),
                ("HERDR_SCRATCH_NAME", "nvim"),
                ("HERDR_SCRATCH_SOURCE_PANE", "w2:p3"),
                ("HERDR_SCRATCH_SOURCE_CWD", "/remote/project with spaces"),
                ("HERDR_SCRATCH_PREFIX", "C-a"),
                ("HERDR_SCRATCH_FUTURE", "a=b 'quoted' $value"),
                ("HERDR_BIN_PATH", "/local payload/herdr"),
                ("HERDR_PLUGIN_STATE_DIR", "/local/state"),
                ("HERDR_PLUGIN_CONFIG_DIR", "/local/config"),
                ("UNRELATED", "not forwarded"),
            ]
            .map(|(key, value)| (OsString::from(key), OsString::from(value))),
        )
        .unwrap();
        assert_eq!(command.get_program(), "/local payload/herdr");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "ssh",
                "run",
                "studio",
                "--env",
                "HERDR_SCRATCH_FUTURE=a=b 'quoted' $value",
                "--env",
                "HERDR_SCRATCH_NAME=nvim",
                "--env",
                "HERDR_SCRATCH_PREFIX=C-a",
                "--env",
                "HERDR_SCRATCH_SOURCE_CWD=/remote/project with spaces",
                "--env",
                "HERDR_SCRATCH_SOURCE_PANE=w2:p3",
                "--env",
                "HERDR_SCRATCH_SSH_HOST=studio",
                "--env",
                "HERDR_PLUGIN_STATE_DIR=@state/shadowfax.scratch",
                "--env",
                "HERDR_PLUGIN_CONFIG_DIR=@root/plugin-config/shadowfax.scratch",
                "--",
                "herdr-scratch",
                "run-popup",
            ]
        );
        assert!(remote_popup_command([]).is_none());
        for marker in ["1", "0", ""] {
            assert!(remote_popup_command([
                ("HERDR_SCRATCH_SSH_HOST".into(), "studio".into()),
                ("HERDR_SSH_PANES".into(), marker.into()),
            ])
            .is_none());
        }
    }

    #[test]
    fn manifest_exposes_toggles_and_owned_reaping() {
        let manifest = include_str!("../herdr-plugin.toml")
            .parse::<DocumentMut>()
            .unwrap();
        let actions = manifest["actions"].as_array_of_tables().unwrap();
        let ids = actions
            .iter()
            .map(|action| action["id"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(ids, ["toggle-nvim", "toggle-shell", "reap"]);
        let reap = actions.get(2).unwrap();
        let command = reap["command"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(command, ["./target/release/herdr-scratch", "reap"]);
    }
}

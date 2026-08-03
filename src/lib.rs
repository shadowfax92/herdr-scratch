mod config;
mod context;
mod herdr;
mod prefix;
mod tmux;

use std::path::PathBuf;

use anyhow::{Context, Result};

use config::LoadedConfig;
use context::SourcePane;
use herdr::{Herdr, PopupRequest};

pub fn toggle(name: &str) -> Result<()> {
    let herdr = Herdr::from_env();
    let source = match SourcePane::from_env()? {
        Some(source) => source,
        None => herdr.current_pane()?,
    };
    let config = LoadedConfig::load()?;
    config.scratch(name)?;
    let popup = config.popup(name, herdr.client_width(&source.pane_id).ok())?;
    let tmux_prefix = prefix::tmux_prefix_from_user_config()?;
    herdr.open_popup(PopupRequest {
        scratch_name: name.into(),
        source_pane_id: source.pane_id,
        source_cwd: source.cwd,
        tmux_prefix,
        width: popup.size.width,
        height: popup.size.height,
    })
}

pub fn run_popup() -> Result<()> {
    let scratch_name =
        std::env::var("HERDR_SCRATCH_NAME").context("HERDR_SCRATCH_NAME is missing")?;
    let source_pane_id = std::env::var("HERDR_SCRATCH_SOURCE_PANE")
        .context("HERDR_SCRATCH_SOURCE_PANE is missing")?;
    let source_cwd = std::env::var_os("HERDR_SCRATCH_SOURCE_CWD")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let state_dir = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .context("HERDR_PLUGIN_STATE_DIR is missing")?;
    let herdr_socket_path = std::env::var("HERDR_SOCKET_PATH")
        .ok()
        .filter(|path| !path.is_empty());
    let tmux_prefix = std::env::var("HERDR_SCRATCH_PREFIX").unwrap_or_else(|_| "C-b".into());
    let config = LoadedConfig::load()?;
    let scratch = config.scratch(&scratch_name)?;
    let hide_keys = config
        .keys()
        .map(prefix::normalize_tmux_key)
        .collect::<Result<Vec<_>>>()?;

    tmux::run(
        &scratch,
        &source_pane_id,
        &source_cwd,
        herdr_socket_path.as_deref(),
        &state_dir,
        &tmux_prefix,
        &hide_keys,
    )
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

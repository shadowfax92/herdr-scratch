mod context;
mod herdr;
mod prefix;
mod tmux;

use std::path::PathBuf;

use anyhow::{Context, Result};

use context::SourcePane;
use herdr::{Herdr, PopupRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchKind {
    Nvim,
    Shell,
}

impl ScratchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nvim => "nvim",
            Self::Shell => "shell",
        }
    }

    fn entrypoint(self) -> &'static str {
        self.as_str()
    }

    fn tmx_type(self) -> &'static str {
        match self {
            Self::Nvim => "vim",
            Self::Shell => "sh",
        }
    }
}

pub fn toggle(kind: ScratchKind) -> Result<()> {
    let source = SourcePane::from_env()?;
    let tmux_prefix = prefix::tmux_prefix_from_user_config()?;
    Herdr::from_env().open_popup(PopupRequest {
        entrypoint: kind.entrypoint(),
        source_pane_id: source.pane_id,
        source_cwd: source.cwd,
        tmux_prefix,
    })
}

pub fn run_popup(kind: ScratchKind) -> Result<()> {
    let source_pane_id = std::env::var("HERDR_SCRATCH_SOURCE_PANE")
        .context("HERDR_SCRATCH_SOURCE_PANE is missing")?;
    let source_cwd = std::env::var_os("HERDR_SCRATCH_SOURCE_CWD")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let state_dir = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .context("HERDR_PLUGIN_STATE_DIR is missing")?;
    let server_identity = std::env::var("HERDR_SOCKET_PATH").unwrap_or_else(|_| "default".into());
    let tmux_prefix = std::env::var("HERDR_SCRATCH_PREFIX").unwrap_or_else(|_| "C-b".into());

    tmux::run(
        kind,
        &source_pane_id,
        &source_cwd,
        &server_identity,
        &state_dir,
        &tmux_prefix,
    )
}

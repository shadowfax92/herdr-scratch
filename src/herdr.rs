use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

const PLUGIN_ID: &str = "shadowfax.scratch";
const POPUP_WIDTH: &str = "90%";
const POPUP_HEIGHT: &str = "85%";

#[derive(Debug, PartialEq, Eq)]
pub struct PopupRequest {
    pub entrypoint: &'static str,
    pub source_pane_id: String,
    pub source_cwd: PathBuf,
    pub tmux_prefix: String,
}

pub struct Herdr {
    bin: OsString,
}

impl Herdr {
    pub fn from_env() -> Self {
        Self {
            bin: std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| OsString::from("herdr")),
        }
    }

    pub fn open_popup(&self, request: PopupRequest) -> Result<()> {
        let args = open_popup_args(&request);
        let output = Command::new(&self.bin)
            .args(&args)
            .output()
            .with_context(|| format!("failed to run Herdr command: {}", args.join(" ")))?;

        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "Herdr popup failed: {}\nstdout: {}\nstderr: {}",
                args.join(" "),
                stdout.trim(),
                stderr.trim()
            );
        }
        Ok(())
    }
}

fn open_popup_args(request: &PopupRequest) -> Vec<String> {
    vec![
        "plugin".into(),
        "pane".into(),
        "open".into(),
        "--plugin".into(),
        PLUGIN_ID.into(),
        "--entrypoint".into(),
        request.entrypoint.into(),
        "--placement".into(),
        "popup".into(),
        "--width".into(),
        POPUP_WIDTH.into(),
        "--height".into(),
        POPUP_HEIGHT.into(),
        "--env".into(),
        format!("HERDR_SCRATCH_SOURCE_PANE={}", request.source_pane_id),
        "--env".into(),
        format!("HERDR_SCRATCH_SOURCE_CWD={}", request.source_cwd.display()),
        "--env".into(),
        format!("HERDR_SCRATCH_PREFIX={}", request.tmux_prefix),
        "--focus".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_native_popup_with_source_identity() {
        let args = open_popup_args(&PopupRequest {
            entrypoint: "nvim",
            source_pane_id: "w2:p1".into(),
            source_cwd: PathBuf::from("/tmp/project"),
            tmux_prefix: "C-a".into(),
        });

        assert!(args.windows(2).any(|pair| pair == ["--placement", "popup"]));
        assert!(args.windows(2).any(|pair| pair == ["--width", "90%"]));
        assert!(!args.iter().any(|arg| arg == "--cwd"));
        assert!(args
            .iter()
            .any(|arg| arg == "HERDR_SCRATCH_SOURCE_PANE=w2:p1"));
        assert!(args
            .iter()
            .any(|arg| arg == "HERDR_SCRATCH_SOURCE_CWD=/tmp/project"));
        assert!(args.iter().any(|arg| arg == "HERDR_SCRATCH_PREFIX=C-a"));
    }
}

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::context::{ScratchContext, SourcePane};

const PLUGIN_ID: &str = "shadowfax.scratch";
const POPUP_ENTRYPOINT: &str = "scratch";

#[derive(Debug, PartialEq, Eq)]
pub struct PopupRequest {
    pub scratch_name: String,
    pub source_pane_id: String,
    pub source_cwd: PathBuf,
    pub ssh_host: Option<String>,
    pub tmux_prefix: String,
    pub width: String,
    pub height: String,
}

#[derive(Debug, Deserialize)]
struct CurrentPaneResponse {
    result: CurrentPaneResult,
}

#[derive(Debug, Deserialize)]
struct CurrentPaneResult {
    pane: PaneInfo,
}

#[derive(Debug, Deserialize)]
struct PaneInfo {
    pane_id: String,
    cwd: Option<String>,
    foreground_cwd: Option<String>,
    #[serde(default)]
    tokens: HashMap<String, String>,
    ssh: Option<SshPane>,
}

#[derive(Debug, Deserialize)]
struct SshPane {
    host: String,
    state: String,
}

impl PaneInfo {
    fn ssh_host(&self) -> Option<&str> {
        self.ssh
            .as_ref()
            .filter(|ssh| matches!(ssh.state.as_str(), "live" | "stale") && !ssh.host.is_empty())
            .map(|ssh| ssh.host.as_str())
    }
}

#[derive(Debug, Deserialize)]
struct LayoutResponse {
    result: LayoutResult,
}

#[derive(Debug, Deserialize)]
struct LayoutResult {
    layout: Layout,
}

#[derive(Debug, Deserialize)]
struct Layout {
    area: LayoutArea,
}

#[derive(Debug, Deserialize)]
struct LayoutArea {
    x: u16,
    width: u16,
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
        self.run(args).map(|_| ())
    }

    pub fn show_notification(&self, body: &str) -> Result<()> {
        self.run(vec![
            "notification".into(),
            "show".into(),
            "Herdr Scratch".into(),
            "--body".into(),
            body.chars().take(220).collect(),
        ])
        .map(|_| ())
    }

    pub fn current_pane(&self) -> Result<SourcePane> {
        let output = self.run(vec!["pane".into(), "current".into()])?;
        let response: CurrentPaneResponse =
            serde_json::from_str(&output).context("failed to parse `herdr pane current`")?;
        let pane = response.result.pane;
        let ssh_host = pane.ssh_host().map(str::to_owned);
        let cwd = pane
            .foreground_cwd
            .or(pane.cwd)
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?);
        Ok(SourcePane {
            pane_id: pane.pane_id,
            cwd,
            ssh_host,
        })
    }

    pub fn pane_ssh_host(&self, pane_id: &str) -> Result<Option<String>> {
        let output = self.run(vec!["pane".into(), "get".into(), pane_id.into()])?;
        let response: CurrentPaneResponse =
            serde_json::from_str(&output).context("failed to parse `herdr pane get`")?;
        Ok(response.result.pane.ssh_host().map(str::to_owned))
    }

    pub fn scratch_context(&self, pane_id: &str) -> Result<ScratchContext> {
        // Resolve at run-popup, not toggle: agent metadata can change while
        // Herdr is creating the popup. One read keeps cwd and token consistent.
        let output = self.run(vec!["pane".into(), "get".into(), pane_id.into()])?;
        let response: CurrentPaneResponse =
            serde_json::from_str(&output).context("failed to parse `herdr pane get`")?;
        let mut pane = response.result.pane;
        let cwd = pane
            .foreground_cwd
            .or(pane.cwd)
            .filter(|cwd| !cwd.is_empty())
            .map(PathBuf::from)
            .context("source pane has no cwd")?;
        // Remote run-popup reaches this only after SSH dispatch, so Grove
        // handles and cwd are resolved against the remote filesystem there.
        Ok(ScratchContext::resolve(
            pane_id.to_owned(),
            cwd,
            pane.tokens.remove("grove_worktree"),
        ))
    }

    pub fn client_width(&self, pane_id: &str) -> Result<u16> {
        let output = self.run(vec![
            "pane".into(),
            "layout".into(),
            "--pane".into(),
            pane_id.into(),
        ])?;
        let response: LayoutResponse =
            serde_json::from_str(&output).context("failed to parse `herdr pane layout`")?;
        Ok(response
            .result
            .layout
            .area
            .x
            .saturating_add(response.result.layout.area.width))
    }

    fn run(&self, args: Vec<String>) -> Result<String> {
        let output = Command::new(&self.bin)
            .args(&args)
            .output()
            .with_context(|| format!("failed to run Herdr command: {}", args.join(" ")))?;

        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "Herdr command failed: {}\nstdout: {}\nstderr: {}",
                args.join(" "),
                stdout.trim(),
                stderr.trim()
            );
        }
        String::from_utf8(output.stdout).context("Herdr returned non-UTF-8 output")
    }
}

fn open_popup_args(request: &PopupRequest) -> Vec<String> {
    let mut args = vec![
        "plugin".into(),
        "pane".into(),
        "open".into(),
        "--plugin".into(),
        PLUGIN_ID.into(),
        "--entrypoint".into(),
        POPUP_ENTRYPOINT.into(),
        "--placement".into(),
        "popup".into(),
        "--width".into(),
        request.width.clone(),
        "--height".into(),
        request.height.clone(),
        "--env".into(),
        format!("HERDR_SCRATCH_NAME={}", request.scratch_name),
        "--env".into(),
        format!("HERDR_SCRATCH_SOURCE_PANE={}", request.source_pane_id),
        "--env".into(),
        format!("HERDR_SCRATCH_SOURCE_CWD={}", request.source_cwd.display()),
        "--env".into(),
        format!("HERDR_SCRATCH_PREFIX={}", request.tmux_prefix),
        "--focus".into(),
    ];
    if let Some(host) = &request.ssh_host {
        args.extend(["--env".into(), format!("HERDR_SCRATCH_SSH_HOST={host}")]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_native_popup_with_source_identity() {
        let args = open_popup_args(&PopupRequest {
            scratch_name: "nvim".into(),
            source_pane_id: "w2:p1".into(),
            source_cwd: PathBuf::from("/tmp/project"),
            ssh_host: None,
            tmux_prefix: "C-a".into(),
            width: "70%".into(),
            height: "95%".into(),
        });

        assert!(args.windows(2).any(|pair| pair == ["--placement", "popup"]));
        assert!(args.windows(2).any(|pair| pair == ["--width", "70%"]));
        assert!(args.windows(2).any(|pair| pair == ["--height", "95%"]));
        assert!(!args.iter().any(|arg| arg == "--cwd"));
        assert!(args
            .iter()
            .any(|arg| arg == "HERDR_SCRATCH_SOURCE_PANE=w2:p1"));
        assert!(args
            .iter()
            .any(|arg| arg == "HERDR_SCRATCH_SOURCE_CWD=/tmp/project"));
        assert!(args.iter().any(|arg| arg == "HERDR_SCRATCH_PREFIX=C-a"));
        assert!(args.iter().any(|arg| arg == "HERDR_SCRATCH_NAME=nvim"));
    }

    #[test]
    fn parses_client_width_from_layout_right_edge() {
        let response: LayoutResponse =
            serde_json::from_str(r#"{"result":{"layout":{"area":{"x":30,"width":482}}}}"#).unwrap();

        assert_eq!(
            response.result.layout.area.x + response.result.layout.area.width,
            512
        );
    }

    #[test]
    fn parses_optional_ssh_routing_only_for_live_or_stale_links() {
        for (metadata, expected) in [
            (serde_json::Value::Null, None),
            (
                serde_json::json!({"host":"studio", "link_id":"l1", "state":"live", "local_cwd":"/local"}),
                Some("studio"),
            ),
            (
                serde_json::json!({"host":"studio", "state":"stale"}),
                Some("studio"),
            ),
            (
                serde_json::json!({"host":"studio", "state":"unknown"}),
                None,
            ),
        ] {
            let mut value = serde_json::json!({"result":{"pane":{"pane_id":"w1:p2", "cwd":"/remote/project", "foreground_cwd":"/remote/foreground"}}});
            if !metadata.is_null() {
                value["result"]["pane"]["ssh"] = metadata;
            }
            let response: CurrentPaneResponse = serde_json::from_value(value).unwrap();
            assert_eq!(response.result.pane.ssh_host(), expected);
        }
    }
}

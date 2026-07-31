use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PluginContext {
    focused_pane_id: Option<String>,
    focused_pane_cwd: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SourcePane {
    pub pane_id: String,
    pub cwd: PathBuf,
}

impl SourcePane {
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
            .context("HERDR_PLUGIN_CONTEXT_JSON is missing")?;
        Self::from_json(&raw)
    }

    fn from_json(raw: &str) -> Result<Self> {
        let context: PluginContext =
            serde_json::from_str(raw).context("invalid HERDR_PLUGIN_CONTEXT_JSON")?;
        let pane_id = context
            .focused_pane_id
            .filter(|value| !value.trim().is_empty())
            .context("plugin action has no focused pane")?;
        let cwd = context
            .focused_pane_cwd
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?);
        Ok(Self { pane_id, cwd })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_focused_pane_and_cwd() {
        let source = SourcePane::from_json(
            r#"{"focused_pane_id":"w2:p3","focused_pane_cwd":"/tmp/project"}"#,
        )
        .unwrap();

        assert_eq!(source.pane_id, "w2:p3");
        assert_eq!(source.cwd, PathBuf::from("/tmp/project"));
    }

    #[test]
    fn rejects_context_without_a_pane() {
        let error = SourcePane::from_json(r#"{"focused_pane_cwd":"/tmp/project"}"#).unwrap_err();

        assert!(error.to_string().contains("no focused pane"));
    }
}

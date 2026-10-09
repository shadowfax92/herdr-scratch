//! Herdr-facing root resolution and the versioned handoff to scratch editors.
//! tmux receives this resolved context; Neovim reads its atomic publication.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

static NEXT_PUBLICATION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RootSource {
    Grove,
    Cwd,
}

/// One source-pane observation per popup open. A removed worktree or stale
/// token falls back to the pane cwd without changing any running shell.
#[derive(Debug, PartialEq, Eq)]
pub struct ScratchContext {
    pub source_pane: String,
    pub root: PathBuf,
    pub root_source: RootSource,
}

#[derive(Serialize)]
struct PublishedContext<'a> {
    version: u8,
    source_pane: &'a str,
    root: &'a Path,
    root_source: &'a RootSource,
    shown_at_ms: u128,
}

impl ScratchContext {
    pub fn resolve(source_pane: String, cwd: PathBuf, grove_worktree: Option<String>) -> Self {
        let worktree = grove_worktree.as_deref().and_then(grove_worktree_root);
        let (root, root_source) = match worktree {
            Some(path) => (path, RootSource::Grove),
            None => (cwd, RootSource::Cwd),
        };
        Self {
            source_pane,
            root,
            root_source,
        }
    }

    pub fn publish(&self, path: &Path) -> Result<()> {
        let directory = path.parent().context("scratch context has no directory")?;
        fs::create_dir_all(directory)?;
        // A same-directory rename gives readers a complete old or new JSON
        // document. Unique, private temp files also isolate concurrent opens.
        let temporary = path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            NEXT_PUBLICATION.fetch_add(1, Ordering::Relaxed)
        ));
        let publication = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            serde_json::to_writer(
                &mut file,
                &PublishedContext {
                    version: 1,
                    source_pane: &self.source_pane,
                    root: &self.root,
                    root_source: &self.root_source,
                    shown_at_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
                },
            )?;
            file.write_all(b"\n")?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if publication.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        publication.with_context(|| format!("failed to publish {}", path.display()))
    }
}

/// Grove owns the immutable handle files; Scratch only reads them. A 64-character
/// hex token fits Herdr's limit while the file carries an arbitrary-length
/// root. Failed lookups stay optional so pane cwd remains a usable fallback.
fn grove_worktree_root(token: &str) -> Option<PathBuf> {
    let root = if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let state_dir = std::env::var_os("XDG_STATE_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".local/state"))
            })?;
        let contents = fs::read_to_string(state_dir.join("grove/worktrees").join(token)).ok()?;
        // Remove only the producer's optional trailing LF. Trimming whitespace
        // would silently alter valid directory names; relative content is invalid.
        PathBuf::from(contents.strip_suffix('\n').unwrap_or(&contents))
    } else if token.starts_with('/') {
        PathBuf::from(token)
    } else {
        return None;
    };
    (root.is_absolute() && root.is_dir()).then_some(root)
}

/// Creation and reaping must agree on the filename, including slash-heavy
/// tmux session names. Never let a session name become a filesystem path.
pub(crate) fn context_path(state_dir: &Path, session_name: &str) -> PathBuf {
    state_dir
        .join("context")
        .join(format!("{}.json", sanitize(session_name)))
}

pub(crate) fn sanitize(value: &str) -> String {
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
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var("HERDR_PLUGIN_CONTEXT_JSON") {
            Ok(raw) => Self::from_json(&raw).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(error).context("failed to read HERDR_PLUGIN_CONTEXT_JSON"),
        }
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

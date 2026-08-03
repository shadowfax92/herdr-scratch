use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

const DEFAULT_CONFIG: &str = include_str!("../config.default.yaml");

#[derive(Debug)]
pub struct LoadedConfig {
    pub path: PathBuf,
    config: Config,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    default_popup: PopupSize,
    scratches: BTreeMap<String, ScratchDefinition>,
    #[serde(default)]
    profiles: Vec<Profile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScratchDefinition {
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    shell: bool,
    #[serde(default)]
    clear_tmux_env: bool,
    #[serde(default)]
    tmux_mode: TmuxMode,
    tmux_prefix: Option<String>,
    tmx_type: Option<String>,
    key: Option<String>,
    popup: Option<PopupSize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TmuxMode {
    #[default]
    Minimal,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PopupSize {
    pub width: String,
    pub height: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    name: String,
    #[serde(rename = "match")]
    selector: ProfileMatch,
    #[serde(default)]
    popups: BTreeMap<String, PopupSize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileMatch {
    min_client_width: Option<u16>,
    max_client_width: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScratch {
    pub name: String,
    pub command: Vec<String>,
    pub tmx_type: String,
    pub clear_tmux_env: bool,
    pub tmux_mode: TmuxMode,
    pub tmux_prefix: Option<String>,
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPopup {
    pub size: PopupSize,
    pub profile: Option<String>,
}

impl LoadedConfig {
    pub fn load() -> Result<Self> {
        let path = config_path();
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create {}", parent.display()))?;
                }
                fs::write(&path, DEFAULT_CONFIG)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                DEFAULT_CONFIG.to_owned()
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()))
            }
        };
        let config = Config::parse(&source)
            .with_context(|| format!("invalid scratch config {}", path.display()))?;
        Ok(Self { path, config })
    }

    pub fn scratch(&self, name: &str) -> Result<ResolvedScratch> {
        self.config.scratch(name)
    }

    pub fn popup(&self, name: &str, client_width: Option<u16>) -> Result<ResolvedPopup> {
        self.config.popup(name, client_width)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.config.scratches.keys().map(String::as_str)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.config
            .scratches
            .values()
            .filter_map(|scratch| scratch.key.as_deref())
    }
}

impl Config {
    fn parse(source: &str) -> Result<Self> {
        let config: Self = noyalib::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        validate_popup(&self.default_popup)?;
        if self.scratches.is_empty() {
            bail!("at least one scratch definition is required");
        }
        for (name, scratch) in &self.scratches {
            if !valid_name(name) {
                bail!("scratch name `{name}` may contain only letters, numbers, `_`, and `-`");
            }
            match (scratch.shell, scratch.command.is_empty()) {
                (true, false) => bail!("scratch `{name}` cannot set both `shell` and `command`"),
                (false, true) => bail!("scratch `{name}` must set `shell: true` or `command`"),
                _ => {}
            }
            if scratch.command.iter().any(|part| part.is_empty()) {
                bail!("scratch `{name}` contains an empty command argument");
            }
            if scratch.tmux_mode == TmuxMode::Workspace && scratch.tmux_prefix.is_none() {
                bail!("scratch `{name}` workspace mode requires `tmux_prefix`");
            }
            if let Some(prefix) = &scratch.tmux_prefix {
                crate::prefix::normalize_tmux_key(prefix)
                    .with_context(|| format!("scratch `{name}` has invalid `tmux_prefix`"))?;
            }
            if let Some(popup) = &scratch.popup {
                validate_popup(popup)?;
            }
        }
        for profile in &self.profiles {
            if profile.name.trim().is_empty() {
                bail!("profile name cannot be empty");
            }
            if profile
                .selector
                .min_client_width
                .zip(profile.selector.max_client_width)
                .is_some_and(|(min, max)| min > max)
            {
                bail!("profile `{}` has min width above max width", profile.name);
            }
            for (name, popup) in &profile.popups {
                if !self.scratches.contains_key(name) {
                    bail!(
                        "profile `{}` references unknown scratch `{name}`",
                        profile.name
                    );
                }
                validate_popup(popup)?;
            }
        }
        Ok(())
    }

    fn scratch(&self, name: &str) -> Result<ResolvedScratch> {
        let scratch = self
            .scratches
            .get(name)
            .with_context(|| format!("unknown scratch `{name}`"))?;
        let mut command = if scratch.shell {
            vec![
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
                "-l".into(),
            ]
        } else {
            scratch.command.clone()
        };
        if scratch.clear_tmux_env {
            let mut wrapper = vec![
                "env".into(),
                "-u".into(),
                "TMUX".into(),
                "-u".into(),
                "TMUX_PANE".into(),
                "-u".into(),
                "TMUX_TMPDIR".into(),
            ];
            wrapper.append(&mut command);
            command = wrapper;
        }
        Ok(ResolvedScratch {
            name: name.into(),
            command,
            tmx_type: scratch.tmx_type.clone().unwrap_or_else(|| name.into()),
            clear_tmux_env: scratch.clear_tmux_env,
            tmux_mode: scratch.tmux_mode,
            tmux_prefix: scratch
                .tmux_prefix
                .as_deref()
                .map(crate::prefix::normalize_tmux_key)
                .transpose()?,
            key: scratch.key.clone(),
        })
    }

    fn popup(&self, name: &str, client_width: Option<u16>) -> Result<ResolvedPopup> {
        let scratch = self
            .scratches
            .get(name)
            .with_context(|| format!("unknown scratch `{name}`"))?;
        let base = scratch
            .popup
            .clone()
            .unwrap_or_else(|| self.default_popup.clone());
        let Some(width) = client_width else {
            return Ok(ResolvedPopup {
                size: base,
                profile: None,
            });
        };
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.selector.matches(width));
        Ok(match profile {
            Some(profile) => ResolvedPopup {
                size: profile.popups.get(name).cloned().unwrap_or(base),
                profile: Some(profile.name.clone()),
            },
            None => ResolvedPopup {
                size: base,
                profile: None,
            },
        })
    }
}

impl ProfileMatch {
    fn matches(&self, width: u16) -> bool {
        self.min_client_width.is_none_or(|min| width >= min)
            && self.max_client_width.is_none_or(|max| width <= max)
    }
}

fn validate_popup(popup: &PopupSize) -> Result<()> {
    for (name, value) in [("width", &popup.width), ("height", &popup.height)] {
        let valid = if let Some(percent) = value.strip_suffix('%') {
            percent
                .parse::<u16>()
                .is_ok_and(|number| (1..=100).contains(&number))
        } else {
            value.parse::<u16>().is_ok_and(|number| number > 0)
        };
        if !valid {
            bail!("popup {name} `{value}` must be positive cells or 1%-100%");
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR") {
        return PathBuf::from(dir).join("config.yaml");
    }
    if let Some(path) = std::env::var_os("HERDR_CONFIG_PATH") {
        let root = PathBuf::from(path);
        return root
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("plugins/config/shadowfax.scratch/config.yaml");
    }
    if let Some(root) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(root).join("herdr/plugins/config/shadowfax.scratch/config.yaml");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config/herdr/plugins/config/shadowfax.scratch/config.yaml");
    }
    std::env::temp_dir().join("herdr/plugins/config/shadowfax.scratch/config.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::parse(DEFAULT_CONFIG).unwrap()
    }

    #[test]
    fn selects_profiles_in_order() {
        let config = config();

        assert_eq!(config.popup("nvim", Some(300)).unwrap().size.width, "95%");
        assert_eq!(config.popup("nvim", Some(330)).unwrap().size.width, "90%");
        assert_eq!(config.popup("nvim", Some(380)).unwrap().size.width, "90%");
        let full = config.popup("nvim", Some(512)).unwrap();
        assert_eq!(
            full.size,
            PopupSize {
                width: "70%".into(),
                height: "99%".into()
            }
        );
        assert_eq!(full.profile.as_deref(), Some("full-ultrawide"));
    }

    #[test]
    fn resolves_commands_and_tmux_shell() {
        let config = config();

        assert_eq!(config.scratch("nvim").unwrap().command, ["nvim"]);
        let tmux = config.scratch("tmux").unwrap();
        assert_eq!(
            &tmux.command[..7],
            ["env", "-u", "TMUX", "-u", "TMUX_PANE", "-u", "TMUX_TMPDIR"]
        );
        assert_eq!(tmux.command.last().map(String::as_str), Some("-l"));
        assert!(tmux.clear_tmux_env);
    }

    #[test]
    fn resolves_workspace_mode_and_prefix() {
        let config = config();

        let nvim = config.scratch("nvim").unwrap();
        assert_eq!(nvim.tmux_mode, TmuxMode::Minimal);
        assert_eq!(nvim.tmux_prefix, None);

        let shell = config.scratch("shell").unwrap();
        assert_eq!(shell.tmux_mode, TmuxMode::Workspace);
        assert_eq!(shell.tmux_prefix.as_deref(), Some("C-g"));
    }

    #[test]
    fn rejects_invalid_explicit_tmux_prefix() {
        let source = DEFAULT_CONFIG.replace("tmux_prefix: ctrl+g", "tmux_prefix: cmd+g");

        assert!(Config::parse(&source).is_err());
    }

    #[test]
    fn rejects_invalid_popup_size() {
        let source = DEFAULT_CONFIG.replace("height: \"99%\"", "height: \"120%\"");

        assert!(Config::parse(&source).is_err());
    }
}

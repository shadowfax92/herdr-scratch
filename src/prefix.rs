use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use toml_edit::{DocumentMut, Item};

pub fn tmux_prefix_from_user_config() -> Result<String> {
    let path = config_path();
    let config = match fs::read_to_string(&path) {
        Ok(config) => config,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok("C-b".into()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    tmux_prefix_from_config(&config)
        .with_context(|| format!("failed to read Herdr prefix from {}", path.display()))
}

fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HERDR_CONFIG_PATH") {
        return PathBuf::from(path);
    }
    if let Some(root) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(root).join("herdr/config.toml");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config/herdr/config.toml");
    }
    std::env::temp_dir().join("herdr/config.toml")
}

fn tmux_prefix_from_config(config: &str) -> Result<String> {
    let doc = config.parse::<DocumentMut>()?;
    let prefix = doc
        .get("keys")
        .and_then(Item::as_table)
        .and_then(|keys| keys.get("prefix"))
        .and_then(Item::as_str)
        .unwrap_or("ctrl+b");
    normalize_tmux_key(prefix)
}

pub fn normalize_tmux_key(prefix: &str) -> Result<String> {
    let value = prefix.trim().to_ascii_lowercase();
    if value.is_empty() {
        bail!("Herdr prefix cannot be empty");
    }
    if matches!(value.as_str(), "esc" | "escape") {
        return Ok("Escape".into());
    }
    if let Some(number) = value
        .strip_prefix('f')
        .and_then(|value| value.parse::<u8>().ok())
    {
        if (1..=24).contains(&number) {
            return Ok(format!("F{number}"));
        }
    }

    let parts = value.split('+').collect::<Vec<_>>();
    if parts.len() == 1 {
        return tmux_base_key(parts[0]);
    }

    let (modifiers, base) = parts.split_at(parts.len() - 1);
    let mut control = false;
    let mut alt = false;
    let mut shift = false;
    for modifier in modifiers {
        match *modifier {
            "ctrl" | "control" => control = true,
            "alt" | "option" | "meta" => alt = true,
            "shift" => shift = true,
            "cmd" | "command" | "super" => {
                bail!("Herdr prefix `{prefix}` uses Command/Super, which tmux cannot receive")
            }
            _ => bail!("unsupported Herdr prefix modifier `{modifier}` in `{prefix}`"),
        }
    }

    let mut base = tmux_base_key(base[0])?;
    if shift && base.len() == 1 && base.as_bytes()[0].is_ascii_alphabetic() {
        base.make_ascii_uppercase();
        shift = false;
    }

    let mut tmux_modifiers = Vec::new();
    if control {
        tmux_modifiers.push("C");
    }
    if alt {
        tmux_modifiers.push("M");
    }
    if shift {
        tmux_modifiers.push("S");
    }
    if tmux_modifiers.is_empty() {
        return Ok(base);
    }
    Ok(format!("{}-{base}", tmux_modifiers.join("-")))
}

fn tmux_base_key(key: &str) -> Result<String> {
    let named = match key {
        "space" => Some("Space"),
        "tab" => Some("Tab"),
        "enter" | "return" => Some("Enter"),
        "backspace" => Some("BSpace"),
        "delete" => Some("DC"),
        "up" => Some("Up"),
        "down" => Some("Down"),
        "left" => Some("Left"),
        "right" => Some("Right"),
        _ => None,
    };
    if let Some(named) = named {
        return Ok(named.into());
    }
    if key.chars().count() == 1 {
        return Ok(key.to_string());
    }
    bail!("unsupported Herdr prefix key `{key}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_current_prefix() {
        assert_eq!(
            tmux_prefix_from_config("[keys]\nprefix = \"ctrl+a\"\n").unwrap(),
            "C-a"
        );
    }

    #[test]
    fn maps_shifted_and_named_keys() {
        assert_eq!(normalize_tmux_key("alt+shift+x").unwrap(), "M-X");
        assert_eq!(normalize_tmux_key("ctrl+space").unwrap(), "C-Space");
        assert_eq!(normalize_tmux_key("escape").unwrap(), "Escape");
    }

    #[test]
    fn rejects_command_prefixes() {
        assert!(normalize_tmux_key("cmd+a").is_err());
    }
}

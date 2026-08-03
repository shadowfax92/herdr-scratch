<div align="center">

# 🪟 Herdr Scratch

**Persistent per-pane scratch popups for Herdr.**

*Native Herdr popups outside; private tmux sessions preserving state inside.*

[![Herdr 0.7.5+](https://img.shields.io/badge/Herdr-0.7.5%2B-6c71c4)](https://herdr.dev)
[![Rust](https://img.shields.io/badge/built%20with-Rust-b7410e)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Herdr Scratch gives every Herdr pane its own persistent Neovim scratch and full tmux shell workspace. Hide a popup and open it again later: the processes, windows, panes, and terminal contents are still there.

- **Native popups** — Herdr owns placement, focus, dimensions, and backdrop rendering.
- **Stateful toggles** — private tmux servers keep each scratch alive while hidden.
- **Per-pane identities** — Neovim scratches and shell workspaces never collide across source panes.
- **Responsive profiles** — popup dimensions can follow the active Herdr client width.
- **Project-aware cwd** — moving the source pane to another directory recreates its scratches there.
- **Familiar controls** — Neovim mirrors Herdr's prefix; the shell loads your full tmux configuration under a separate prefix.

## Install

Requires macOS, [Herdr](https://herdr.dev) 0.7.5 or newer, tmux, and a Rust toolchain. Neovim is required only for the default `nvim` scratch.

```sh
herdr plugin install shadowfax92/herdr-scratch
```

Add the two actions to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "alt+i"
type = "plugin_action"
command = "shadowfax.scratch.toggle-nvim"
description = "Toggle pane scratch nvim"

[[keys.command]]
key = "alt+o"
type = "plugin_action"
command = "shadowfax.scratch.toggle-shell"
description = "Toggle pane scratch shell"
```

Reload the running server:

```sh
herdr server reload-config
```

## Keys

| Key | Result |
| --- | --- |
| `Alt-i` | Toggle this pane's persistent Neovim |
| `Alt-o` | Toggle this pane's full tmux shell workspace |
| any configured scratch key | Hide the currently open scratch popup |
| `prefix prefix` | Send the prefix through to the program inside |

The minimal Neovim scratch inherits Herdr's prefix. The shell workspace uses its configured `tmux_prefix` and otherwise retains your normal tmux bindings. With the default configuration, `Ctrl-g` controls shell windows and panes while `Ctrl-a` remains available to Herdr and your normal tmux server.

## Configuration

The first toggle creates `config.yaml` from [config.default.yaml](config.default.yaml). Find its directory with:

```sh
herdr plugin config-dir shadowfax.scratch
```

Each scratch selects a command, a key used for hiding, and optional dimensions:

```yaml
default_popup: { width: "90%", height: "99%" }

scratches:
  nvim:
    command: ["nvim"]
    tmx_type: vim
    key: alt+i

  shell:
    shell: true
    tmx_type: sh
    tmux_mode: workspace
    tmux_prefix: ctrl+g
    key: alt+o
```

Popup sizes accept either positive cell counts or percentages from `1%` through `100%`.

### Responsive profiles

Profiles are checked in order against the active Herdr client width. The first match wins; unspecified scratches fall back to their scratch-level or default dimensions.

```yaml
profiles:
  - name: laptop
    match: { max_client_width: 310 }
    popups:
      nvim: { width: "95%", height: "99%" }
      shell: { width: "95%", height: "99%" }

  - name: full-ultrawide
    match: { min_client_width: 400 }
    popups:
      nvim: { width: "70%", height: "99%" }
      shell: { width: "80%", height: "99%" }
```

The configuration is loaded on every toggle, so size and command changes do not require a Herdr reload. Minimal scratches inherit Herdr's prefix. A `tmux_mode: workspace` scratch requires an explicit `tmux_prefix`, loads the normal user tmux configuration, and keeps that configuration's status, navigation, plugins, and session switching.

## Full tmux workspace

The shell workspace runs on its own named tmux server, separate from both the normal tmux server and the minimal Neovim scratch server. Your normal tmux configuration is loaded without copying it. Scratch overlays only the workspace prefix and configured popup-hide keys.

Pressing `Alt-o` inside the popup detaches its tmux client. The popup command then exits, so Herdr closes the popup while the workspace server keeps every window, pane, and process alive. The next `Alt-o` from the same Herdr pane attaches to that session again. Configuration reloads reapply the Scratch overlay automatically.

## Add another scratch

Scratch definitions are data-driven, but Herdr actions are declared in the plugin manifest. For a custom scratch, use a local clone or fork and add both pieces.

Add the scratch to `config.yaml`:

```yaml
scratches:
  lazygit:
    command: ["lazygit"]
    tmx_type: lazygit
    key: alt+g
```

Scratches use the minimal server unless they set `tmux_mode: workspace` together with a `tmux_prefix`.

Add a matching action to `herdr-plugin.toml`:

```toml
[[actions]]
id = "toggle-lazygit"
title = "Toggle scratch lazygit"
contexts = ["pane"]
command = ["./target/release/herdr-scratch", "toggle", "--scratch", "lazygit"]
```

Then bind `shadowfax.scratch.toggle-lazygit` in Herdr and re-link the local checkout.

## How persistence works

Each scratch session is identified by the scratch name, source pane, and Herdr server. Herdr renders the popup, while a private tmux server under the plugin state directory owns the long-running process. Minimal and workspace scratches use separate servers so their prefix, status, and key behavior remain independent. The popup client detaches when hidden and reattaches on the next toggle; persistence ends if its private tmux server exits.

Scratch sessions expose these compatibility variables:

- `TMX_SCRATCH=1`
- `TMX_SCRATCH_TYPE=<tmx_type>`
- `TMX_PARENT_PANE=<source pane>`
- `HERDR_SCRATCH_KIND=<scratch name>`
- `HERDR_SCRATCH_SOURCE_PANE=<source pane>`

## Local development

```sh
git clone https://github.com/shadowfax92/herdr-scratch.git
cd herdr-scratch
herdr plugin link .
```

Run the local gate:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

## License

[MIT](LICENSE)

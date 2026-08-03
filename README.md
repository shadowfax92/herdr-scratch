<div align="center">

# 🪟 Herdr Scratch

**Persistent per-pane scratch popups for Herdr.**

*Native Herdr popups outside; private tmux sessions preserving state inside.*

[![Herdr 0.7.5+](https://img.shields.io/badge/Herdr-0.7.5%2B-6c71c4)](https://herdr.dev)
[![Rust](https://img.shields.io/badge/built%20with-Rust-b7410e)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Herdr Scratch gives every Herdr pane its own persistent Neovim, shell, and tmux-session popup. Hide a popup and open it again later: the process, working state, and terminal contents are still there.

- **Native popups** — Herdr owns placement, focus, dimensions, and backdrop rendering.
- **Stateful toggles** — a private tmux server keeps each scratch alive while hidden.
- **Per-pane identities** — Neovim, shell, and tmux scratches never collide across source panes.
- **Responsive profiles** — popup dimensions can follow the active Herdr client width.
- **Project-aware cwd** — moving the source pane to another directory recreates its scratches there.
- **Familiar controls** — the current Herdr prefix is mirrored inside the scratch session.

## Install

Requires macOS, [Herdr](https://herdr.dev) 0.7.5 or newer, tmux, and a Rust toolchain. Neovim is required only for the default `nvim` scratch.

```sh
herdr plugin install shadowfax92/herdr-scratch
```

Add the three actions to `~/.config/herdr/config.toml`:

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

[[keys.command]]
key = "alt+t"
type = "plugin_action"
command = "shadowfax.scratch.toggle-tmux"
description = "Toggle pane tmux sessions"
```

Reload the running server:

```sh
herdr server reload-config
```

## Keys

| Key | Result |
| --- | --- |
| `Alt-i` | Toggle this pane's persistent Neovim |
| `Alt-o` | Toggle this pane's persistent login shell |
| `Alt-t` | Toggle a shell prepared for your normal tmux server |
| any configured scratch key | Hide the currently open scratch popup |
| `prefix x` | Confirm and terminate the current scratch session |
| `prefix prefix` | Send the prefix through to the program inside |

For example, with Herdr's prefix set to `Ctrl-a`, use `Ctrl-a x` to terminate a scratch. Hiding and terminating are different: hiding preserves state; terminating starts a fresh session the next time you toggle it.

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
    key: alt+o

  tmux:
    shell: true
    clear_tmux_env: true
    tmx_type: tmux
    key: alt+t
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

The configuration is loaded on every toggle, so size and command changes do not require a Herdr reload. If a command changes while its scratch session is alive, terminate that session once with `prefix x` before reopening it.

## Existing tmux sessions

The `tmux` scratch deliberately removes `TMUX`, `TMUX_PANE`, and `TMUX_TMPDIR` before starting its shell. Commands inside it therefore target your normal tmux server instead of Herdr Scratch's private server:

```sh
tmux list-sessions
tmux attach -t <session>
```

Hiding the popup leaves both the attached client and the target tmux session running.

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

Each scratch session is identified by the scratch name, source pane, and Herdr server. Herdr renders the popup, while a private tmux server under the plugin state directory owns the long-running process. The popup client detaches when hidden and reattaches on the next toggle.

Scratch sessions expose these compatibility variables:

- `TMX_SCRATCH=1`
- `TMX_SCRATCH_TYPE=<tmx_type>`
- `TMX_PARENT_PANE=<source pane>`
- `HERDR_SCRATCH_KIND=<scratch name>`
- `HERDR_SCRATCH_SOURCE_PANE=<source pane>`
- `HERDR_SOCKET_PATH=<originating Herdr API socket>` when Herdr provides one

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

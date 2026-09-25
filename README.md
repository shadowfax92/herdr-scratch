<div align="center">

# 🪟 Herdr Scratch

**Persistent per-pane scratch popups for Herdr.**

*Native Herdr popups outside; private tmux sessions preserving state inside.*

[![Herdr 0.7.5+](https://img.shields.io/badge/Herdr-0.7.5%2B-6c71c4)](https://herdr.dev)
[![Rust](https://img.shields.io/badge/built%20with-Rust-b7410e)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Herdr Scratch gives every Herdr pane two persistent tmux workspaces: one starts in Neovim, the other in your shell. Both support multiple editors, shells, windows, and split panes. Hide a popup and open it again later: the processes, windows, panes, and terminal contents are still there.

- **Native popups** — Herdr owns placement, focus, dimensions, and backdrop rendering.
- **Stateful toggles** — private tmux servers keep each scratch alive while hidden.
- **Per-pane identities** — Neovim scratches and shell workspaces never collide across source panes.
- **Responsive profiles** — popup dimensions can follow the active Herdr client width.
- **Project-aware cwd** — new workspaces start in the source pane's directory; existing workspaces survive directory changes.
- **Familiar controls** — both workspaces load your full tmux configuration with a configurable prefix.

## Install

Requires macOS or Linux (including WSL), [Herdr](https://herdr.dev) 0.7.5 or newer, tmux, and a Rust toolchain. Neovim is required only for the default `nvim` scratch.

On Windows, run Herdr and this plugin inside WSL. Native Windows is not supported.

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
key = "alt+0"
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
| `Alt-i` | Toggle this pane's full tmux workspace, starting in Neovim |
| `Alt-0` | Toggle this pane's full tmux shell workspace |
| any configured scratch key | Hide the currently open scratch popup |
| `prefix prefix` | Send the prefix through to the program inside |

Both workspaces use their configured `tmux_prefix` and otherwise retain your normal tmux bindings. With the default configuration, `Ctrl-a` controls tmux while the popup is focused and returns to Herdr when the popup is hidden. Standard tmux bindings include `prefix c` for a new shell window, `prefix %` for a split to the right, and `prefix "` for a split below. Run `nvim` in any shell to open another editor; your tmux configuration can customize these bindings.

## Configuration

The first toggle creates `config.yaml` from [config.default.yaml](config.default.yaml). Find its directory with:

```sh
herdr plugin config-dir shadowfax.scratch
```

Each scratch selects a command, a key used for hiding, and optional dimensions:

```yaml
default_popup: { width: "90%", height: "95%" }

scratches:
  nvim:
    command: ["nvim"]
    tmx_type: vim
    tmux_mode: workspace
    tmux_prefix: ctrl+a
    key: alt+i

  shell:
    shell: true
    tmx_type: sh
    tmux_mode: workspace
    tmux_prefix: ctrl+a
    key: alt+0
```

Popup sizes accept either positive cell counts or percentages from `1%` through `100%`.

### Responsive profiles

Profiles are checked in order against the active Herdr client width. The first match wins; unspecified scratches fall back to their scratch-level or default dimensions.

```yaml
profiles:
  - name: laptop
    match: { max_client_width: 310 }
    popups:
      nvim: { width: "95%", height: "95%" }
      shell: { width: "95%", height: "95%" }

  - name: full-ultrawide
    match: { min_client_width: 400 }
    popups:
      nvim: { width: "70%", height: "95%" }
      shell: { width: "80%", height: "95%" }
```

The configuration is loaded on every toggle, so size and command changes do not require a Herdr reload. Minimal scratches inherit Herdr's prefix. A `tmux_mode: workspace` scratch requires an explicit `tmux_prefix`, loads the normal user tmux configuration, and keeps that configuration's status, navigation, plugins, and session switching.

## Full tmux workspace

The Neovim and shell workspaces have separate sessions on a private named tmux server, separate from the normal tmux server and any minimal scratches. Your normal tmux configuration is loaded without copying it. Scratch overlays only the workspace prefix and configured popup-hide keys. Workspace scratches share the server's prefix and key tables, so configure the same `tmux_prefix` for them.

The configured command starts only the first pane: Neovim for `Alt-i`, your login shell for `Alt-0`. Additional windows and splits follow your normal tmux configuration. Closing one editor leaves the other panes running; closing the final pane ends that session.

Pressing either scratch key inside the popup detaches its tmux client. The popup command then exits, so Herdr closes the popup while the workspace server keeps every window, pane, and process alive. Opening the same scratch from the same Herdr pane attaches to that session again. Configuration reloads reapply the Scratch overlay automatically.

Existing installations keep their `config.yaml`. To enable the Neovim workspace, add `tmux_mode: workspace` and `tmux_prefix: ctrl+a` under `scratches.nvim`, as above. The next open uses a new workspace session. An existing minimal Neovim session stays on its original server under the normal cleanup policy; its buffers are not migrated or closed by this change. To reopen it, temporarily set `tmux_mode: minimal` with the source pane in its original directory: minimal mode recreates sessions when that directory changes.

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

Each scratch session is identified by the scratch name, source pane, and Herdr server. Herdr renders the popup, while a private tmux server under the plugin state directory owns the long-running process. Minimal and workspace scratches use separate servers so their prefix, status, and key behavior remain independent. The popup client detaches when hidden and reattaches on the next toggle. The background retention policy below limits how long hidden sessions remain alive.

### Background cleanup

Scratch removes sessions whose source terminal has closed. It also removes hidden sessions unused for **24 hours** by default, including any editors, agents, or background jobs still running inside them. Attached popups never expire by TTL. A workspace move preserves the source terminal identity and does not trigger cleanup.

Configure retention in the existing `config.yaml`:

```yaml
cleanup:
  enabled: true
  ttl_hours: 24
  interval_seconds: 60
```

Last use includes attachment and keyboard/mouse activity. The worker also renews the timestamp while a client is attached; background program output alone does not renew it. Legacy configurations inherit these defaults. Invalid cleanup settings suspend deletion rather than falling back to defaults.

Herdr's startup hook launches a separate worker process. Every sweep reloads configuration, snapshots the two private tmux servers and relevant Herdr servers, and reclaims at most 16 sessions with a two-second work budget between operations. In-flight requests are separately bounded to 500 ms. A large backlog is reclaimed over several sweeps. Neither popup toggle nor creation scans sessions or waits for this worker.

Only sessions matching Scratch's naming and ownership metadata are eligible. The worker resolves old pane aliases and remembers stable terminal IDs. A failed Herdr connection does not imply closure; the independent TTL still applies. Immediately before removing a session it rechecks tmux identity and, for TTL, attachment/activity, so a newly reopened scratch is preserved.

For manual inspection, set `HERDR_PLUGIN_STATE_DIR` and `HERDR_PLUGIN_CONFIG_DIR` to this plugin's directories (Herdr supplies both to plugin commands), then run:

```sh
herdr-scratch cleanup          # read-only preview of one bounded sweep
herdr-scratch cleanup --apply  # perform one bounded sweep
herdr-scratch cleanup-start    # ensure the background worker is running
herdr-scratch cleanup-stop     # stop the worker without closing scratches
```

Use `cleanup.enabled: false` to disable retention persistently; startup and pane-close hooks may restart a manually stopped worker. `cleanup-status.json` in the state directory records the latest sweep, and `cleanup.log` records worker errors. Tests use isolated servers only:

```sh
cargo test --locked
cargo test cleanup::real_tests --locked -- --ignored
python3 tests/cleanup_real.py target/debug/herdr-scratch
```

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

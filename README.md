# Herdr Scratch

Persistent nvim and shell popups for each Herdr pane. The popup is native Herdr UI; a private tmux session preserves the program while it is hidden.

## Behavior

- `Alt-i` opens or hides nvim for the focused Herdr pane.
- `Alt-o` opens or hides a shell for the focused Herdr pane.
- Each pane has separate nvim and shell sessions, created in that pane's cwd.
- Changing the source pane's cwd recreates its scratch session in the new path.
- `Ctrl-a x` inside a popup confirms before terminating its tmux session.
- Exiting nvim or the shell terminates that scratch session.

Both `Alt-i` and `Alt-o` hide an open popup. Press the wanted key again to open the other scratch type.

## Install

```sh
cargo build --release
herdr plugin link /Users/shadowfax/code/side-projects/herdr-custom-plugins/my/herdr-scratch
```

Add these commands to `~/.config/herdr/config.toml`:

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

Then run:

```sh
herdr server reload-config
```

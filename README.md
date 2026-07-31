# Herdr Scratch

Configurable persistent scratch popups for each Herdr pane. Herdr draws the native popup while a private tmux server preserves the program when it is hidden.

## Keys

- `Alt-i` toggles nvim.
- `Alt-o` toggles a shell.
- `Alt-t` toggles a shell prepared for attaching existing tmux sessions.
- Any configured scratch key hides the open popup.
- `Ctrl-a x` terminates the current scratch session after confirmation.

The nvim, shell, and tmux scratches have separate identities for every Herdr pane. Changing the source pane's cwd recreates that pane's scratch in the new path. `TMX_SCRATCH=1` remains available inside every scratch.

## Configuration

Edit:

```text
~/.config/herdr/plugins/config/shadowfax.scratch/config.yaml
```

The file is created from [config.default.yaml](config.default.yaml) on first use. Profiles use the active Herdr client width and the first matching profile wins:

```yaml
default_popup: { width: "90%", height: "99%" }

profiles:
  - name: laptop
    match: { max_client_width: 310 }
    popups:
      nvim: { width: "95%", height: "99%" }

  - name: full-ultrawide
    match: { min_client_width: 400 }
    popups:
      nvim: { width: "70%", height: "99%" }
```

Inspect the active profile and resolved sizes:

```sh
/Users/shadowfax/code/side-projects/herdr-custom-plugins/my/herdr-scratch/target/release/herdr-scratch config
```

## Existing tmux sessions

Open the `tmux` scratch with `Alt-t`, then attach normally:

```sh
tmux list-sessions
tmux attach -t <session>
```

That scratch removes the outer private tmux variables before starting its shell, so these commands target your normal tmux server. Hiding the Herdr popup leaves the attached client and tmux session running.

## Adding a scratch

Add a definition to `config.yaml`:

```yaml
scratches:
  lazygit:
    command: ["lazygit"]
    tmx_type: lazygit
    key: alt+g
```

Then add a Herdr binding:

```toml
[[keys.command]]
key = "alt+g"
type = "shell"
command = "/Users/shadowfax/code/side-projects/herdr-custom-plugins/my/herdr-scratch/target/release/herdr-scratch toggle --scratch lazygit"
description = "Toggle pane scratch lazygit"
```

Reload Herdr with `herdr server reload-config`. If a scratch command changes while its tmux session exists, terminate that session once with `Ctrl-a x` before reopening it.

## Install

```sh
cargo build --release
herdr plugin link /Users/shadowfax/code/side-projects/herdr-custom-plugins/my/herdr-scratch --enabled
herdr server reload-config
```

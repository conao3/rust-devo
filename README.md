# devo

devo is a CLI tool that generates tmux session shell commands from a small TOML DSL and runs them via `bash`.

## quick start

```bash
nix develop
make build
make plan
```

Run:

```bash
make run
```

## dsl

The config file is `devo.toml`. Main keys:

- `session`: tmux session name
- `tmux_bin`: tmux command path (default: `tmux`)
- `hook_session_closed`: `session-closed` hook command
- `[env]`: environment variables exported before execution
- `[[tasks]]`: task definitions
  - `id`: task id
  - `pane`: `root` / `right_of:<task-id>` / `down_of:<task-id>`
  - `cmd`: command executed in that pane (multi-line supported)
  - `depends_on`: list of dependent task ids
- `focus`: task id to focus at the end

## commands

```bash
cargo run -- plan -f devo.toml
cargo run -- run -f devo.toml
```

`plan` prints the generated shell script, and `run` executes it with `bash`.

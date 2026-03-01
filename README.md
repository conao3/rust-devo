# devo

devo is a CLI tool that generates tmux session shell commands from a small YAML DSL and runs them via `bash`.

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

The default config file is `devo.yaml`. Main keys:

- `session`: tmux session name
- `tmux_bin`: tmux command path (default: `tmux`)
- `hook_session_closed`: `session-closed` hook command
- `env`: environment variables exported before execution
- `tasks`: task definitions
  - `id`: task id
  - `pane`: `root` / `right_of:<task-id>` / `down_of:<task-id>`
  - `cmd`: command executed in that pane (multi-line supported)
  - `depends_on`: list of dependent task ids
- `focus`: task id to focus at the end

## commands

```bash
cargo run -- plan -f devo.yaml
cargo run -- run -f devo.yaml
```

`plan` prints the generated shell script, and `run` executes it with `bash`.

TOML is still supported. `.yaml`, `.yml`, and `.toml` are auto-detected by file extension.

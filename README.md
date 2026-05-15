# devo

devo is a CLI tool that generates tmux session shell commands from a small YAML DSL and runs them via `bash`.
The name "devo" comes from "dev orchestrator".

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

- `session`: optional tmux session name; defaults to `SESSION_NAME`. Supports shell-style variable expansion: `$VAR`, `${VAR}`, `${VAR:-default}`, `${VAR:+alt}`. The `default` / `alt` segments are themselves expanded (e.g. `rust-sa${SLUG:+-$SLUG}` becomes `rust-sa` when `SLUG` is empty, `rust-sa-foo` when `SLUG=foo`).
- `hook_session_closed`: `session-closed` hook command
- `inherit_env`: list of environment variable names to snapshot once and source before each pane command
- `tasks`: task definitions
  - `id`: task id
  - `pane`: optional; `root` / `right_of:<task-id>` / `down_of:<task-id>`
  - `cmd`: command(s) executed in that pane (`string` or `string[]`)
- `focus`: optional task id to focus at the end

If `pane` is omitted, the first task uses the root pane and later tasks are split below the previous task.
If `focus` is omitted, devo leaves tmux focus where pane creation naturally leaves it.

## examples

### simple example

`devo.yaml`:

```yaml
tasks:
  - id: editor
    cmd: nvim

  - id: logs
    cmd: tail -f /var/log/system.log
```

Layout result (conceptual):

```text
+-------------------------+
| editor (root)           |
| nvim                    |
+-------------------------+
| logs (down_of:editor)   |
| tail -f ...             |
+-------------------------+
```

Generated command flow (simplified):

```text
new-session -> capture root pane id
editor uses root pane
split down from editor -> logs pane
send-keys to editor and logs
select-pane editor
```

### advanced example

`devo.yaml`:

```yaml
hook_session_closed: run-shell 'devo dev-stop'
inherit_env:
  - DEV_CMD
  - DEV_FRONTEND
  - DEV_KINTONE_JS
  - BIND_IP
  - COMPOSE_PROJECT_NAME

tasks:
  - id: backend
    pane: root
    cmd: $DEV_CMD make start-backend-dev

  - id: repl
    pane: right_of:backend
    cmd:
      - $DEV_CMD make -C backend repl NREPL_HOST='${BIND_IP}'
      - (go)

  - id: frontend
    pane: down_of:backend
    cmd: $DEV_CMD $DEV_FRONTEND

  - id: kintone_js
    pane: down_of:frontend
    cmd: $DEV_CMD $DEV_KINTONE_JS

  - id: compose
    pane: down_of:repl
    cmd: env UID=$(id -u) GID=$(id -g) HOST_IP='${BIND_IP}' docker compose -p $COMPOSE_PROJECT_NAME up
```

Layout result (conceptual):

```text
+-----------------------------+-----------------------------+
| backend (root)              | repl (right_of:backend)    |
| make start-backend-dev      | make -C backend repl       |
+-----------------------------+-----------------------------+
| frontend (down_of:backend)  | compose (down_of:repl)     |
| $DEV_FRONTEND               | docker compose up           |
+-----------------------------+-----------------------------+
| kintone_js (down_of:front.) |                             |
| $DEV_KINTONE_JS             |                             |
+-----------------------------+-----------------------------+
```

Execution ordering rules:

```text
1) pane dependency: right_of/down_of requires its base pane task
2) if multiple tasks are available, file order is preserved
```

## commands

```bash
cargo run -- plan -f devo.yaml
cargo run -- run -f devo.yaml
cargo run -- run --session my-worktree -f devo.yaml
cargo run -- run --attach-or-create --session my-worktree -f devo.yaml
cargo run -- status --json --session my-worktree -f devo.yaml
cargo run -- stop --session my-worktree -f devo.yaml
```

`plan` prints the generated shell script, and `run` executes it with `bash`.

`--session` overrides the `session` value from `devo.yaml`. This is intended for external tools that need to choose deterministic tmux session names per worktree or task.

`--attach-or-create` attaches to an existing session when it exists. If it does not exist, devo creates it from the config and attaches afterward.

`status --json` reports whether the tmux session exists, the configured tasks, and the current tmux panes. Devo stores each task id in the pane-local tmux option `@devo_task_id` when creating panes, so external tools can correlate panes with tasks.

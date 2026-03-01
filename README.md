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

## examples

### simple example

`devo.yaml`:

```yaml
session: "demo-simple"
focus: "editor"

tasks:
  - id: "editor"
    pane: "root"
    cmd: "nvim"

  - id: "logs"
    pane: "right_of:editor"
    depends_on: ["editor"]
    cmd: "tail -f /var/log/system.log"
```

Layout result (conceptual):

```text
+-------------------------+-------------------------+
| editor (root)           | logs (right_of:editor) |
| nvim                    | tail -f ...             |
+-------------------------+-------------------------+
```

Generated command flow (simplified):

```text
new-session -> capture root pane id
editor uses root pane
split right from editor -> logs pane
send-keys to editor and logs
select-pane editor
```

### advanced example

`devo.yaml`:

```yaml
session: "$SESSION_NAME"
tmux_bin: "tmux"
hook_session_closed: "run-shell 'devo dev-stop'"
focus: "backend"

env:
  DEV_CMD: "$DEV_CMD"
  DEV_FRONTEND: "$DEV_FRONTEND"
  DEV_KINTONE_JS: "$DEV_KINTONE_JS"
  BIND_IP: "$BIND_IP"
  COMPOSE_PROJECT_NAME: "$COMPOSE_PROJECT_NAME"

tasks:
  - id: "backend"
    pane: "root"
    cmd: "$DEV_CMD make start-backend-dev"

  - id: "repl"
    pane: "right_of:backend"
    depends_on: ["backend"]
    cmd: |
      $DEV_CMD make -C backend repl NREPL_HOST='${BIND_IP}'
      (go)

  - id: "frontend"
    pane: "down_of:backend"
    depends_on: ["backend"]
    cmd: "$DEV_CMD $DEV_FRONTEND"

  - id: "kintone_js"
    pane: "down_of:frontend"
    depends_on: ["frontend"]
    cmd: "$DEV_CMD $DEV_KINTONE_JS"

  - id: "compose"
    pane: "down_of:repl"
    depends_on: ["repl"]
    cmd: "env UID=$(id -u) GID=$(id -g) HOST_IP='${BIND_IP}' docker compose -p $COMPOSE_PROJECT_NAME up"
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
2) explicit dependency: depends_on must run after listed tasks
3) final order: topological sort of (1) + (2)
```

## commands

```bash
cargo run -- plan -f devo.yaml
cargo run -- run -f devo.yaml
```

`plan` prints the generated shell script, and `run` executes it with `bash`.

TOML is still supported. `.yaml`, `.yml`, and `.toml` are auto-detected by file extension.

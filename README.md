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
- `hook_session_started`: command run once in the outer shell after tmux panes and task commands are started, before attach
- `inherit_env`: list of environment variable names to snapshot once and source before each pane command (`--inherit-all-env` of `session start-session` extends this to every variable of the launching process)
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
hook_session_closed: run-shell 'cd $PROJECT_ROOT && nix run $FLAKE_REF#dev-stop'
hook_session_started: |
  echo "session started: $SESSION_NAME"
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

devo follows the AWS CLI grammar: `devo [global options] <command> <subcommand> [--parameter value ...]`.
All parameters are named; there are no positional arguments.

```bash
devo session generate-session-script --file devo.yaml --output text
devo session start-session --file devo.yaml
devo session start-session --session-name my-worktree --file devo.yaml
devo session start-session --attach-or-create --session-name my-worktree
nix develop -c devo session start-session --attach --inherit-all-env
devo session describe-session --session-name my-worktree
devo session stop-session --session-name my-worktree
```

| subcommand | parameters | output |
|---|---|---|
| `session generate-session-script` | `--file`, `--session-name`, `--inherit-all-env` | `Script` |
| `session start-session` | `--file`, `--session-name`, `--attach`, `--attach-or-create`, `--inherit-all-env` | `SessionName`, `Created`, `Attached` |
| `session describe-session` | `--file`, `--session-name` | `SessionName`, `Exists`, `Tasks[].TaskId`, `Tasks[].Pane`, `Panes[].PaneId`, `Panes[].PaneIndex`, `Panes[].TaskId`, `Panes[].Title`, `Panes[].Active`, `Panes[].CurrentCommand`, `Panes[].CurrentPath` |
| `session stop-session` | `--file`, `--session-name` | `SessionName`, `Stopped` |

`--file` defaults to `devo.yaml`. Booleans take `--flag` / `--no-flag`.
A value starting with `file://` is read from that file (`file:///dev/stdin` reads standard input).

Global options: `--output json|text|table` (default `json`), `--debug`, `--version`.
`help` works at every level: `devo help`, `devo session help`, `devo session start-session help`.

Exit codes: `0` success, `252` invalid command line or parameter, `254` failure on the target side (config cannot be read or is invalid, the session does not exist or already exists, tmux or the session script fails), `255` any other failure.

`generate-session-script` returns the generated shell script; `--output text` prints it as is. `start-session` runs the script with `bash` and then prints its result. Without `--attach`, the output of the script goes to stderr so that stdout carries only the result. With `--attach` or `--attach-or-create` the terminal is handed to tmux and the result is printed after you detach.

`--session-name` overrides the `session` value from `devo.yaml`. This is intended for external tools that need to choose deterministic tmux session names per worktree or task.

`--attach-or-create` attaches to an existing session when it exists (`Created` is `false`). If it does not exist, devo creates it from the config and attaches afterward.

`--inherit-all-env` snapshots every environment variable of the process that runs devo and sources it before each pane command, the same way `inherit_env` does for the listed names. tmux panes otherwise start from the tmux server environment, so this is how a `nix develop -c devo session start-session` (or any wrapper that prepares `PATH` and friends) reaches the panes. Shell- and terminal-specific variables (`PWD`, `SHLVL`, `SHELL`, `TERM`, `TMUX`, `TMUX_PANE`, `HOME`, `USER`, `TMPDIR` and the other `nix develop` temp-dir variables, `DIRENV_*`, ...) stay with each pane.

`describe-session` reports whether the tmux session exists (`Exists`; a missing session is not an error), the configured tasks, and the current tmux panes. Devo stores each task id in the pane-local tmux option `@devo_task_id` when creating panes, so external tools can correlate panes with tasks (`Panes[].TaskId`).

`stop-session` kills the session and exits with `254` when the session does not exist.

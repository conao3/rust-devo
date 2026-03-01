#!/usr/bin/env bash
set -euxo pipefail -o posix
TMUX="${TMUX:-tmux}"
SESSION_NAME="$SESSION_NAME"
DEVO_ENV_SNAPSHOT="$(mktemp)"
: > "$DEVO_ENV_SNAPSHOT"
chmod 600 "$DEVO_ENV_SNAPSHOT"
printf 'export %s=%q\n' 'DEV_CMD' "${DEV_CMD-}" >> "$DEVO_ENV_SNAPSHOT"
printf 'export %s=%q\n' 'DEV_FRONTEND' "${DEV_FRONTEND-}" >> "$DEVO_ENV_SNAPSHOT"
printf 'export %s=%q\n' 'DEV_KINTONE_JS' "${DEV_KINTONE_JS-}" >> "$DEVO_ENV_SNAPSHOT"
printf 'export %s=%q\n' 'BIND_IP' "${BIND_IP-}" >> "$DEVO_ENV_SNAPSHOT"
printf 'export %s=%q\n' 'COMPOSE_PROJECT_NAME' "${COMPOSE_PROJECT_NAME-}" >> "$DEVO_ENV_SNAPSHOT"
$TMUX new-session -d -s "$SESSION_NAME"
# tmux set-hook -t <session> session-closed may not fire due to tmux issue #4267
# https://github.com/tmux/tmux/issues/4267
# Workaround: use a global session-closed hook and filter by #{hook_session_name}.
DEVO_SESSION_CLEANUP_SCRIPT="$(mktemp)"
cat > "$DEVO_SESSION_CLEANUP_SCRIPT" <<'__DEVO_HOOK__'
#!/usr/bin/env bash
set -euo pipefail -o posix
hook_session_name="$1"
target_session_name="$2"
if [ "$hook_session_name" != "$target_session_name" ]; then
  exit 0
fi
devo dev-stop
__DEVO_HOOK__
chmod +x "$DEVO_SESSION_CLEANUP_SCRIPT"
$TMUX set-hook -g session-closed "run-shell '$DEVO_SESSION_CLEANUP_SCRIPT #{hook_session_name} $SESSION_NAME'"
ROOT_PANE="$($TMUX list-panes -t \"$SESSION_NAME\" -F '#{pane_id}' | head -n1)"
PANE_BACKEND="$ROOT_PANE"
$TMUX send-keys -t "${PANE_BACKEND}" "source \"$DEVO_ENV_SNAPSHOT\"" Enter
$TMUX send-keys -t "${PANE_BACKEND}" "$DEV_CMD make start-backend-dev" Enter
PANE_REPL="$($TMUX split-window -t "${PANE_BACKEND}" -h -P -F '#{pane_id}')"
$TMUX send-keys -t "${PANE_REPL}" "source \"$DEVO_ENV_SNAPSHOT\"" Enter
$TMUX send-keys -t "${PANE_REPL}" "$DEV_CMD make -C backend repl NREPL_HOST='${BIND_IP}'" Enter
$TMUX send-keys -t "${PANE_REPL}" "source \"$DEVO_ENV_SNAPSHOT\"" Enter
$TMUX send-keys -t "${PANE_REPL}" "(go)" Enter
PANE_FRONTEND="$($TMUX split-window -t "${PANE_BACKEND}" -v -P -F '#{pane_id}')"
$TMUX send-keys -t "${PANE_FRONTEND}" "source \"$DEVO_ENV_SNAPSHOT\"" Enter
$TMUX send-keys -t "${PANE_FRONTEND}" "$DEV_CMD $DEV_FRONTEND" Enter
PANE_KINTONE_JS="$($TMUX split-window -t "${PANE_FRONTEND}" -v -P -F '#{pane_id}')"
$TMUX send-keys -t "${PANE_KINTONE_JS}" "source \"$DEVO_ENV_SNAPSHOT\"" Enter
$TMUX send-keys -t "${PANE_KINTONE_JS}" "$DEV_CMD $DEV_KINTONE_JS" Enter
PANE_COMPOSE="$($TMUX split-window -t "${PANE_REPL}" -v -P -F '#{pane_id}')"
$TMUX send-keys -t "${PANE_COMPOSE}" "source \"$DEVO_ENV_SNAPSHOT\"" Enter
$TMUX send-keys -t "${PANE_COMPOSE}" "env UID=$(id -u) GID=$(id -g) HOST_IP='${BIND_IP}' docker compose -p $COMPOSE_PROJECT_NAME up" Enter
$TMUX select-pane -t "${PANE_BACKEND}"

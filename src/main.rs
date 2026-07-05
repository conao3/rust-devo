use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Parser, Debug)]
#[command(
    name = "devo",
    version,
    about = "Generate and run tmux workflows from a tiny DSL"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Print generated shell script
    Plan {
        /// Path to devo config file (.yaml/.yml)
        #[arg(short, long, default_value = "devo.yaml")]
        file: PathBuf,
        /// Override the tmux session name from config
        #[arg(long)]
        session: Option<String>,
    },
    /// Generate shell script and execute via bash
    Run {
        /// Path to devo config file (.yaml/.yml)
        #[arg(short, long, default_value = "devo.yaml")]
        file: PathBuf,
        /// Override the tmux session name from config
        #[arg(long)]
        session: Option<String>,
        /// Attach to the tmux session after creation
        #[arg(long)]
        attach: bool,
        /// Attach to an existing session, or create it and attach
        #[arg(long)]
        attach_or_create: bool,
    },
    /// Print tmux session status
    Status {
        /// Path to devo config file (.yaml/.yml)
        #[arg(short, long, default_value = "devo.yaml")]
        file: PathBuf,
        /// Override the tmux session name from config
        #[arg(long)]
        session: Option<String>,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Stop the tmux session
    Stop {
        /// Path to devo config file (.yaml/.yml)
        #[arg(short, long, default_value = "devo.yaml")]
        file: PathBuf,
        /// Override the tmux session name from config
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    hook_session_closed: Option<String>,
    #[serde(default)]
    hook_session_started: Option<String>,
    #[serde(default)]
    inherit_env: Vec<String>,
    tasks: Vec<Task>,
    #[serde(default)]
    focus: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct Task {
    id: String,
    #[serde(default)]
    pane: Option<String>,
    cmd: CmdSpec,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
enum CmdSpec {
    One(String),
    Many(Vec<String>),
}

impl CmdSpec {
    fn lines(&self) -> Vec<&str> {
        match self {
            CmdSpec::One(s) => vec![s.as_str()],
            CmdSpec::Many(items) => items.iter().map(|s| s.as_str()).collect(),
        }
    }
}

#[derive(Debug, Clone)]
enum PaneSpec {
    Root,
    RightOf(String),
    DownOf(String),
}

#[derive(Debug, Serialize)]
struct Status {
    session: String,
    exists: bool,
    tasks: Vec<TaskStatus>,
    panes: Vec<PaneStatus>,
}

#[derive(Debug, Serialize)]
struct TaskStatus {
    id: String,
    pane: String,
}

#[derive(Debug, Serialize)]
struct PaneStatus {
    id: String,
    index: String,
    task_id: String,
    title: String,
    active: bool,
    current_command: String,
    current_path: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Plan { file, session } => {
            let cfg = load_config(&file)?;
            let script = generate_script(&cfg, &RunOptions::new(session, false, false), None)?;
            print!("{}", script);
        }
        Commands::Run {
            file,
            session,
            attach,
            attach_or_create,
        } => {
            let cfg = load_config(&file)?;
            let path = std::env::temp_dir().join(format!("devo-{}.sh", std::process::id()));
            let script = generate_script(
                &cfg,
                &RunOptions::new(session, attach, attach_or_create),
                Some(&path),
            )?;
            let mut f = fs::File::create(&path)
                .with_context(|| format!("failed to create temp script: {}", path.display()))?;
            f.write_all(script.as_bytes())
                .context("failed to write temp script")?;
            f.flush().context("failed to flush temp script")?;
            drop(f);
            let err = Command::new("/usr/bin/env")
                .arg("bash")
                .arg("-eux")
                .arg("-o")
                .arg("pipefail")
                .arg("-o")
                .arg("posix")
                .arg(&path)
                .exec();
            bail!("failed to exec bash: {}", err);
        }
        Commands::Status {
            file,
            session,
            json,
        } => {
            let cfg = load_config(&file)?;
            let status = session_status(&cfg, session)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else if status.exists {
                println!("session {} exists", status.session);
                for pane in status.panes {
                    println!(
                        "{}\t{}\t{}\t{}",
                        pane.id, pane.index, pane.title, pane.current_command
                    );
                }
            } else {
                println!("session {} does not exist", status.session);
            }
        }
        Commands::Stop { file, session } => {
            let cfg = load_config(&file)?;
            let session_name = resolve_session_name(&cfg, session)?;
            let _ = Command::new("tmux")
                .arg("kill-session")
                .arg("-t")
                .arg(format!("={session_name}"))
                .status();
        }
    }
    Ok(())
}

struct RunOptions {
    session: Option<String>,
    attach: bool,
    attach_or_create: bool,
}

impl RunOptions {
    fn new(session: Option<String>, attach: bool, attach_or_create: bool) -> Self {
        Self {
            session,
            attach,
            attach_or_create,
        }
    }
}

fn load_config(path: &PathBuf) -> Result<Config> {
    let body = fs::read_to_string(path)
        .with_context(|| format!("failed to read config: {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    let cfg: Config = match ext.as_deref() {
        Some("yaml") | Some("yml") => serde_yaml::from_str(&body)
            .with_context(|| format!("failed to parse YAML: {}", path.display()))?,
        _ => bail!(
            "unsupported config extension: {} (expected .yaml or .yml)",
            path.display()
        ),
    };
    validate_config(&cfg)?;
    Ok(cfg)
}

fn validate_config(cfg: &Config) -> Result<()> {
    if cfg.tasks.is_empty() {
        bail!("tasks must not be empty");
    }

    for name in &cfg.inherit_env {
        validate_env_var_name(name)?;
    }

    let mut ids = HashMap::new();
    for (idx, t) in cfg.tasks.iter().enumerate() {
        if ids.insert(t.id.clone(), idx).is_some() {
            bail!("duplicate task id: {}", t.id);
        }
    }

    let root_count = cfg
        .tasks
        .iter()
        .filter(|t| matches!(effective_pane_spec(cfg, t), Ok(PaneSpec::Root)))
        .count();
    if root_count != 1 {
        bail!("exactly one task must use pane = \"root\" (found {root_count})");
    }

    for t in &cfg.tasks {
        match effective_pane_spec(cfg, t)? {
            PaneSpec::Root => {}
            PaneSpec::RightOf(ref base) | PaneSpec::DownOf(ref base) => {
                if !ids.contains_key(base) {
                    bail!("task {} references unknown pane base {}", t.id, base);
                }
            }
        }
    }

    if let Some(focus) = &cfg.focus {
        if !ids.contains_key(focus) {
            bail!("focus references unknown task {}", focus);
        }
    }

    let _ = topo_sort(cfg)?;
    Ok(())
}

fn effective_pane_spec(cfg: &Config, task: &Task) -> Result<PaneSpec> {
    if let Some(pane) = &task.pane {
        return parse_pane_spec(pane);
    }

    let idx = cfg
        .tasks
        .iter()
        .position(|t| t.id == task.id)
        .ok_or_else(|| anyhow!("unknown task {}", task.id))?;
    if idx == 0 {
        return Ok(PaneSpec::Root);
    }

    Ok(PaneSpec::DownOf(cfg.tasks[idx - 1].id.clone()))
}

fn effective_pane_string(cfg: &Config, task: &Task) -> Result<String> {
    Ok(match effective_pane_spec(cfg, task)? {
        PaneSpec::Root => "root".to_string(),
        PaneSpec::RightOf(base) => format!("right_of:{base}"),
        PaneSpec::DownOf(base) => format!("down_of:{base}"),
    })
}

fn parse_pane_spec(s: &str) -> Result<PaneSpec> {
    if s == "root" {
        return Ok(PaneSpec::Root);
    }
    if let Some(base) = s.strip_prefix("right_of:") {
        if base.is_empty() {
            bail!("pane spec right_of: requires task id");
        }
        return Ok(PaneSpec::RightOf(base.to_string()));
    }
    if let Some(base) = s.strip_prefix("down_of:") {
        if base.is_empty() {
            bail!("pane spec down_of: requires task id");
        }
        return Ok(PaneSpec::DownOf(base.to_string()));
    }
    bail!("invalid pane spec: {s} (expected root | right_of:<task> | down_of:<task>)")
}

fn topo_sort(cfg: &Config) -> Result<Vec<Task>> {
    let mut id_to_idx = HashMap::new();
    for (i, t) in cfg.tasks.iter().enumerate() {
        id_to_idx.insert(t.id.clone(), i);
    }

    let n = cfg.tasks.len();
    let mut indeg = vec![0usize; n];
    let mut graph = vec![Vec::<usize>::new(); n];

    for (i, task) in cfg.tasks.iter().enumerate() {
        match effective_pane_spec(cfg, task)? {
            PaneSpec::Root => {}
            PaneSpec::RightOf(base) | PaneSpec::DownOf(base) => {
                let &d = id_to_idx
                    .get(&base)
                    .ok_or_else(|| anyhow!("unknown pane reference {base}"))?;
                graph[d].push(i);
                indeg[i] += 1;
            }
        }
    }

    let mut queue = BTreeSet::new();
    for (i, deg) in indeg.iter().enumerate() {
        if *deg == 0 {
            queue.insert(i);
        }
    }

    let mut out = Vec::with_capacity(n);
    while let Some(i) = queue.pop_first() {
        out.push(cfg.tasks[i].clone());
        for &next in &graph[i] {
            indeg[next] -= 1;
            if indeg[next] == 0 {
                queue.insert(next);
            }
        }
    }

    if out.len() != n {
        bail!("task graph contains a cycle in pane references");
    }

    Ok(out)
}

fn generate_script(
    cfg: &Config,
    opts: &RunOptions,
    script_path: Option<&PathBuf>,
) -> Result<String> {
    let tasks = topo_sort(cfg)?;

    let mut id_to_var = HashMap::<String, String>::new();
    for t in &cfg.tasks {
        id_to_var.insert(t.id.clone(), format!("PANE_{}", sanitize_var(&t.id)));
    }

    let mut lines = Vec::<String>::new();
    lines.push("#!/usr/bin/env bash".to_string());
    lines.push("set -euxo pipefail -o posix".to_string());
    if let Some(p) = script_path {
        lines.push(format!("rm -f {}", sh_expand_quote(&p.to_string_lossy())));
    }
    lines.push(format!(
        "SESSION_NAME={}",
        session_shell_value(cfg, opts.session.as_deref())
    ));
    let use_inherit_env = !cfg.inherit_env.is_empty();

    if opts.attach_or_create {
        lines.push("if tmux has-session -t \"=$SESSION_NAME\" 2>/dev/null; then".to_string());
        lines.push("  exec tmux attach-session -t \"=$SESSION_NAME\"".to_string());
        lines.push("fi".to_string());
    }

    if use_inherit_env {
        lines.push("DEVO_ENV_SNAPSHOT=\"$(mktemp)\"".to_string());
        lines.push(": > \"$DEVO_ENV_SNAPSHOT\"".to_string());
        lines.push("chmod 600 \"$DEVO_ENV_SNAPSHOT\"".to_string());
        for name in &cfg.inherit_env {
            lines.push(format!(
                "printf 'export %s=%q\\n' '{}' \"${{{}-}}\" >> \"$DEVO_ENV_SNAPSHOT\"",
                name, name
            ));
        }
    }

    lines.push("tmux new-session -d -s \"$SESSION_NAME\"".to_string());

    if let Some(hook) = &cfg.hook_session_closed {
        let normalized_hook = normalize_session_closed_hook(hook);
        lines.push(
            "# tmux set-hook -t <session> session-closed may not fire due to tmux issue #4267"
                .to_string(),
        );
        lines.push("# https://github.com/tmux/tmux/issues/4267".to_string());
        lines.push(
            "# Workaround: use a global session-closed hook and filter by #{hook_session_name}."
                .to_string(),
        );
        lines.push("DEVO_SESSION_CLEANUP_SCRIPT=\"$(mktemp)\"".to_string());
        lines.push("cat > \"$DEVO_SESSION_CLEANUP_SCRIPT\" <<__DEVO_HOOK__".to_string());
        lines.push("#!/usr/bin/env bash".to_string());
        lines.push("set -euo pipefail -o posix".to_string());
        lines.push("hook_session_name=\"\\$1\"".to_string());
        lines.push("target_session_name=\"\\$2\"".to_string());
        lines.push(
            "if [ \"\\$hook_session_name\" != \"\\$target_session_name\" ]; then".to_string(),
        );
        lines.push("  exit 0".to_string());
        lines.push("fi".to_string());
        lines.push("export SESSION_NAME=\"\\$target_session_name\"".to_string());
        for line in normalized_hook.lines() {
            lines.push(line.to_string());
        }
        lines.push("__DEVO_HOOK__".to_string());
        lines.push("chmod +x \"$DEVO_SESSION_CLEANUP_SCRIPT\"".to_string());
        lines.push(
            "DEVO_HOOK_INDEX=$(( $(printf '%s' \"$SESSION_NAME\" | cksum | cut -d' ' -f1) % 2147483647 ))".to_string(),
        );
        lines.push(
            "tmux set-hook -g \"session-closed[$DEVO_HOOK_INDEX]\" \"run-shell '$DEVO_SESSION_CLEANUP_SCRIPT #{hook_session_name} $SESSION_NAME'\""
                .to_string(),
        );
    }

    lines.push(
        "ROOT_PANE=\"$(tmux list-panes -t \"=$SESSION_NAME\" -F '#{pane_id}' | head -n1)\""
            .to_string(),
    );

    for task in tasks {
        let this_var = id_to_var
            .get(&task.id)
            .ok_or_else(|| anyhow!("missing task var for {}", task.id))?
            .clone();

        match effective_pane_spec(cfg, &task)? {
            PaneSpec::Root => {
                lines.push(format!("{}=\"$ROOT_PANE\"", this_var));
            }
            PaneSpec::RightOf(base) => {
                let base_var = id_to_var
                    .get(&base)
                    .ok_or_else(|| anyhow!("missing base task var for {}", base))?;
                lines.push(format!(
                    "{}=\"$(tmux split-window -t \"${{{}}}\" -h -P -F '#{{pane_id}}')\"",
                    this_var, base_var
                ));
            }
            PaneSpec::DownOf(base) => {
                let base_var = id_to_var
                    .get(&base)
                    .ok_or_else(|| anyhow!("missing base task var for {}", base))?;
                lines.push(format!(
                    "{}=\"$(tmux split-window -t \"${{{}}}\" -v -P -F '#{{pane_id}}')\"",
                    this_var, base_var
                ));
            }
        }

        lines.push(format!(
            "tmux select-pane -t \"${{{}}}\" -T {}",
            this_var,
            sh_single_quote(&task.id)
        ));
        lines.push(format!(
            "tmux set-option -p -t \"${{{}}}\" @devo_task_id {}",
            this_var,
            sh_single_quote(&task.id)
        ));

        if use_inherit_env {
            lines.push(format!(
                "tmux send-keys -t \"${{{}}}\" {} Enter",
                this_var,
                sh_expand_quote("source \"$DEVO_ENV_SNAPSHOT\"")
            ));
        }

        for line in task.cmd.lines() {
            if line.trim().is_empty() {
                continue;
            }
            lines.push(format!(
                "tmux send-keys -t \"${{{}}}\" {} Enter",
                this_var,
                sh_single_quote(line)
            ));
        }
    }

    if let Some(focus) = &cfg.focus {
        let var = id_to_var
            .get(focus)
            .ok_or_else(|| anyhow!("focus references unknown task {focus}"))?;
        lines.push(format!("tmux select-pane -t \"${{{}}}\"", var));
    }

    if let Some(hook) = &cfg.hook_session_started {
        lines.push(
            "# hook_session_started runs once after panes and task commands are started."
                .to_string(),
        );
        lines.push("export SESSION_NAME".to_string());
        for line in hook.lines() {
            if line.trim().is_empty() {
                continue;
            }
            lines.push(line.to_string());
        }
    }

    if opts.attach || opts.attach_or_create {
        lines.push("tmux attach-session -t \"=$SESSION_NAME\"".to_string());
    }

    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn session_status(cfg: &Config, session: Option<String>) -> Result<Status> {
    let session_name = resolve_session_name(cfg, session)?;
    let tasks = cfg
        .tasks
        .iter()
        .map(|task| TaskStatus {
            id: task.id.clone(),
            pane: effective_pane_string(cfg, task).unwrap_or_else(|_| "<invalid>".to_string()),
        })
        .collect();

    if !tmux_session_exists(&session_name)? {
        return Ok(Status {
            session: session_name,
            exists: false,
            tasks,
            panes: Vec::new(),
        });
    }

    let output = Command::new("tmux")
        .arg("list-panes")
        .arg("-t")
        .arg(format!("={session_name}"))
        .arg("-F")
        .arg("#{pane_id}\t#{pane_index}\t#{@devo_task_id}\t#{pane_title}\t#{pane_active}\t#{pane_current_command}\t#{pane_current_path}")
        .output()
        .context("failed to run tmux list-panes")?;
    if !output.status.success() {
        bail!(
            "tmux list-panes failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let panes = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let mut fields = line.splitn(7, '\t');
            PaneStatus {
                id: fields.next().unwrap_or_default().to_string(),
                index: fields.next().unwrap_or_default().to_string(),
                task_id: fields.next().unwrap_or_default().to_string(),
                title: fields.next().unwrap_or_default().to_string(),
                active: fields.next().unwrap_or_default() == "1",
                current_command: fields.next().unwrap_or_default().to_string(),
                current_path: fields.next().unwrap_or_default().to_string(),
            }
        })
        .collect();

    Ok(Status {
        session: session_name,
        exists: true,
        tasks,
        panes,
    })
}

fn tmux_session_exists(session: &str) -> Result<bool> {
    let status = Command::new("tmux")
        .arg("has-session")
        .arg("-t")
        .arg(format!("={session}"))
        .stderr(Stdio::null())
        .status()
        .context("failed to run tmux has-session")?;
    Ok(status.success())
}

fn resolve_session_name(cfg: &Config, override_session: Option<String>) -> Result<String> {
    if let Some(session) = override_session {
        if session.is_empty() {
            bail!("--session must not be empty");
        }
        return Ok(session);
    }
    match cfg.session.as_deref() {
        Some(session) => {
            let resolved = expand_env_vars(session)?;
            if resolved.is_empty() {
                bail!("resolved session name is empty");
            }
            Ok(resolved)
        }
        None => {
            let session = std::env::var("SESSION_NAME")
                .context("session is not set and SESSION_NAME is not available")?;
            if session.is_empty() {
                bail!("resolved session name is empty");
            }
            Ok(session)
        }
    }
}

fn session_shell_value(cfg: &Config, override_session: Option<&str>) -> String {
    match override_session {
        Some(session) => sh_single_quote(session),
        None => match cfg.session.as_deref() {
            Some(session) => sh_expand_quote(session),
            None => "\"${SESSION_NAME:?session is not set and SESSION_NAME is not available}\""
                .to_string(),
        },
    }
}

fn expand_env_vars(input: &str) -> Result<String> {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }

        if chars.peek() == Some(&'{') {
            chars.next();
            let mut body = String::new();
            for next in chars.by_ref() {
                if next == '}' {
                    break;
                }
                body.push(next);
            }
            out.push_str(&expand_brace_body(&body)?);
            continue;
        }

        let mut name = String::new();
        while let Some(next) = chars.peek() {
            if next.is_ascii_alphanumeric() || *next == '_' {
                name.push(*next);
                chars.next();
            } else {
                break;
            }
        }
        if name.is_empty() {
            out.push('$');
        } else {
            validate_env_var_name(&name)?;
            out.push_str(&std::env::var(&name).unwrap_or_default());
        }
    }

    Ok(out)
}

fn expand_brace_body(body: &str) -> Result<String> {
    if let Some(idx) = body.find(":-") {
        let name = &body[..idx];
        let default = &body[idx + 2..];
        validate_env_var_name(name)?;
        let value = std::env::var(name).unwrap_or_default();
        return Ok(if value.is_empty() {
            expand_env_vars(default)?
        } else {
            value
        });
    }

    if let Some(idx) = body.find(":+") {
        let name = &body[..idx];
        let alt = &body[idx + 2..];
        validate_env_var_name(name)?;
        let value = std::env::var(name).unwrap_or_default();
        return Ok(if value.is_empty() {
            String::new()
        } else {
            expand_env_vars(alt)?
        });
    }

    validate_env_var_name(body)?;
    Ok(std::env::var(body).unwrap_or_default())
}

fn sh_expand_quote(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`");
    format!("\"{}\"", escaped)
}

fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

fn normalize_session_closed_hook(hook: &str) -> String {
    let trimmed = hook.trim();
    let Some(rest) = trimmed.strip_prefix("run-shell") else {
        return trimmed.to_string();
    };

    let rest = rest.trim();
    if rest.len() >= 2 {
        let first = rest.as_bytes()[0] as char;
        let last = rest.as_bytes()[rest.len() - 1] as char;
        if (first == '\'' && last == '\'') || (first == '"' && last == '"') {
            return rest[1..rest.len() - 1].to_string();
        }
    }
    rest.to_string()
}

fn sanitize_var(id: &str) -> String {
    let mut out = String::new();
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn validate_env_var_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("inherit_env contains empty variable name");
    }
    let first = name
        .chars()
        .next()
        .ok_or_else(|| anyhow!("inherit_env contains empty variable name"))?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        bail!("invalid env variable name in inherit_env: {name}");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid env variable name in inherit_env: {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_opts() -> RunOptions {
        RunOptions::new(None, false, false)
    }

    #[test]
    fn task_commands_are_quoted_without_outer_shell_expansion() {
        let cfg = Config {
            session: Some("test".to_string()),
            hook_session_closed: None,
            hook_session_started: None,
            inherit_env: Vec::new(),
            tasks: vec![Task {
                id: "app".to_string(),
                pane: Some("root".to_string()),
                cmd: CmdSpec::One(
                    "psql \"${DATABASE_URL%%\\?*}\" -c 'select 1' && echo $(id -u)".to_string(),
                ),
            }],
            focus: None,
        };

        let script = generate_script(&cfg, &run_opts(), None).expect("script");

        assert!(script.contains(
            "tmux send-keys -t \"${PANE_APP}\" 'psql \"${DATABASE_URL%%\\?*}\" -c '\"'\"'select 1'\"'\"' && echo $(id -u)' Enter"
        ));
    }

    fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        f();
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn expand_env_vars_supports_plain_and_braced_names() {
        with_env("DEVO_TEST_SESSION_PLAIN", Some("hello"), || {
            assert_eq!(
                expand_env_vars("pre-$DEVO_TEST_SESSION_PLAIN-post").unwrap(),
                "pre-hello-post"
            );
            assert_eq!(
                expand_env_vars("pre-${DEVO_TEST_SESSION_PLAIN}-post").unwrap(),
                "pre-hello-post"
            );
        });
    }

    #[test]
    fn expand_env_vars_supports_default_form() {
        with_env("DEVO_TEST_SLUG_DEFAULT", None, || {
            assert_eq!(
                expand_env_vars("rust-${DEVO_TEST_SLUG_DEFAULT:-fallback}").unwrap(),
                "rust-fallback"
            );
        });
        with_env("DEVO_TEST_SLUG_DEFAULT", Some(""), || {
            assert_eq!(
                expand_env_vars("rust-${DEVO_TEST_SLUG_DEFAULT:-fallback}").unwrap(),
                "rust-fallback"
            );
        });
        with_env("DEVO_TEST_SLUG_DEFAULT", Some("worktree"), || {
            assert_eq!(
                expand_env_vars("rust-${DEVO_TEST_SLUG_DEFAULT:-fallback}").unwrap(),
                "rust-worktree"
            );
        });
    }

    #[test]
    fn expand_env_vars_supports_alternate_form() {
        with_env("DEVO_TEST_SLUG_ALT", None, || {
            assert_eq!(
                expand_env_vars("rust-sa${DEVO_TEST_SLUG_ALT:+-$DEVO_TEST_SLUG_ALT}").unwrap(),
                "rust-sa"
            );
        });
        with_env("DEVO_TEST_SLUG_ALT", Some(""), || {
            assert_eq!(
                expand_env_vars("rust-sa${DEVO_TEST_SLUG_ALT:+-suffix}").unwrap(),
                "rust-sa"
            );
        });
        with_env("DEVO_TEST_SLUG_ALT", Some("feature"), || {
            assert_eq!(
                expand_env_vars("rust-sa${DEVO_TEST_SLUG_ALT:+-suffix}").unwrap(),
                "rust-sa-suffix"
            );
        });
    }

    #[test]
    fn expand_env_vars_rejects_unsupported_operators() {
        assert!(expand_env_vars("${DEVO_TEST_BAD:?missing}").is_err());
        assert!(expand_env_vars("${DEVO_TEST_BAD:=value}").is_err());
    }

    #[test]
    fn expand_env_vars_expands_nested_variables_inside_default_and_alt() {
        with_env("DEVO_TEST_SLUG_NESTED", Some("nested"), || {
            with_env("DEVO_TEST_SLUG_EMPTY", None, || {
                assert_eq!(
                    expand_env_vars(
                        "rust-${DEVO_TEST_SLUG_EMPTY:-pre-$DEVO_TEST_SLUG_NESTED-post}"
                    )
                    .unwrap(),
                    "rust-pre-nested-post"
                );
                assert_eq!(
                    expand_env_vars("rust-${DEVO_TEST_SLUG_NESTED:+-$DEVO_TEST_SLUG_NESTED}")
                        .unwrap(),
                    "rust--nested"
                );
            });
        });
    }

    #[test]
    fn resolve_session_name_rejects_empty_result() {
        with_env("DEVO_TEST_EMPTY_SLUG", None, || {
            let cfg = Config {
                session: Some("${DEVO_TEST_EMPTY_SLUG}".to_string()),
                hook_session_closed: None,
                hook_session_started: None,
                inherit_env: Vec::new(),
                tasks: Vec::new(),
                focus: None,
            };
            assert!(resolve_session_name(&cfg, None).is_err());
        });
    }

    #[test]
    fn env_snapshot_source_still_expands_snapshot_path_in_outer_shell() {
        let cfg = Config {
            session: Some("test".to_string()),
            hook_session_closed: None,
            hook_session_started: None,
            inherit_env: vec!["APP_ROOT".to_string()],
            tasks: vec![Task {
                id: "app".to_string(),
                pane: Some("root".to_string()),
                cmd: CmdSpec::One("cd $APP_ROOT && make dev".to_string()),
            }],
            focus: None,
        };

        let script = generate_script(&cfg, &run_opts(), None).expect("script");

        assert!(script.contains(
            "tmux send-keys -t \"${PANE_APP}\" \"source \\\"$DEVO_ENV_SNAPSHOT\\\"\" Enter"
        ));
        assert!(
            script.contains("tmux send-keys -t \"${PANE_APP}\" 'cd $APP_ROOT && make dev' Enter")
        );
    }

    #[test]
    fn hook_session_started_runs_in_outer_script_after_task_commands() {
        let cfg = Config {
            session: Some("test".to_string()),
            hook_session_closed: None,
            hook_session_started: Some("echo \"$SESSION_NAME\"\necho hook done".to_string()),
            inherit_env: Vec::new(),
            tasks: vec![Task {
                id: "app".to_string(),
                pane: Some("root".to_string()),
                cmd: CmdSpec::One("make dev".to_string()),
            }],
            focus: None,
        };

        let script = generate_script(&cfg, &run_opts(), None).expect("script");

        let task_command = script.find("tmux send-keys -t \"${PANE_APP}\" 'make dev' Enter");
        let hook_export = script.find("export SESSION_NAME");
        let hook_command = script.find("echo \"$SESSION_NAME\"");
        assert!(task_command.is_some());
        assert!(hook_export.is_some());
        assert!(hook_command.is_some());
        assert!(task_command.unwrap() < hook_export.unwrap());
        assert!(hook_export.unwrap() < hook_command.unwrap());
        assert!(
            !script.contains("tmux send-keys -t \"${PANE_APP}\" 'echo \"$SESSION_NAME\"' Enter")
        );
    }
}

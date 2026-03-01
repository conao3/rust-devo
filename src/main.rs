use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;

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
    },
    /// Generate shell script and execute via bash
    Run {
        /// Path to devo config file (.yaml/.yml)
        #[arg(short, long, default_value = "devo.yaml")]
        file: PathBuf,
        /// Attach to the tmux session after creation
        #[arg(long)]
        attach: bool,
    },
}

#[derive(Debug, Deserialize)]
struct Config {
    session: String,
    #[serde(default)]
    hook_session_closed: Option<String>,
    #[serde(default)]
    inherit_env: Vec<String>,
    tasks: Vec<Task>,
    #[serde(default)]
    focus: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct Task {
    id: String,
    pane: String,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Plan { file } => {
            let cfg = load_config(&file)?;
            let script = generate_script(&cfg, false)?;
            print!("{}", script);
        }
        Commands::Run { file, attach } => {
            let cfg = load_config(&file)?;
            let script = generate_script(&cfg, attach)?;
            let mut child = Command::new("/usr/bin/env")
                .arg("bash")
                .arg("-eux")
                .arg("-o")
                .arg("pipefail")
                .arg("-o")
                .arg("posix")
                .stdin(Stdio::piped())
                .spawn()
                .context("failed to execute bash")?;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .context("failed to write script to bash stdin")?;
            let status = child.wait().context("failed to wait for bash")?;
            if !status.success() {
                bail!("generated script exited with status: {}", status);
            }
        }
    }
    Ok(())
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
        .filter(|t| matches!(parse_pane_spec(&t.pane), Ok(PaneSpec::Root)))
        .count();
    if root_count != 1 {
        bail!("exactly one task must use pane = \"root\" (found {root_count})");
    }

    for t in &cfg.tasks {
        match parse_pane_spec(&t.pane)? {
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
        match parse_pane_spec(&task.pane)? {
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

fn generate_script(cfg: &Config, attach: bool) -> Result<String> {
    let tasks = topo_sort(cfg)?;

    let mut id_to_var = HashMap::<String, String>::new();
    for t in &cfg.tasks {
        id_to_var.insert(t.id.clone(), format!("PANE_{}", sanitize_var(&t.id)));
    }

    let mut lines = Vec::<String>::new();
    lines.push("#!/usr/bin/env bash".to_string());
    lines.push("set -euxo pipefail -o posix".to_string());
    lines.push("DEVO_TMUX=\"${DEVO_TMUX:-tmux}\"".to_string());
    lines.push(format!("SESSION_NAME={}", sh_expand_quote(&cfg.session)));
    let use_inherit_env = !cfg.inherit_env.is_empty();

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

    lines.push("$DEVO_TMUX new-session -d -s \"$SESSION_NAME\"".to_string());

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
        lines.push("cat > \"$DEVO_SESSION_CLEANUP_SCRIPT\" <<'__DEVO_HOOK__'".to_string());
        lines.push("#!/usr/bin/env bash".to_string());
        lines.push("set -euo pipefail -o posix".to_string());
        lines.push("hook_session_name=\"$1\"".to_string());
        lines.push("target_session_name=\"$2\"".to_string());
        lines.push("if [ \"$hook_session_name\" != \"$target_session_name\" ]; then".to_string());
        lines.push("  exit 0".to_string());
        lines.push("fi".to_string());
        for line in normalized_hook.lines() {
            lines.push(line.to_string());
        }
        lines.push("__DEVO_HOOK__".to_string());
        lines.push("chmod +x \"$DEVO_SESSION_CLEANUP_SCRIPT\"".to_string());
        lines.push(
            "DEVO_HOOK_INDEX=$(( $(printf '%s' \"$SESSION_NAME\" | cksum | cut -d' ' -f1) % 2147483647 ))".to_string(),
        );
        lines.push(
            "$DEVO_TMUX set-hook -g \"session-closed[$DEVO_HOOK_INDEX]\" \"run-shell '$DEVO_SESSION_CLEANUP_SCRIPT #{hook_session_name} $SESSION_NAME'\""
                .to_string(),
        );
    }

    lines.push(
        "ROOT_PANE=\"$($DEVO_TMUX list-panes -t \"$SESSION_NAME\" -F '#{pane_id}' | head -n1)\""
            .to_string(),
    );

    for task in tasks {
        let this_var = id_to_var
            .get(&task.id)
            .ok_or_else(|| anyhow!("missing task var for {}", task.id))?
            .clone();

        match parse_pane_spec(&task.pane)? {
            PaneSpec::Root => {
                lines.push(format!("{}=\"$ROOT_PANE\"", this_var));
            }
            PaneSpec::RightOf(base) => {
                let base_var = id_to_var
                    .get(&base)
                    .ok_or_else(|| anyhow!("missing base task var for {}", base))?;
                lines.push(format!(
                    "{}=\"$($DEVO_TMUX split-window -t \"${{{}}}\" -h -P -F '#{{pane_id}}')\"",
                    this_var, base_var
                ));
            }
            PaneSpec::DownOf(base) => {
                let base_var = id_to_var
                    .get(&base)
                    .ok_or_else(|| anyhow!("missing base task var for {}", base))?;
                lines.push(format!(
                    "{}=\"$($DEVO_TMUX split-window -t \"${{{}}}\" -v -P -F '#{{pane_id}}')\"",
                    this_var, base_var
                ));
            }
        }

        if use_inherit_env {
            lines.push(format!(
                "$DEVO_TMUX send-keys -t \"${{{}}}\" {} Enter",
                this_var,
                sh_expand_quote("source \"$DEVO_ENV_SNAPSHOT\"")
            ));
        }

        for line in task.cmd.lines() {
            if line.trim().is_empty() {
                continue;
            }
            lines.push(format!(
                "$DEVO_TMUX send-keys -t \"${{{}}}\" {} Enter",
                this_var,
                sh_expand_quote(line)
            ));
        }
    }

    if let Some(focus) = &cfg.focus {
        let var = id_to_var
            .get(focus)
            .ok_or_else(|| anyhow!("focus references unknown task {focus}"))?;
        lines.push(format!("$DEVO_TMUX select-pane -t \"${{{}}}\"", var));
    }

    if attach {
        lines.push("$DEVO_TMUX attach-session -t \"$SESSION_NAME\"".to_string());
    }

    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn sh_expand_quote(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`");
    format!("\"{}\"", escaped)
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

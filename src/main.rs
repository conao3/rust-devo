use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

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
        /// Path to devo config file (.yaml/.yml/.toml)
        #[arg(short, long, default_value = "devo.yaml")]
        file: PathBuf,
    },
    /// Generate shell script and execute via bash
    Run {
        /// Path to devo config file (.yaml/.yml/.toml)
        #[arg(short, long, default_value = "devo.yaml")]
        file: PathBuf,
        /// Print generated script before execution
        #[arg(long)]
        print_script: bool,
    },
}

#[derive(Debug, Deserialize)]
struct Config {
    session: String,
    #[serde(default)]
    tmux_bin: Option<String>,
    #[serde(default)]
    hook_session_closed: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    tasks: Vec<Task>,
    #[serde(default)]
    focus: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct Task {
    id: String,
    pane: String,
    cmd: String,
    #[serde(default)]
    depends_on: Vec<String>,
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
            let script = generate_script(&cfg)?;
            print!("{}", script);
        }
        Commands::Run { file, print_script } => {
            let cfg = load_config(&file)?;
            let script = generate_script(&cfg)?;
            if print_script {
                println!("{}", script);
            }
            let status = Command::new("bash")
                .arg("-eux")
                .arg("-o")
                .arg("pipefail")
                .arg("-o")
                .arg("posix")
                .arg("-c")
                .arg(&script)
                .status()
                .context("failed to execute bash")?;
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
        Some("toml") => toml::from_str(&body)
            .with_context(|| format!("failed to parse TOML: {}", path.display()))?,
        _ => serde_yaml::from_str(&body)
            .or_else(|_| toml::from_str(&body))
            .with_context(|| format!("failed to parse config (YAML/TOML): {}", path.display()))?,
    };
    validate_config(&cfg)?;
    Ok(cfg)
}

fn validate_config(cfg: &Config) -> Result<()> {
    if cfg.tasks.is_empty() {
        bail!("tasks must not be empty");
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
        for dep in &t.depends_on {
            if !ids.contains_key(dep) {
                bail!("task {} depends_on unknown task {}", t.id, dep);
            }
        }
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
        for dep in &task.depends_on {
            let &d = id_to_idx
                .get(dep)
                .ok_or_else(|| anyhow!("unknown dependency {dep}"))?;
            graph[d].push(i);
            indeg[i] += 1;
        }

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

    let mut queue = VecDeque::new();
    for (i, deg) in indeg.iter().enumerate() {
        if *deg == 0 {
            queue.push_back(i);
        }
    }

    let mut out = Vec::with_capacity(n);
    while let Some(i) = queue.pop_front() {
        out.push(cfg.tasks[i].clone());
        for &next in &graph[i] {
            indeg[next] -= 1;
            if indeg[next] == 0 {
                queue.push_back(next);
            }
        }
    }

    if out.len() != n {
        bail!("task graph contains a cycle (depends_on and/or pane references)");
    }

    Ok(out)
}

fn generate_script(cfg: &Config) -> Result<String> {
    let tasks = topo_sort(cfg)?;
    let tmux_bin = cfg.tmux_bin.as_deref().unwrap_or("tmux");

    let mut id_to_var = HashMap::<String, String>::new();
    for t in &cfg.tasks {
        id_to_var.insert(t.id.clone(), format!("PANE_{}", sanitize_var(&t.id)));
    }

    let mut lines = Vec::<String>::new();
    lines.push("#!/usr/bin/env bash".to_string());
    lines.push("set -euxo pipefail -o posix".to_string());
    lines.push(format!("TMUX={}", sh_expand_quote(tmux_bin)));
    lines.push(format!("SESSION_NAME={}", sh_expand_quote(&cfg.session)));

    for (k, v) in &cfg.env {
        lines.push(format!(
            "export {}={}",
            sanitize_env_key(k)?,
            sh_expand_quote(v)
        ));
    }

    lines.push("$TMUX new-session -d -s \"$SESSION_NAME\"".to_string());

    if let Some(hook) = &cfg.hook_session_closed {
        lines.push(format!(
            "$TMUX set-hook -t \"$SESSION_NAME\" session-closed {}",
            sh_expand_quote(hook)
        ));
    }

    lines.push(
        "ROOT_PANE=\"$($TMUX list-panes -t \\\"$SESSION_NAME\\\" -F '#{pane_id}' | head -n1)\""
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
                    "{}=\"$($TMUX split-window -t \"${{{}}}\" -h -P -F '#{{pane_id}}')\"",
                    this_var, base_var
                ));
            }
            PaneSpec::DownOf(base) => {
                let base_var = id_to_var
                    .get(&base)
                    .ok_or_else(|| anyhow!("missing base task var for {}", base))?;
                lines.push(format!(
                    "{}=\"$($TMUX split-window -t \"${{{}}}\" -v -P -F '#{{pane_id}}')\"",
                    this_var, base_var
                ));
            }
        }

        for line in task.cmd.lines() {
            if line.trim().is_empty() {
                continue;
            }
            lines.push(format!(
                "$TMUX send-keys -t \"${{{}}}\" {} Enter",
                this_var,
                sh_expand_quote(line)
            ));
        }
    }

    if let Some(focus) = &cfg.focus {
        let var = id_to_var
            .get(focus)
            .ok_or_else(|| anyhow!("focus references unknown task {focus}"))?;
        lines.push(format!("$TMUX select-pane -t \"${{{}}}\"", var));
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

fn sanitize_env_key(key: &str) -> Result<String> {
    let valid = !key.is_empty()
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !key
            .chars()
            .next()
            .ok_or_else(|| anyhow!("empty env key"))?
            .is_ascii_digit();

    if !valid {
        bail!("invalid env key: {key}");
    }
    Ok(key.to_string())
}

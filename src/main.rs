mod render;

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, ExitCode, Stdio};

use anyhow::{anyhow, bail, Context};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug)]
enum Failure {
    Usage(String),
    Target(String),
    Other(String),
}

impl Failure {
    fn code(&self) -> u8 {
        match self {
            Failure::Usage(_) => 252,
            Failure::Target(_) => 254,
            Failure::Other(_) => 255,
        }
    }

    fn message(&self) -> &str {
        match self {
            Failure::Usage(message) | Failure::Target(message) | Failure::Other(message) => message,
        }
    }

    fn target(error: anyhow::Error) -> Self {
        Failure::Target(format!("{error:#}"))
    }
}

type Result<T> = std::result::Result<T, Failure>;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Flag,
}

struct Param {
    name: &'static str,
    kind: Kind,
    required: bool,
    default: Option<&'static str>,
    help: &'static str,
}

struct Group {
    name: &'static str,
    about: &'static str,
}

struct Operation {
    group: &'static str,
    name: &'static str,
    about: &'static str,
    params: &'static [Param],
    run: fn(&Params) -> Result<Value>,
    output_skeleton: fn() -> Result<Value>,
}

const FILE: Param = Param {
    name: "file",
    kind: Kind::Text,
    required: false,
    default: Some("devo.yaml"),
    help: "Path to the devo config file (.yaml/.yml)",
};

const SESSION_NAME: Param = Param {
    name: "session-name",
    kind: Kind::Text,
    required: false,
    default: None,
    help: "tmux session name; overrides `session` in the config and the SESSION_NAME environment variable",
};

const INHERIT_ALL_ENV: Param = Param {
    name: "inherit-all-env",
    kind: Kind::Flag,
    required: false,
    default: None,
    help: "Snapshot every environment variable of the launching process for the panes",
};

const GROUPS: &[Group] = &[Group {
    name: "session",
    about: "Generate, start, describe, and stop the tmux session defined by a devo config",
}];

const OPERATIONS: &[Operation] = &[
    Operation {
        group: "session",
        name: "generate-session-script",
        about: "Generate the bash script that creates the tmux session, without running it",
        params: &[FILE, SESSION_NAME, INHERIT_ALL_ENV],
        run: generate_session_script,
        output_skeleton: generate_session_script_skeleton,
    },
    Operation {
        group: "session",
        name: "start-session",
        about: "Generate the session script and run it with bash; with --attach the terminal is handed to tmux until detach",
        params: &[
            FILE,
            SESSION_NAME,
            Param {
                name: "attach",
                kind: Kind::Flag,
                required: false,
                default: None,
                help: "Attach to the tmux session after creating it",
            },
            Param {
                name: "attach-or-create",
                kind: Kind::Flag,
                required: false,
                default: None,
                help: "Attach to the session when it exists; otherwise create it and attach",
            },
            INHERIT_ALL_ENV,
        ],
        run: start_session,
        output_skeleton: start_session_skeleton,
    },
    Operation {
        group: "session",
        name: "describe-session",
        about: "Describe whether the tmux session exists, the configured tasks, and the current panes",
        params: &[FILE, SESSION_NAME],
        run: describe_session,
        output_skeleton: describe_session_skeleton,
    },
    Operation {
        group: "session",
        name: "stop-session",
        about: "Kill the tmux session",
        params: &[FILE, SESSION_NAME],
        run: stop_session,
        output_skeleton: stop_session_skeleton,
    },
];

#[derive(Debug)]
struct Params(Map<String, Value>);

impl Params {
    fn optional_text(&self, name: &str) -> Option<&str> {
        self.0.get(&pascal_case(name)).and_then(Value::as_str)
    }

    fn text(&self, name: &str) -> Result<&str> {
        self.optional_text(name)
            .ok_or_else(|| Failure::Usage(format!("missing required parameter --{name}")))
    }

    fn flag(&self, name: &str) -> bool {
        self.0
            .get(&pascal_case(name))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn session_name(&self) -> Result<Option<String>> {
        match self.optional_text("session-name") {
            Some("") => Err(Failure::Usage(
                "--session-name must not be empty".to_owned(),
            )),
            other => Ok(other.map(str::to_owned)),
        }
    }

    fn config(&self) -> Result<Config> {
        load_config(Path::new(self.text("file")?))
    }
}

impl Operation {
    fn command(&self) -> Command {
        let mut command = Command::new(self.name).about(self.about);
        for param in self.params {
            command = match param.kind {
                Kind::Text => command.arg(
                    Arg::new(param.name)
                        .long(param.name)
                        .value_name(param.name.to_uppercase().replace('-', "_"))
                        .value_parser(read_value)
                        .help(param_help(param)),
                ),
                Kind::Flag => {
                    let negated = format!("no-{}", param.name);
                    command
                        .arg(
                            Arg::new(param.name)
                                .long(param.name)
                                .action(ArgAction::SetTrue)
                                .overrides_with(negated.clone())
                                .help(param.help),
                        )
                        .arg(
                            Arg::new(negated.clone())
                                .long(negated)
                                .action(ArgAction::SetTrue)
                                .overrides_with(param.name)
                                .help(format!("Turn off --{}", param.name)),
                        )
                }
            };
        }
        command
            .arg(
                Arg::new("cli-input-json")
                    .long("cli-input-json")
                    .value_name("JSON")
                    .value_parser(read_value)
                    .help("Read parameters from a JSON object with PascalCase keys; command-line values take precedence"),
            )
            .arg(
                Arg::new("generate-cli-skeleton")
                    .long("generate-cli-skeleton")
                    .value_parser(["input", "output"])
                    .num_args(0..=1)
                    .default_missing_value("input")
                    .help("Print a JSON skeleton of the input parameters or of the output, then exit"),
            )
    }

    fn input_skeleton(&self) -> Value {
        Value::Object(
            self.params
                .iter()
                .map(|param| {
                    let value = match param.kind {
                        Kind::Text => Value::String(String::new()),
                        Kind::Flag => Value::Bool(false),
                    };
                    (pascal_case(param.name), value)
                })
                .collect(),
        )
    }

    fn params(&self, matches: &ArgMatches) -> Result<Params> {
        let mut values = Map::new();
        if let Some(input) = matches.get_one::<String>("cli-input-json") {
            let object = match serde_json::from_str(input) {
                Ok(Value::Object(object)) => object,
                Ok(_) => {
                    return Err(Failure::Usage(
                        "--cli-input-json must be a JSON object".to_owned(),
                    ));
                }
                Err(error) => return Err(Failure::Usage(format!("--cli-input-json: {error}"))),
            };
            for (key, value) in object {
                let param = self
                    .params
                    .iter()
                    .find(|param| pascal_case(param.name) == key)
                    .ok_or_else(|| {
                        Failure::Usage(format!(
                            "--cli-input-json: unknown parameter `{key}`; expected {}",
                            self.params
                                .iter()
                                .map(|param| pascal_case(param.name))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?;
                let valid = match param.kind {
                    Kind::Text => value.is_string(),
                    Kind::Flag => value.is_boolean(),
                };
                if !valid {
                    return Err(Failure::Usage(format!(
                        "--cli-input-json: `{key}` must be a {}",
                        match param.kind {
                            Kind::Text => "string",
                            Kind::Flag => "boolean",
                        }
                    )));
                }
                values.insert(key, value);
            }
        }
        for param in self.params {
            let key = pascal_case(param.name);
            match param.kind {
                Kind::Text => {
                    if let Some(value) = matches.get_one::<String>(param.name) {
                        values.insert(key.clone(), Value::String(value.clone()));
                    }
                }
                Kind::Flag => {
                    if matches.get_flag(param.name) {
                        values.insert(key.clone(), Value::Bool(true));
                    } else if matches.get_flag(&format!("no-{}", param.name)) {
                        values.insert(key.clone(), Value::Bool(false));
                    }
                }
            }
            if !values.contains_key(&key) {
                if let Some(default) = param.default {
                    values.insert(key, Value::String(default.to_owned()));
                } else if param.required {
                    return Err(Failure::Usage(format!(
                        "missing required parameter --{} (or `{key}` in --cli-input-json)",
                        param.name
                    )));
                }
            }
        }
        Ok(Params(values))
    }
}

fn param_help(param: &Param) -> String {
    match (param.required, param.default) {
        (true, _) => format!("{} (required)", param.help),
        (false, Some(default)) => format!("{} [default: {default}]", param.help),
        (false, None) => param.help.to_owned(),
    }
}

fn pascal_case(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut characters = word.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().chain(characters).collect::<String>())
                .unwrap_or_default()
        })
        .collect()
}

fn read_value(value: &str) -> std::result::Result<String, String> {
    match value.strip_prefix("file://") {
        Some(path) => fs::read_to_string(path).map_err(|error| format!("{path}: {error}")),
        None => Ok(value.to_owned()),
    }
}

fn cli() -> Command {
    let mut root = Command::new("devo")
        .version(env!("CARGO_PKG_VERSION"))
        .propagate_version(true)
        .about("Generate and run tmux workflows from a small YAML DSL")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            Arg::new("output")
                .long("output")
                .global(true)
                .help_heading("Global Options")
                .value_parser(["json", "text", "table"])
                .default_value("json")
                .help("Output format"),
        )
        .arg(
            Arg::new("query")
                .long("query")
                .global(true)
                .help_heading("Global Options")
                .value_name("JMESPATH")
                .value_parser(read_value)
                .help("JMESPath expression applied to the output"),
        )
        .arg(
            Arg::new("debug")
                .long("debug")
                .global(true)
                .help_heading("Global Options")
                .action(ArgAction::SetTrue)
                .help("Print the resolved operation and parameters to stderr"),
        );
    for group in GROUPS {
        let mut command = Command::new(group.name)
            .about(group.about)
            .subcommand_required(true)
            .arg_required_else_help(true);
        for operation in OPERATIONS
            .iter()
            .filter(|operation| operation.group == group.name)
        {
            command = command.subcommand(operation.command());
        }
        root = root.subcommand(command);
    }
    root
}

fn normalize_help(mut args: Vec<OsString>) -> Vec<OsString> {
    let length = args.len();
    if length >= 3
        && args[length - 1] == "help"
        && !args[length - 2].to_string_lossy().starts_with('-')
    {
        args[length - 1] = OsString::from("--help");
    }
    args
}

fn main() -> ExitCode {
    let args = normalize_help(std::env::args_os().collect());
    let matches = match cli().try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(if error.use_stderr() { 252 } else { 0 });
        }
    };
    match execute(&matches) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("error: {}", failure.message());
            ExitCode::from(failure.code())
        }
    }
}

fn execute(matches: &ArgMatches) -> Result<String> {
    let (group, group_matches) = matches
        .subcommand()
        .ok_or_else(|| Failure::Usage("missing command".to_owned()))?;
    let (name, leaf) = group_matches
        .subcommand()
        .ok_or_else(|| Failure::Usage("missing subcommand".to_owned()))?;
    let operation = OPERATIONS
        .iter()
        .find(|operation| operation.group == group && operation.name == name)
        .ok_or_else(|| Failure::Other(format!("operation `{group} {name}` is not defined")))?;
    let debug = leaf.get_flag("debug");
    if let Some(kind) = leaf.get_one::<String>("generate-cli-skeleton") {
        let skeleton = match kind.as_str() {
            "input" => operation.input_skeleton(),
            _ => (operation.output_skeleton)()?,
        };
        return Ok(render::json(&skeleton));
    }
    let params = operation.params(leaf)?;
    if debug {
        eprintln!("debug: operation: {group} {name}");
        eprintln!("debug: parameters: {}", Value::Object(params.0.clone()));
    }
    let mut output = (operation.run)(&params)?;
    if let Some(expression) = leaf.get_one::<String>("query") {
        if debug {
            eprintln!("debug: query: {expression}");
        }
        output = query(expression, &output)?;
    }
    Ok(match leaf.get_one::<String>("output").map(String::as_str) {
        Some("text") => render::text(&output),
        Some("table") => render::table(&output),
        _ => render::json(&output),
    })
}

fn query(expression: &str, value: &Value) -> Result<Value> {
    let compiled = jmespath::compile(expression)
        .map_err(|error| Failure::Usage(format!("--query: {error}")))?;
    let result = compiled
        .search(value.clone())
        .map_err(|error| Failure::Usage(format!("--query: {error}")))?;
    serde_json::to_value(&*result).map_err(|error| Failure::Other(error.to_string()))
}

fn to_value<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| Failure::Other(error.to_string()))
}

fn blank(value: Value) -> Value {
    match value {
        Value::Null | Value::String(_) => Value::String(String::new()),
        Value::Number(_) => Value::from(0),
        Value::Bool(_) => Value::Bool(true),
        Value::Array(items) => Value::Array(items.into_iter().take(1).map(blank).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, blank(value)))
                .collect(),
        ),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct GenerateSessionScriptOutput {
    script: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StartSessionOutput {
    session_name: String,
    created: bool,
    attached: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct DescribeSessionOutput {
    session_name: String,
    exists: bool,
    tasks: Vec<TaskStatus>,
    panes: Vec<PaneStatus>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct TaskStatus {
    task_id: String,
    pane: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct PaneStatus {
    pane_id: String,
    pane_index: String,
    task_id: String,
    title: String,
    active: bool,
    current_command: String,
    current_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StopSessionOutput {
    session_name: String,
    stopped: bool,
}

fn generate_session_script(params: &Params) -> Result<Value> {
    let cfg = params.config()?;
    let opts = RunOptions::new(
        params.session_name()?,
        false,
        false,
        params.flag("inherit-all-env"),
    );
    let script = generate_script(&cfg, &opts, None).map_err(Failure::target)?;
    to_value(&GenerateSessionScriptOutput { script })
}

fn start_session(params: &Params) -> Result<Value> {
    let cfg = params.config()?;
    let session = params.session_name()?;
    let attach = params.flag("attach");
    let attach_or_create = params.flag("attach-or-create");
    let session_name = resolve_session_name(&cfg, session.clone()).map_err(Failure::target)?;
    let created = !(attach_or_create && tmux_session_exists(&session_name)?);
    let opts = RunOptions::new(
        session,
        attach,
        attach_or_create,
        params.flag("inherit-all-env"),
    );
    let path = std::env::temp_dir().join(format!("devo-{}.sh", std::process::id()));
    let script = generate_script(&cfg, &opts, Some(&path)).map_err(Failure::target)?;
    write_script(&path, &script).map_err(|error| Failure::Other(format!("{error:#}")))?;
    let attached = attach || attach_or_create;
    let stdout = if attached {
        Stdio::inherit()
    } else {
        std::io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .map(Stdio::from)
            .map_err(|error| Failure::Other(format!("failed to duplicate stderr: {error}")))?
    };
    let status = Process::new("/usr/bin/env")
        .args(["bash", "-eux", "-o", "pipefail", "-o", "posix"])
        .arg(&path)
        .stdout(stdout)
        .status()
        .map_err(|error| Failure::Other(format!("failed to run bash: {error}")))?;
    if !status.success() {
        let _ = fs::remove_file(&path);
        return Err(Failure::Target(format!(
            "session script for `{session_name}` failed ({status})"
        )));
    }
    to_value(&StartSessionOutput {
        session_name,
        created,
        attached,
    })
}

fn write_script(path: &Path, script: &str) -> anyhow::Result<()> {
    let mut f = fs::File::create(path)
        .with_context(|| format!("failed to create temp script: {}", path.display()))?;
    f.write_all(script.as_bytes())
        .context("failed to write temp script")?;
    f.flush().context("failed to flush temp script")?;
    Ok(())
}

fn describe_session(params: &Params) -> Result<Value> {
    let cfg = params.config()?;
    to_value(&session_status(&cfg, params.session_name()?)?)
}

fn stop_session(params: &Params) -> Result<Value> {
    let cfg = params.config()?;
    let session_name =
        resolve_session_name(&cfg, params.session_name()?).map_err(Failure::target)?;
    if !tmux_session_exists(&session_name)? {
        return Err(Failure::Target(format!(
            "tmux session `{session_name}` does not exist"
        )));
    }
    let output = Process::new("tmux")
        .arg("kill-session")
        .arg("-t")
        .arg(format!("={session_name}"))
        .output()
        .map_err(|error| Failure::Other(format!("failed to run tmux kill-session: {error}")))?;
    if !output.status.success() {
        return Err(Failure::Target(format!(
            "tmux kill-session failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    to_value(&StopSessionOutput {
        session_name,
        stopped: true,
    })
}

fn generate_session_script_skeleton() -> Result<Value> {
    to_value(&GenerateSessionScriptOutput {
        script: String::new(),
    })
    .map(blank)
}

fn start_session_skeleton() -> Result<Value> {
    to_value(&StartSessionOutput {
        session_name: String::new(),
        created: false,
        attached: false,
    })
    .map(blank)
}

fn describe_session_skeleton() -> Result<Value> {
    to_value(&DescribeSessionOutput {
        session_name: String::new(),
        exists: false,
        tasks: vec![TaskStatus {
            task_id: String::new(),
            pane: String::new(),
        }],
        panes: vec![PaneStatus {
            pane_id: String::new(),
            pane_index: String::new(),
            task_id: String::new(),
            title: String::new(),
            active: false,
            current_command: String::new(),
            current_path: String::new(),
        }],
    })
    .map(blank)
}

fn stop_session_skeleton() -> Result<Value> {
    to_value(&StopSessionOutput {
        session_name: String::new(),
        stopped: false,
    })
    .map(blank)
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

struct RunOptions {
    session: Option<String>,
    attach: bool,
    attach_or_create: bool,
    inherit_all_env: bool,
}

impl RunOptions {
    fn new(
        session: Option<String>,
        attach: bool,
        attach_or_create: bool,
        inherit_all_env: bool,
    ) -> Self {
        Self {
            session,
            attach,
            attach_or_create,
            inherit_all_env,
        }
    }
}

const INHERIT_ALL_ENV_EXCLUDE: &[&str] = &[
    "_",
    "BASHOPTS",
    "HOME",
    "LOGNAME",
    "NIX_BUILD_TOP",
    "NIX_ENFORCE_PURITY",
    "NIX_LOG_FD",
    "NIX_REMOTE",
    "OLDPWD",
    "PPID",
    "PWD",
    "SHELL",
    "SHELLOPTS",
    "SHLVL",
    "SSL_CERT_FILE",
    "TEMP",
    "TEMPDIR",
    "TERM",
    "TMP",
    "TMPDIR",
    "TMUX",
    "TMUX_PANE",
    "TZ",
    "UID",
    "USER",
];

const INHERIT_ALL_ENV_EXCLUDE_PREFIXES: &[&str] = &["DIRENV_"];

fn is_inheritable_env_name(name: &str) -> bool {
    validate_env_var_name(name).is_ok()
        && !INHERIT_ALL_ENV_EXCLUDE.contains(&name)
        && !INHERIT_ALL_ENV_EXCLUDE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn inherit_env_names(cfg: &Config, opts: &RunOptions) -> Vec<String> {
    let mut names = cfg.inherit_env.clone();
    if opts.inherit_all_env {
        let mut extra: BTreeSet<String> = std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| is_inheritable_env_name(name))
            .collect();
        for name in &cfg.inherit_env {
            extra.remove(name);
        }
        names.extend(extra);
    }
    names
}

fn load_config(path: &Path) -> Result<Config> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    if !matches!(ext.as_deref(), Some("yaml") | Some("yml")) {
        return Err(Failure::Usage(format!(
            "unsupported config extension for --file: {} (expected .yaml or .yml)",
            path.display()
        )));
    }
    let body = fs::read_to_string(path).map_err(|error| {
        Failure::Target(format!("failed to read config {}: {error}", path.display()))
    })?;
    let cfg: Config = serde_yaml::from_str(&body).map_err(|error| {
        Failure::Target(format!("failed to parse YAML {}: {error}", path.display()))
    })?;
    validate_config(&cfg).map_err(|error| {
        Failure::Target(format!("invalid config {}: {error:#}", path.display()))
    })?;
    Ok(cfg)
}

fn validate_config(cfg: &Config) -> anyhow::Result<()> {
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

fn effective_pane_spec(cfg: &Config, task: &Task) -> anyhow::Result<PaneSpec> {
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

fn effective_pane_string(cfg: &Config, task: &Task) -> anyhow::Result<String> {
    Ok(match effective_pane_spec(cfg, task)? {
        PaneSpec::Root => "root".to_string(),
        PaneSpec::RightOf(base) => format!("right_of:{base}"),
        PaneSpec::DownOf(base) => format!("down_of:{base}"),
    })
}

fn parse_pane_spec(s: &str) -> anyhow::Result<PaneSpec> {
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

fn topo_sort(cfg: &Config) -> anyhow::Result<Vec<Task>> {
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
) -> anyhow::Result<String> {
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
    let inherit_env_names = inherit_env_names(cfg, opts);
    let use_inherit_env = !inherit_env_names.is_empty();

    if opts.attach_or_create {
        lines.push("if tmux has-session -t \"=$SESSION_NAME\" 2>/dev/null; then".to_string());
        lines.push("  exec tmux attach-session -t \"=$SESSION_NAME\"".to_string());
        lines.push("fi".to_string());
    }

    if use_inherit_env {
        lines.push("DEVO_ENV_SNAPSHOT=\"$(mktemp)\"".to_string());
        lines.push(": > \"$DEVO_ENV_SNAPSHOT\"".to_string());
        lines.push("chmod 600 \"$DEVO_ENV_SNAPSHOT\"".to_string());
        for name in &inherit_env_names {
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

fn session_status(cfg: &Config, session: Option<String>) -> Result<DescribeSessionOutput> {
    let session_name = resolve_session_name(cfg, session).map_err(Failure::target)?;
    let tasks = cfg
        .tasks
        .iter()
        .map(|task| TaskStatus {
            task_id: task.id.clone(),
            pane: effective_pane_string(cfg, task).unwrap_or_else(|_| "<invalid>".to_string()),
        })
        .collect();

    if !tmux_session_exists(&session_name)? {
        return Ok(DescribeSessionOutput {
            session_name,
            exists: false,
            tasks,
            panes: Vec::new(),
        });
    }

    let output = Process::new("tmux")
        .arg("list-panes")
        .arg("-t")
        .arg(format!("={session_name}"))
        .arg("-F")
        .arg("#{pane_id}\t#{pane_index}\t#{@devo_task_id}\t#{pane_title}\t#{pane_active}\t#{pane_current_command}\t#{pane_current_path}")
        .output()
        .map_err(|error| Failure::Other(format!("failed to run tmux list-panes: {error}")))?;
    if !output.status.success() {
        return Err(Failure::Target(format!(
            "tmux list-panes failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let panes = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let mut fields = line.splitn(7, '\t');
            PaneStatus {
                pane_id: fields.next().unwrap_or_default().to_string(),
                pane_index: fields.next().unwrap_or_default().to_string(),
                task_id: fields.next().unwrap_or_default().to_string(),
                title: fields.next().unwrap_or_default().to_string(),
                active: fields.next().unwrap_or_default() == "1",
                current_command: fields.next().unwrap_or_default().to_string(),
                current_path: fields.next().unwrap_or_default().to_string(),
            }
        })
        .collect();

    Ok(DescribeSessionOutput {
        session_name,
        exists: true,
        tasks,
        panes,
    })
}

fn tmux_session_exists(session: &str) -> Result<bool> {
    let status = Process::new("tmux")
        .arg("has-session")
        .arg("-t")
        .arg(format!("={session}"))
        .stderr(Stdio::null())
        .status()
        .map_err(|error| Failure::Other(format!("failed to run tmux has-session: {error}")))?;
    Ok(status.success())
}

fn resolve_session_name(cfg: &Config, override_session: Option<String>) -> anyhow::Result<String> {
    if let Some(session) = override_session {
        if session.is_empty() {
            bail!("--session-name must not be empty");
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

fn expand_env_vars(input: &str) -> anyhow::Result<String> {
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

fn expand_brace_body(body: &str) -> anyhow::Result<String> {
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

fn validate_env_var_name(name: &str) -> anyhow::Result<()> {
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

    #[test]
    fn parser_definition_is_consistent() {
        cli().debug_assert();
    }

    #[test]
    fn trailing_help_becomes_a_help_flag() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            normalize_help(args(&["devo", "session", "start-session", "help"])),
            args(&["devo", "session", "start-session", "--help"])
        );
        assert_eq!(
            normalize_help(args(&[
                "devo",
                "session",
                "stop-session",
                "--session-name",
                "help"
            ])),
            args(&["devo", "session", "stop-session", "--session-name", "help"])
        );
    }

    #[test]
    fn cli_input_json_uses_pascal_case_keys_and_command_line_wins() {
        let matches = cli()
            .try_get_matches_from([
                "devo",
                "session",
                "start-session",
                "--cli-input-json",
                r#"{"SessionName": "from-json", "Attach": true, "InheritAllEnv": true}"#,
                "--session-name",
                "from-flag",
                "--no-attach",
            ])
            .expect("matches");
        let (_, group) = matches.subcommand().expect("group");
        let (name, leaf) = group.subcommand().expect("operation");
        let operation = OPERATIONS
            .iter()
            .find(|operation| operation.name == name)
            .expect("operation");
        let params = operation.params(leaf).expect("params");
        assert_eq!(params.optional_text("session-name"), Some("from-flag"));
        assert_eq!(params.optional_text("file"), Some("devo.yaml"));
        assert!(!params.flag("attach"));
        assert!(params.flag("inherit-all-env"));
    }

    #[test]
    fn describe_session_output_keys_are_pascal_case() {
        let skeleton = describe_session_skeleton().expect("skeleton");
        assert_eq!(
            skeleton,
            serde_json::json!({
                "SessionName": "",
                "Exists": true,
                "Tasks": [{"TaskId": "", "Pane": ""}],
                "Panes": [{
                    "PaneId": "",
                    "PaneIndex": "",
                    "TaskId": "",
                    "Title": "",
                    "Active": true,
                    "CurrentCommand": "",
                    "CurrentPath": ""
                }]
            })
        );
    }

    #[test]
    fn config_errors_are_classified_by_side() {
        assert_eq!(load_config(Path::new("devo.toml")).unwrap_err().code(), 252);
        assert_eq!(
            load_config(Path::new("/nonexistent/devo.yaml"))
                .unwrap_err()
                .code(),
            254
        );
    }

    fn run_opts() -> RunOptions {
        RunOptions::new(None, false, false, false)
    }

    fn inherit_all_env_opts() -> RunOptions {
        RunOptions::new(None, false, false, true)
    }

    fn single_task_config(inherit_env: Vec<String>) -> Config {
        Config {
            session: Some("test".to_string()),
            hook_session_closed: None,
            hook_session_started: None,
            inherit_env,
            tasks: vec![Task {
                id: "app".to_string(),
                pane: Some("root".to_string()),
                cmd: CmdSpec::One("make dev".to_string()),
            }],
            focus: None,
        }
    }

    #[test]
    fn inherit_all_env_snapshots_launcher_environment_except_shell_specific_names() {
        with_env("DEVO_TEST_INHERIT_ALL", Some("1"), || {
            let cfg = single_task_config(vec!["APP_ROOT".to_string()]);

            let script = generate_script(&cfg, &inherit_all_env_opts(), None).expect("script");

            assert!(script.contains(
                "printf 'export %s=%q\\n' 'APP_ROOT' \"${APP_ROOT-}\" >> \"$DEVO_ENV_SNAPSHOT\""
            ));
            assert!(script.contains(
                "printf 'export %s=%q\\n' 'DEVO_TEST_INHERIT_ALL' \"${DEVO_TEST_INHERIT_ALL-}\" >> \"$DEVO_ENV_SNAPSHOT\""
            ));
            assert!(script.contains(
                "printf 'export %s=%q\\n' 'PATH' \"${PATH-}\" >> \"$DEVO_ENV_SNAPSHOT\""
            ));
            for name in INHERIT_ALL_ENV_EXCLUDE {
                assert!(!script.contains(&format!("printf 'export %s=%q\\n' '{name}' ")));
            }
            assert_eq!(
                script
                    .matches("printf 'export %s=%q\\n' 'APP_ROOT' ")
                    .count(),
                1
            );
            assert!(script.contains(
                "tmux send-keys -t \"${PANE_APP}\" \"source \\\"$DEVO_ENV_SNAPSHOT\\\"\" Enter"
            ));
        });
    }

    #[test]
    fn inherit_all_env_is_off_by_default() {
        with_env("DEVO_TEST_INHERIT_OFF", Some("1"), || {
            let cfg = single_task_config(Vec::new());

            let script = generate_script(&cfg, &run_opts(), None).expect("script");

            assert!(!script.contains("DEVO_TEST_INHERIT_OFF"));
            assert!(!script.contains("DEVO_ENV_SNAPSHOT"));
        });
    }

    #[test]
    fn inheritable_env_name_rejects_excluded_and_invalid_names() {
        assert!(is_inheritable_env_name("PATH"));
        assert!(is_inheritable_env_name("PKG_CONFIG_PATH"));
        assert!(!is_inheritable_env_name("TMUX_PANE"));
        assert!(!is_inheritable_env_name("TMPDIR"));
        assert!(!is_inheritable_env_name("DIRENV_DIFF"));
        assert!(!is_inheritable_env_name("BASH_FUNC_foo%%"));
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

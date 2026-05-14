use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value, json};

type CliResult = Result<(), Box<dyn std::error::Error>>;

pub fn run() -> CliResult {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        print_workgroup_probe(None);
        return Ok(());
    };

    match command {
        "broker" => hsp::serve_default().map_err(Into::into),
        "global" => global_command(),
        "hook" => hook_command(&args[1..]),
        "log" => log_command(&args[1..]),
        "mcp" => crate::mcp::run(),
        "run" => run_command(&args[1..]),
        "watch" => watch_command(&args[1..]),
        "ping" => request_and_print("ping", Map::new(), true),
        "status" => request_and_print("status", Map::new(), true),
        "shutdown" => request_and_print("shutdown", Map::new(), false),
        "socket" => {
            println!("{}", hsp::socket_path().display());
            Ok(())
        }
        "workgroup" => {
            print_workgroup_probe(args.get(1).map(PathBuf::from));
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        _ => {
            print_workgroup_probe(Some(PathBuf::from(command)));
            Ok(())
        }
    }
}

fn log_command(args: &[OsString]) -> CliResult {
    let Some(action) = args.first().and_then(|arg| arg.to_str()) else {
        return err("hsp log requires an action");
    };
    if action == "-h" || action == "--help" || action == "help" {
        print_log_help();
        return Ok(());
    }

    let options = LogOptions::parse(action, &args[1..])?;
    let method = options.method()?;
    request_and_print(&method, options.params()?, true)
}

fn hook_command(args: &[OsString]) -> CliResult {
    if args
        .first()
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| matches!(arg, "-h" | "--help" | "help"))
    {
        print_hook_help();
        return Ok(());
    }
    let mut options = HookOptions::parse(args)?;
    let mut payload = String::new();
    std::io::stdin().read_to_string(&mut payload)?;
    if options.message.is_empty() {
        options.message = hook_message(&payload);
    }

    let mut params = Map::new();
    params.insert("workspace_root".to_string(), json!(options.workspace_root));
    params.insert("agent_id".to_string(), json!(options.agent_id));
    params.insert("client_id".to_string(), json!(options.client_id));
    params.insert("now".to_string(), json!(now_seconds()));
    params.insert("message".to_string(), json!(options.message));
    insert_string(&mut params, "files", &options.files);
    insert_string(&mut params, "symbols", &options.symbols);
    insert_string(&mut params, "aliases", &options.aliases);
    params.insert("event_type".to_string(), json!(options.kind));
    params.insert("kind".to_string(), json!(options.kind));
    let mut metadata = BTreeMap::new();
    insert_metadata(&mut metadata, "status", &options.status);
    insert_metadata(&mut metadata, "targets", &options.targets);
    insert_metadata(&mut metadata, "commit", &options.commit);
    if !payload.trim().is_empty() {
        insert_metadata(&mut metadata, "hook_payload", payload.trim());
    }
    if !metadata.is_empty() {
        params.insert("metadata".to_string(), serde_json::to_value(metadata)?);
    }
    request_and_print("bus.event", params, true)
}

#[derive(Debug, Clone)]
struct HookOptions {
    kind: String,
    message: String,
    files: String,
    symbols: String,
    aliases: String,
    status: String,
    targets: String,
    commit: String,
    workspace_root: String,
    agent_id: String,
    client_id: String,
}

impl HookOptions {
    fn parse(args: &[OsString]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            kind: String::new(),
            message: String::new(),
            files: String::new(),
            symbols: String::new(),
            aliases: String::new(),
            status: String::new(),
            targets: String::new(),
            commit: String::new(),
            workspace_root: std::env::current_dir()?.to_string_lossy().into_owned(),
            agent_id: default_agent_id(),
            client_id: default_client_id(),
        };

        let mut index = 0;
        if args.first().and_then(|arg| arg.to_str()) == Some("stdin") {
            index = 1;
            options.kind = args
                .get(index)
                .ok_or("hsp hook stdin requires a kind")?
                .to_string_lossy()
                .into_owned();
            index += 1;
        }

        while index < args.len() {
            let flag = args[index].to_string_lossy();
            let value = match flag.as_ref() {
                "--kind" | "--message" | "--files" | "--symbols" | "--aliases" | "--status"
                | "--targets" | "--commit" | "--workspace-root" | "--root" | "--agent-id"
                | "--client-id" => {
                    index += 1;
                    args.get(index)
                        .ok_or_else(|| format!("{flag} requires a value"))?
                        .to_string_lossy()
                        .into_owned()
                }
                other => return err(format!("unknown hsp hook option: {other}")),
            };

            match flag.as_ref() {
                "--kind" => options.kind = value,
                "--message" => options.message = value,
                "--files" => options.files = value,
                "--symbols" => options.symbols = value,
                "--aliases" => options.aliases = value,
                "--status" => options.status = value,
                "--targets" => options.targets = value,
                "--commit" => options.commit = value,
                "--workspace-root" | "--root" => options.workspace_root = normalize_root(value),
                "--agent-id" => options.agent_id = value,
                "--client-id" => options.client_id = value,
                _ => {}
            }
            index += 1;
        }

        if options.kind.is_empty() {
            return err("hsp hook requires `stdin <kind>` or --kind <kind>");
        }
        Ok(options)
    }
}

fn global_command() -> CliResult {
    let status = request("status", Map::new(), true)?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

fn run_command(args: &[OsString]) -> CliResult {
    if args
        .first()
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| matches!(arg, "-h" | "--help" | "help"))
    {
        print_run_help();
        return Ok(());
    }
    let options = RunOptions::parse(args)?;
    let gate = wait_for_build_gate(&options)?;
    if !gate
        .get("unlocked")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return err(format!(
            "build gate locked: {}",
            gate.get("reason").and_then(Value::as_str).unwrap_or("unknown")
        ));
    }

    let status = std::process::Command::new(&options.argv[0])
        .args(&options.argv[1..])
        .status()?;
    if !options.no_log {
        log_run_result(&options, status.success())?;
    }
    std::process::exit(status.code().unwrap_or(1));
}

#[derive(Debug, Clone)]
struct RunOptions {
    argv: Vec<OsString>,
    timeout_seconds: f64,
    kind: String,
    files: String,
    symbols: String,
    message: String,
    no_log: bool,
    workspace_root: String,
    agent_id: String,
}

impl RunOptions {
    fn parse(args: &[OsString]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            argv: Vec::new(),
            timeout_seconds: 120.0,
            kind: "test.ran".to_string(),
            files: String::new(),
            symbols: String::new(),
            message: String::new(),
            no_log: false,
            workspace_root: std::env::current_dir()?.to_string_lossy().into_owned(),
            agent_id: default_agent_id(),
        };

        let mut index = 0;
        while index < args.len() {
            let arg = args[index].to_string_lossy();
            if arg == "--" {
                options.argv = args[index + 1..].to_vec();
                break;
            }
            match arg.as_ref() {
                "-h" | "--help" | "help" => return err("hsp run help must be the first argument"),
                "--timeout" => {
                    index += 1;
                    options.timeout_seconds = parse_duration_seconds(
                        &args
                            .get(index)
                            .ok_or("--timeout requires a value")?
                            .to_string_lossy(),
                    )?;
                }
                "--kind" => {
                    index += 1;
                    options.kind = args
                        .get(index)
                        .ok_or("--kind requires a value")?
                        .to_string_lossy()
                        .into_owned();
                }
                "--files" => {
                    index += 1;
                    options.files = args
                        .get(index)
                        .ok_or("--files requires a value")?
                        .to_string_lossy()
                        .into_owned();
                }
                "--symbols" => {
                    index += 1;
                    options.symbols = args
                        .get(index)
                        .ok_or("--symbols requires a value")?
                        .to_string_lossy()
                        .into_owned();
                }
                "--message" => {
                    index += 1;
                    options.message = args
                        .get(index)
                        .ok_or("--message requires a value")?
                        .to_string_lossy()
                        .into_owned();
                }
                "--workspace-root" | "--root" => {
                    index += 1;
                    options.workspace_root = normalize_root(
                        args.get(index)
                            .ok_or("--workspace-root requires a value")?
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                "--agent-id" => {
                    index += 1;
                    options.agent_id = args
                        .get(index)
                        .ok_or("--agent-id requires a value")?
                        .to_string_lossy()
                        .into_owned();
                }
                "--no-log" => options.no_log = true,
                value if value.starts_with('-') => {
                    return err(format!("unknown hsp run option: {value}"));
                }
                _ => {
                    options.argv = args[index..].to_vec();
                    break;
                }
            }
            index += 1;
        }

        if options.argv.is_empty() {
            return err("hsp run requires a command after --");
        }
        Ok(options)
    }
}

fn wait_for_build_gate(options: &RunOptions) -> Result<Value, Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    loop {
        let mut params = Map::new();
        params.insert("workspace_root".to_string(), json!(options.workspace_root));
        params.insert("agent_id".to_string(), json!(options.agent_id));
        params.insert("now".to_string(), json!(now_seconds()));
        params.insert("files".to_string(), json!(options.files));
        params.insert("symbols".to_string(), json!(options.symbols));
        params.insert("full_workspace".to_string(), json!(options.files.is_empty()));
        let gate = request("bus.build_gate", params, true)?;
        if gate
            .get("unlocked")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || started.elapsed().as_secs_f64() >= options.timeout_seconds
        {
            return Ok(gate);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn log_run_result(options: &RunOptions, passed: bool) -> Result<(), Box<dyn std::error::Error>> {
    let message = if options.message.is_empty() {
        options
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        options.message.clone()
    };
    let mut params = Map::new();
    params.insert("workspace_root".to_string(), json!(options.workspace_root));
    params.insert("agent_id".to_string(), json!(options.agent_id));
    params.insert("client_id".to_string(), json!(default_client_id()));
    params.insert("now".to_string(), json!(now_seconds()));
    params.insert("message".to_string(), json!(message));
    params.insert("files".to_string(), json!(options.files));
    params.insert("symbols".to_string(), json!(options.symbols));
    params.insert("event_type".to_string(), json!(options.kind));
    params.insert("kind".to_string(), json!(options.kind));
    params.insert(
        "metadata".to_string(),
        json!({
            "status": if passed { "passed" } else { "failed" },
            "targets": options.argv.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" "),
        }),
    );
    request("bus.event", params, true)?;
    Ok(())
}

fn watch_command(args: &[OsString]) -> CliResult {
    if args
        .first()
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| matches!(arg, "-h" | "--help" | "help"))
    {
        print_watch_help();
        return Ok(());
    }
    let options = WatchOptions::parse(args)?;
    let roots = if options.global {
        Vec::new()
    } else if options.roots.is_empty() {
        vec![std::env::current_dir()?.to_string_lossy().into_owned()]
    } else {
        options.roots.clone()
    };

    println!(
        "watch: broker={} scope={} interval={}s",
        hsp::socket_path().display(),
        if options.global {
            "global".to_string()
        } else {
            roots.join(",")
        },
        options.interval_seconds,
    );

    let mut after_id = 0;
    loop {
        let mut params = Map::new();
        params.insert("limit".to_string(), json!(options.limit));
        params.insert("after_id".to_string(), json!(after_id));
        params.insert("now".to_string(), json!(now_seconds()));

        let method = if options.global {
            "bus.recent_all"
        } else {
            params.insert("workspace_roots".to_string(), json!(roots));
            "bus.recent_tree"
        };
        let result = request(method, params, true)?;
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for event in &events {
            println!("{}", compact_event(event));
            if let Some(seq) = event.get("seq").and_then(Value::as_u64) {
                after_id = after_id.max(seq);
            }
        }

        if options.once {
            break;
        }
        std::thread::sleep(Duration::from_secs_f64(options.interval_seconds));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct WatchOptions {
    roots: Vec<String>,
    global: bool,
    once: bool,
    limit: u64,
    interval_seconds: f64,
}

impl WatchOptions {
    fn parse(args: &[OsString]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            roots: Vec::new(),
            global: false,
            once: false,
            limit: 25,
            interval_seconds: 0.5,
        };

        let mut index = 0;
        while index < args.len() {
            let arg = args[index].to_string_lossy();
            match arg.as_ref() {
                "-h" | "--help" | "help" => return err("hsp watch help must be the first argument"),
                "--global" => options.global = true,
                "--once" => options.once = true,
                "--exact" => {}
                "--limit" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or("--limit requires a value")?
                        .to_string_lossy();
                    options.limit = value.parse()?;
                }
                "--interval" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or("--interval requires a value")?
                        .to_string_lossy();
                    options.interval_seconds = value.parse::<f64>()?.max(0.1);
                }
                value if value.starts_with('-') => {
                    return err(format!("unknown hsp watch option: {value}"));
                }
                value => options.roots.push(normalize_root(value.to_string())),
            }
            index += 1;
        }

        Ok(options)
    }
}

#[derive(Debug, Clone)]
struct LogOptions {
    action: String,
    message: String,
    files: String,
    symbols: String,
    aliases: String,
    id: String,
    timeout: String,
    kind: String,
    status: String,
    targets: String,
    commit: String,
    workspace_root: String,
    agent_id: String,
    client_id: String,
    limit: String,
    after_id: String,
}

impl LogOptions {
    fn parse(action: &str, args: &[OsString]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            action: if action == "hook" {
                "event".to_string()
            } else {
                action.to_string()
            },
            message: String::new(),
            files: String::new(),
            symbols: String::new(),
            aliases: String::new(),
            id: String::new(),
            timeout: "3m".to_string(),
            kind: String::new(),
            status: String::new(),
            targets: String::new(),
            commit: String::new(),
            workspace_root: std::env::current_dir()?.to_string_lossy().into_owned(),
            agent_id: default_agent_id(),
            client_id: default_client_id(),
            limit: String::new(),
            after_id: String::new(),
        };

        let mut index = 0;
        while index < args.len() {
            let flag = args[index].to_string_lossy();
            let value = match flag.as_ref() {
                "--message" | "--files" | "--symbols" | "--aliases" | "--id" | "--timeout"
                | "--kind" | "--status" | "--targets" | "--commit" | "--workspace-root"
                | "--root" | "--agent-id" | "--client-id" | "--limit" | "--after-id"
                | "--after-seq" => {
                    index += 1;
                    args.get(index)
                        .ok_or_else(|| format!("{flag} requires a value"))?
                        .to_string_lossy()
                        .into_owned()
                }
                other => return err(format!("unknown hsp log option: {other}")),
            };

            match flag.as_ref() {
                "--message" => options.message = value,
                "--files" => options.files = value,
                "--symbols" => options.symbols = value,
                "--aliases" => options.aliases = value,
                "--id" => options.id = value,
                "--timeout" => options.timeout = value,
                "--kind" => options.kind = value,
                "--status" => options.status = value,
                "--targets" => options.targets = value,
                "--commit" => options.commit = value,
                "--workspace-root" | "--root" => options.workspace_root = normalize_root(value),
                "--agent-id" => options.agent_id = value,
                "--client-id" => options.client_id = value,
                "--limit" => options.limit = value,
                "--after-id" | "--after-seq" => options.after_id = value,
                _ => {}
            }
            index += 1;
        }

        Ok(options)
    }

    fn method(&self) -> Result<String, Box<dyn std::error::Error>> {
        let suffix = match self.action.as_str() {
            "event" => "event",
            "note" => "note",
            "ask" => "ask",
            "reply" => "reply",
            "chat" => "chat",
            "ticket" => "ticket",
            "journal" => "journal",
            "question" => "question",
            "edit_gate" => "edit_gate",
            "build_gate" => "build_gate",
            "recent" => "recent",
            "recent_all" => "recent_all",
            "recent_tree" => "recent_tree",
            "settle" => "settle",
            "precommit" => "precommit",
            "postcommit" => "postcommit",
            "weather" => "weather",
            "presence" | "workgroup" => "presence",
            "status" => "status",
            other => return err(format!("unknown hsp log action: {other}")),
        };
        Ok(format!("bus.{suffix}"))
    }

    fn params(&self) -> Result<Map<String, Value>, Box<dyn std::error::Error>> {
        let mut params = Map::new();
        insert_string(&mut params, "workspace_root", &self.workspace_root);
        insert_string(&mut params, "agent_id", &self.agent_id);
        insert_string(&mut params, "client_id", &self.client_id);
        params.insert("now".to_string(), json!(now_seconds()));
        insert_string(&mut params, "message", &self.message);
        insert_string(&mut params, "files", &self.files);
        insert_string(&mut params, "symbols", &self.symbols);
        insert_string(&mut params, "aliases", &self.aliases);
        insert_string(&mut params, "id", &self.id);
        insert_string(&mut params, "timeout", &self.timeout);
        insert_string(&mut params, "status", &self.status);
        insert_u64(&mut params, "limit", &self.limit)?;
        insert_u64(&mut params, "after_id", &self.after_id)?;

        if !self.kind.is_empty() {
            params.insert("event_type".to_string(), json!(self.kind));
            params.insert("kind".to_string(), json!(self.kind));
        }

        let mut metadata = BTreeMap::new();
        insert_metadata(&mut metadata, "status", &self.status);
        insert_metadata(&mut metadata, "targets", &self.targets);
        insert_metadata(&mut metadata, "commit", &self.commit);
        if !metadata.is_empty() {
            params.insert("metadata".to_string(), serde_json::to_value(metadata)?);
        }
        Ok(params)
    }
}

fn request_and_print(method: &str, params: Map<String, Value>, start: bool) -> CliResult {
    let result = request(method, params, start)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn request(method: &str, params: Map<String, Value>, start: bool) -> Result<Value, hsp_wire::BrokerWireError> {
    let mut client = hsp::BrokerClient::from_default_path();
    if start {
        client.connect_or_start(Duration::from_millis(250), Duration::from_secs(5))?;
    } else {
        client.connect()?;
    }
    client.request(method, params)
}

fn print_workgroup_probe(path: Option<PathBuf>) {
    let path =
        path.unwrap_or_else(|| std::env::current_dir().expect("current directory is available"));
    let workspace = hsp::HspWorkspace::discover(&path);

    println!("hsp {}", env!("CARGO_PKG_VERSION"));
    println!("root: {}", workspace.root.display());
    println!("python_reference: {}", workspace.py_reference.display());

    if workspace.workgroups.is_empty() {
        println!("workgroups: none");
        return;
    }

    println!("workgroups:");
    for workgroup in workspace.workgroups {
        println!(
            "  {} {} {}",
            workgroup.level.as_str(),
            workgroup.name,
            workgroup.root.display()
        );
    }
}

fn insert_string(params: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        params.insert(key.to_string(), json!(value));
    }
}

fn insert_u64(
    params: &mut Map<String, Value>,
    key: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if value.is_empty() {
        return Ok(());
    }
    params.insert(key.to_string(), json!(value.parse::<u64>()?));
    Ok(())
}

fn insert_metadata(metadata: &mut BTreeMap<String, String>, key: &str, value: &str) {
    if !value.is_empty() {
        metadata.insert(key.to_string(), value.to_string());
    }
}

fn compact_event(event: &Value) -> String {
    let seq = event.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let kind = event
        .get("event_type")
        .or_else(|| event.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let agent = event.get("agent_id").and_then(Value::as_str).unwrap_or("");
    let message = event.get("message").and_then(Value::as_str).unwrap_or("");
    let files = event
        .get("files")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    if files.is_empty() {
        format!("E{seq} {kind} {agent}: {message}")
    } else {
        format!("E{seq} {kind} {agent} [{files}]: {message}")
    }
}

fn hook_message(payload: &str) -> String {
    let raw = payload.trim();
    if raw.is_empty() {
        return String::new();
    }
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        for key in ["message", "prompt", "command", "tool_name"] {
            if let Some(text) = value
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                return text.to_string();
            }
        }
    }
    raw.lines().next().unwrap_or("").chars().take(500).collect()
}

fn default_agent_id() -> String {
    std::env::var("HSP_AGENT_ID")
        .or_else(|_| std::env::var("CODEX_AGENT_ID"))
        .unwrap_or_else(|_| format!("hsp-cli-{}", std::process::id()))
}

fn default_client_id() -> String {
    std::env::var("HSP_CLIENT_ID").unwrap_or_else(|_| "hsp-cli".to_string())
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn parse_duration_seconds(raw: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok(0.0);
    }
    let (number, scale) = if let Some(number) = value.strip_suffix("ms") {
        (number, 0.001)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60.0)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3600.0)
    } else {
        (value.as_str(), 1.0)
    };
    Ok((number.parse::<f64>()? * scale).max(0.0))
}

fn normalize_root(raw: String) -> String {
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .expect("current directory is available")
            .join(path)
    };
    path.to_string_lossy().into_owned()
}

fn err<T>(message: impl Into<String>) -> Result<T, Box<dyn std::error::Error>> {
    Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()).into())
}

fn print_help() {
    println!("hsp {}", env!("CARGO_PKG_VERSION"));
    println!("usage:");
    println!("  hsp [path]");
    println!("  hsp workgroup [path]");
    println!("  hsp broker");
    println!("  hsp mcp");
    println!("  hsp socket");
    println!("  hsp ping|status|shutdown");
    println!("  hsp hook stdin <kind> [options]");
    println!("  hsp log <action> [options]");
    println!("  hsp run [options] -- <command>");
    println!("  hsp watch [path...] [--once] [--global]");
    println!("  hsp global");
}

fn print_log_help() {
    println!("hsp log actions:");
    println!("  event note ask reply chat ticket journal question");
    println!("  recent settle precommit postcommit weather presence status");
    println!("  build_gate edit_gate");
    println!("options:");
    println!("  --message --files --symbols --aliases --id --timeout --kind");
    println!("  --status --targets --commit --workspace-root --agent-id --client-id");
    println!("  --limit --after-id");
}

fn print_watch_help() {
    println!("hsp watch [path...]");
    println!("options:");
    println!("  --once");
    println!("  --global");
    println!("  --limit <n>");
    println!("  --interval <seconds>");
}

fn print_run_help() {
    println!("hsp run [options] -- <command>");
    println!("options:");
    println!("  --timeout <duration>");
    println!("  --kind <event-kind>");
    println!("  --files <scope>");
    println!("  --symbols <scope>");
    println!("  --message <text>");
    println!("  --workspace-root <path>");
    println!("  --agent-id <id>");
    println!("  --no-log");
}

fn print_hook_help() {
    println!("hsp hook stdin <kind> [options]");
    println!("options:");
    println!("  --kind <event-kind>");
    println!("  --message <text>");
    println!("  --files <scope>");
    println!("  --symbols <scope>");
    println!("  --aliases <scope>");
    println!("  --status <status>");
    println!("  --targets <targets>");
    println!("  --commit <sha>");
    println!("  --workspace-root <path>");
    println!("  --agent-id <id>");
    println!("  --client-id <id>");
}

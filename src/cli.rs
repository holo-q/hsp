use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hsp_build::{command_gate_spec, command_gate_spec_from_line};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

type CliResult = Result<(), Box<dyn std::error::Error>>;
const BUILD_BATCH_CAPTURE_LIMIT: usize = 12_000;
const BUILD_BATCH_DEFAULT_TTL_SECONDS: f64 = 30.0;
const BUILD_BATCH_DEFAULT_WAIT_SECONDS: f64 = 1800.0;

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
        "wrap" => wrap_command(&args[1..]),
        "cargo" | "spaceship" => wrapped_alias_command(command, &args[1..]),
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
    if !hooks_enabled() {
        return Ok(());
    }
    let payload_value = serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null);
    if options.message.is_empty() {
        options.message = hook_message(&payload);
    }
    if matches!(options.kind.as_str(), "prompt" | "user.prompt") && options.message.trim() == ".end"
    {
        options.kind = "session.stop".to_string();
        options.message = ".end".to_string();
    }
    let original_hook_kind = options.kind.clone();
    options.kind = normalize_hook_kind(&options.kind);
    let command = hook_command_value(&payload_value);
    options.files = join_scope(&options.files, hook_files(&payload_value));
    options.symbols = join_scope(&options.symbols, hook_symbols(&payload_value));
    if is_edit_before_hook(&options.kind) && require_ticket_for_edits() {
        let gate = edit_gate(&options.workspace_root, &options.agent_id)?;
        if !gate.get("allowed").and_then(Value::as_bool).unwrap_or(false) {
            write_hook_denial(&edit_denial_reason(&gate))?;
            return Ok(());
        }
    }
    if is_build_before_hook(&options.kind, &payload_value, &command) {
        let Some(spec) = command_gate_spec_from_line(&command) else {
            return Ok(());
        };
        let files = if options.files.is_empty() {
            spec.files_csv()
        } else {
            options.files.clone()
        };
        let full_workspace = options.files.is_empty() && spec.full_workspace;
        let gate = wait_for_build_gate_scope(
            &options.workspace_root,
            &options.agent_id,
            hook_build_gate_timeout_seconds(),
            &files,
            &options.symbols,
            full_workspace,
        )?;
        if !gate
            .get("unlocked")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            eprintln!("{}", build_gate_message(&gate));
            std::process::exit(124);
        }
        if gate.get("reason").and_then(Value::as_str) == Some("all_waiting")
            && authoritative_build_enabled()
        {
            let batch = run_authoritative_build_batch(
                &command,
                &gate,
                &options.workspace_root,
                &files,
                full_workspace,
                &spec.tool,
                spec.phase.as_str(),
            )?;
            write_hook_denial(&build_batch_denial_reason(&batch))?;
            return Ok(());
        }
        return Ok(());
    }
    let build_after_spec = if is_build_after_hook(&options.kind, &payload_value, &command) {
        command_gate_spec_from_line(&command)
    } else {
        None
    };
    if let Some(spec) = &build_after_spec {
        options.kind = "test.ran".to_string();
        options.message = command.clone();
        if options.targets.is_empty() {
            options.targets = spec.targets();
        }
        if options.files.is_empty() {
            options.files = spec.files_csv();
        }
        if options.status.is_empty() {
            options.status = build_status(&hook_status_value(&payload_value));
        }
    }
    if let Some(context) = hook_context_notice(&options, &payload_value)? {
        println!("{context}");
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
    if original_hook_kind != options.kind {
        insert_metadata(&mut metadata, "hook_kind", &original_hook_kind);
    }
    if let Some(spec) = &build_after_spec {
        insert_metadata(&mut metadata, "tool", &spec.tool);
        insert_metadata(&mut metadata, "phase", spec.phase.as_str());
        insert_metadata(&mut metadata, "detector", "hsp-build");
    }
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
    execute_run(options, false)
}

fn wrap_command(args: &[OsString]) -> CliResult {
    if args
        .first()
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| matches!(arg, "-h" | "--help" | "help"))
    {
        print_wrap_help();
        return Ok(());
    }
    let options = RunOptions::parse(args)?;
    execute_run(options, true)
}

fn wrapped_alias_command(command: &str, args: &[OsString]) -> CliResult {
    let mut wrapped = vec![OsString::from(command)];
    wrapped.extend_from_slice(args);
    execute_run(RunOptions::parse(&wrapped)?, true)
}

fn execute_run(options: RunOptions, require_build_command: bool) -> CliResult {
    if require_build_command && options.build_tool.is_empty() {
        return err("hsp wrap only runs recognized build/check/test commands");
    }
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
    full_workspace: bool,
    build_tool: String,
    build_phase: String,
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
            full_workspace: true,
            build_tool: String::new(),
            build_phase: String::new(),
        };

        let mut index = 0;
        let mut kind_set = false;
        let mut files_set = false;
        let mut message_set = false;
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
                    kind_set = true;
                    options.kind = args
                        .get(index)
                        .ok_or("--kind requires a value")?
                        .to_string_lossy()
                        .into_owned();
                }
                "--files" => {
                    index += 1;
                    files_set = true;
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
                    message_set = true;
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
        options.apply_build_profile(kind_set, files_set, message_set);
        Ok(options)
    }

    fn apply_build_profile(&mut self, kind_set: bool, files_set: bool, message_set: bool) {
        let argv = self
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let Some(spec) = command_gate_spec(&argv) else {
            self.full_workspace = self.files.is_empty();
            return;
        };
        if !kind_set {
            self.kind = "test.ran".to_string();
        }
        if !files_set {
            self.files = spec.files_csv();
            self.full_workspace = spec.full_workspace;
        } else {
            self.full_workspace = self.files.is_empty();
        }
        if !message_set {
            self.message = spec.targets();
        }
        self.build_tool = spec.tool;
        self.build_phase = spec.phase.as_str().to_string();
    }
}

fn wait_for_build_gate(options: &RunOptions) -> Result<Value, Box<dyn std::error::Error>> {
    wait_for_build_gate_scope(
        &options.workspace_root,
        &options.agent_id,
        options.timeout_seconds,
        &options.files,
        &options.symbols,
        options.full_workspace,
    )
}

fn wait_for_build_gate_scope(
    workspace_root: &str,
    agent_id: &str,
    timeout_seconds: f64,
    files: &str,
    symbols: &str,
    full_workspace: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    loop {
        let mut params = Map::new();
        params.insert("workspace_root".to_string(), json!(workspace_root));
        params.insert("agent_id".to_string(), json!(agent_id));
        params.insert("now".to_string(), json!(now_seconds()));
        params.insert("files".to_string(), json!(files));
        params.insert("symbols".to_string(), json!(symbols));
        params.insert("full_workspace".to_string(), json!(full_workspace));
        let gate = request("bus.build_gate", params, true)?;
        if gate
            .get("unlocked")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || started.elapsed().as_secs_f64() >= timeout_seconds
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
    let mut metadata = BTreeMap::new();
    insert_metadata(
        &mut metadata,
        "status",
        if passed { "passed" } else { "failed" },
    );
    insert_metadata(
        &mut metadata,
        "targets",
        &options
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" "),
    );
    insert_metadata(&mut metadata, "tool", &options.build_tool);
    insert_metadata(&mut metadata, "phase", &options.build_phase);
    if !options.build_tool.is_empty() {
        insert_metadata(&mut metadata, "detector", "hsp-build");
    }
    params.insert("metadata".to_string(), serde_json::to_value(metadata)?);
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

fn hooks_enabled() -> bool {
    let Ok(value) = std::env::var("HSP_HOOKS") else {
        return true;
    };
    !is_false_env_value(&value)
}

fn authoritative_build_enabled() -> bool {
    let value = std::env::var("HSP_AUTHORITATIVE_BUILD").unwrap_or_else(|_| "1".to_string());
    !is_false_env_value(&value)
}

fn require_ticket_for_edits() -> bool {
    let Ok(value) = std::env::var("HSP_REQUIRE_TICKET_FOR_EDITS") else {
        return false;
    };
    is_true_env_value(&value)
}

fn hook_context_enabled() -> bool {
    let value = std::env::var("HSP_HOOK_CONTEXT").unwrap_or_else(|_| "1".to_string());
    !is_false_env_value(&value)
}

fn is_false_env_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no" | "disable" | "disabled"
    )
}

fn is_true_env_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes" | "enable" | "enabled"
    )
}

fn hook_command_value(payload: &Value) -> String {
    string_member(payload, "command")
        .or_else(|| nested_string_member(payload, "tool_input", "command"))
        .or_else(|| nested_string_member(payload, "toolInput", "command"))
        .or_else(|| nested_string_member(payload, "input", "command"))
        .unwrap_or_default()
}

fn hook_tool_name(payload: &Value) -> String {
    string_member(payload, "tool_name")
        .or_else(|| string_member(payload, "toolName"))
        .or_else(|| string_member(payload, "name"))
        .unwrap_or_default()
}

fn hook_files(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_path_like(payload, &mut out);
    for key in ["tool_input", "toolInput", "input"] {
        if let Some(nested) = payload.get(key) {
            collect_path_like(nested, &mut out);
        }
    }
    dedupe(out)
}

fn hook_symbols(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_items(payload.get("symbol"), &mut out);
    collect_items(payload.get("symbols"), &mut out);
    for key in ["tool_input", "toolInput", "input"] {
        if let Some(nested) = payload.get(key) {
            collect_items(nested.get("symbol"), &mut out);
            collect_items(nested.get("symbols"), &mut out);
        }
    }
    dedupe(out)
}

fn collect_path_like(value: &Value, out: &mut Vec<String>) {
    for key in [
        "file_path",
        "filePath",
        "path",
        "notebook_path",
        "notebookPath",
        "files",
        "paths",
    ] {
        collect_items(value.get(key), out);
    }
}

fn collect_items(value: Option<&Value>, out: &mut Vec<String>) {
    match value {
        Some(Value::String(item)) if !item.trim().is_empty() => out.push(item.trim().to_string()),
        Some(Value::Array(items)) => {
            for item in items {
                collect_items(Some(item), out);
            }
        }
        _ => {}
    }
}

fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

fn join_scope(existing: &str, discovered: Vec<String>) -> String {
    let mut items = hsp_wire::BusScope::parse(existing, "", "").files;
    for item in discovered {
        if !items.contains(&item) {
            items.push(item);
        }
    }
    items.join(",")
}

fn hook_status_value(payload: &Value) -> String {
    string_member(payload, "status")
        .or_else(|| nested_string_member(payload, "tool_response", "status"))
        .or_else(|| nested_string_member(payload, "toolResponse", "status"))
        .or_else(|| nested_string_member(payload, "response", "status"))
        .unwrap_or_default()
}

fn build_status(status: &str) -> String {
    match status {
        "success" | "passed" | "ok" => "passed".to_string(),
        "error" | "failed" | "interrupted" => "failed".to_string(),
        value => value.to_string(),
    }
}

fn is_build_before_hook(kind: &str, payload: &Value, command: &str) -> bool {
    matches!(kind, "tool.before" | "bash.before" | "pre_tool")
        && hook_tool_name(payload) == "Bash"
        && command_gate_spec_from_line(command).is_some()
}

fn is_build_after_hook(kind: &str, payload: &Value, command: &str) -> bool {
    matches!(kind, "tool.after" | "bash.after" | "post_tool")
        && hook_tool_name(payload) == "Bash"
        && command_gate_spec_from_line(command).is_some()
}

fn is_edit_before_hook(kind: &str) -> bool {
    matches!(kind, "edit.before" | "write.before")
}

fn edit_gate(workspace_root: &str, agent_id: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let mut params = Map::new();
    params.insert("workspace_root".to_string(), json!(workspace_root));
    params.insert("agent_id".to_string(), json!(agent_id));
    params.insert(
        "mode".to_string(),
        json!(
            std::env::var("HSP_EDIT_GATE_SCOPE")
                .unwrap_or_else(|_| "workgroup".to_string())
        ),
    );
    Ok(request("bus.edit_gate", params, true)?)
}

fn edit_denial_reason(gate: &Value) -> String {
    let reason = gate
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!(
        "Edit denied by HSP workgroup policy: no active ticket is held for this workspace. Start work with hsp.ticket(\"...\") or `hsp log ticket --message \"...\"`, then retry the edit.\n\nedit gate: denied ({reason})"
    )
}

fn hook_context_notice(
    options: &HookOptions,
    payload: &Value,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if !hook_context_enabled()
        || !is_context_hook(&options.kind, payload)
        || (options.files.is_empty() && options.symbols.is_empty())
    {
        return Ok(None);
    }
    let mut params = Map::new();
    params.insert("workspace_root".to_string(), json!(options.workspace_root));
    params.insert("agent_id".to_string(), json!(options.agent_id));
    params.insert("client_id".to_string(), json!(options.client_id));
    params.insert("now".to_string(), json!(now_seconds()));
    params.insert("limit".to_string(), json!(10));
    insert_string(&mut params, "files", &options.files);
    insert_string(&mut params, "symbols", &options.symbols);
    let recent = match request("bus.recent", params, true) {
        Ok(recent) => recent,
        Err(error) => return Ok(Some(format!("hsp context unavailable: {error}"))),
    };
    let body = render_hook_context(&recent);
    if body.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "hsp context for {}:\n{body}",
        hook_context_target(&options.files, &options.symbols)
    )))
}

fn is_context_hook(kind: &str, payload: &Value) -> bool {
    let tool = hook_tool_name(payload);
    if matches!(kind, "read.before" | "read.after") {
        return true;
    }
    if matches!(kind, "edit.before" | "edit.after") {
        return tool.is_empty()
            || matches!(
                tool.as_str(),
                "Edit" | "MultiEdit" | "Write" | "NotebookEdit"
            );
    }
    matches!(kind, "tool.before" | "tool.after" | "pre_tool" | "post_tool")
        && matches!(tool.as_str(), "Read" | "NotebookRead")
}

fn normalize_hook_kind(kind: &str) -> String {
    match kind {
        "session.end" => "session.stop".to_string(),
        "compact.after" | "permission.request" | "stop.failure" | "subagent.start" => {
            "babel.event".to_string()
        }
        value if hsp_wire::BusEventKind::from_wire(value).is_ok() => value.to_string(),
        _ => "babel.event".to_string(),
    }
}

fn hook_context_target(files: &str, symbols: &str) -> String {
    let scope = hsp_wire::BusScope::parse(files, symbols, "");
    let mut items = scope.files;
    for symbol in scope.symbols {
        if !items.contains(&symbol) {
            items.push(symbol);
        }
    }
    if items.is_empty() {
        "scope".to_string()
    } else {
        items.join(", ")
    }
}

fn render_hook_context(recent: &Value) -> String {
    let mut lines = Vec::new();
    if let Some(tickets) = recent.get("active_tickets").and_then(Value::as_array) {
        for ticket in tickets {
            if let Some(line) = compact_ticket(ticket) {
                lines.push(line);
            }
        }
    }
    if let Some(questions) = recent.get("open_questions").and_then(Value::as_array) {
        for question in questions {
            if let Some(line) = compact_question(question) {
                lines.push(line);
            }
        }
    }
    if let Some(events) = recent.get("events").and_then(Value::as_array) {
        for event in events {
            lines.push(compact_event(event));
        }
    }
    if recent
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.push("... truncated".to_string());
    }
    lines.join("\n")
}

fn compact_ticket(ticket: &Value) -> Option<String> {
    let id = ticket.get("ticket_id").and_then(Value::as_str)?;
    let message = ticket.get("message").and_then(Value::as_str).unwrap_or("");
    let holders = ticket
        .get("holders")
        .and_then(Value::as_object)
        .map(|holders| holders.keys().cloned().collect::<Vec<_>>().join(","))
        .unwrap_or_default();
    Some(format!("{id} {message} [{holders}]"))
}

fn compact_question(question: &Value) -> Option<String> {
    let id = question
        .get("question_id")
        .or_else(|| question.get("id"))
        .and_then(Value::as_str)?;
    let message = question.get("message").and_then(Value::as_str).unwrap_or("");
    Some(format!("{id} {message}"))
}

fn string_member(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn nested_string_member(value: &Value, outer: &str, inner: &str) -> Option<String> {
    value.get(outer).and_then(|nested| string_member(nested, inner))
}

fn hook_build_gate_timeout_seconds() -> f64 {
    std::env::var("HSP_BUILD_GATE_TIMEOUT")
        .ok()
        .and_then(|value| parse_duration_seconds(&value).ok())
        .unwrap_or(120.0)
}

#[derive(Debug, Clone)]
struct BuildBatchResult {
    command: String,
    key: String,
    owner: bool,
    returncode: i32,
    status: String,
    stdout: String,
    stderr: String,
    timestamp: f64,
}

impl BuildBatchResult {
    fn to_json(&self) -> Value {
        json!({
            "command": self.command,
            "key": self.key,
            "owner": self.owner,
            "returncode": self.returncode,
            "status": self.status,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "timestamp": self.timestamp,
        })
    }

    fn from_json(value: Value) -> Option<Self> {
        let object = value.as_object()?;
        Some(Self {
            command: object.get("command")?.as_str()?.to_string(),
            key: object.get("key")?.as_str()?.to_string(),
            owner: object.get("owner").and_then(Value::as_bool).unwrap_or(false),
            returncode: object
                .get("returncode")
                .and_then(Value::as_i64)
                .unwrap_or(1) as i32,
            status: object
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("failed")
                .to_string(),
            stdout: object
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            stderr: object
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            timestamp: object
                .get("timestamp")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        })
    }

    fn with_owner(mut self, owner: bool) -> Self {
        self.owner = owner;
        self
    }
}

fn run_authoritative_build_batch(
    command: &str,
    gate: &Value,
    workspace_root: &str,
    files: &str,
    full_workspace: bool,
    tool: &str,
    phase: &str,
) -> Result<BuildBatchResult, Box<dyn std::error::Error>> {
    let root = PathBuf::from(workspace_root);
    let directory = root.join("tmp").join("hsp-build-batches");
    fs::create_dir_all(&directory)?;
    let gate_text = serde_json::to_string(gate)?;
    let key = build_batch_key(&root, command, &gate_text);
    let result_path = directory.join(format!("{key}.json"));
    let lock_path = directory.join(format!("{key}.lock"));
    let ttl = duration_env("HSP_BUILD_BATCH_TTL", BUILD_BATCH_DEFAULT_TTL_SECONDS);
    let wait_timeout = duration_env(
        "HSP_BUILD_BATCH_WAIT_TIMEOUT",
        BUILD_BATCH_DEFAULT_WAIT_SECONDS,
    );

    if let Some(result) = read_fresh_batch_result(&result_path, ttl) {
        return Ok(result.with_owner(false));
    }
    if try_create_lock(&lock_path, ttl)? {
        let result = run_build_command(command, &root, &key)?.with_owner(true);
        write_batch_result(&result_path, &result)?;
        record_authoritative_build_result(
            command,
            &result,
            workspace_root,
            files,
            full_workspace,
            tool,
            phase,
        )?;
        let _ = fs::remove_file(lock_path);
        return Ok(result);
    }
    if let Some(result) = wait_for_batch_result(&result_path, wait_timeout) {
        return Ok(result.with_owner(false));
    }
    Ok(BuildBatchResult {
        command: command.to_string(),
        key,
        owner: false,
        returncode: 124,
        status: "failed".to_string(),
        stdout: String::new(),
        stderr: format!("timed out waiting for HSP build batch result after {wait_timeout:.0}s"),
        timestamp: now_seconds(),
    })
}

fn build_batch_key(root: &Path, command: &str, gate: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    hasher.update(b"\n");
    hasher.update(command.as_bytes());
    hasher.update(b"\n");
    hasher.update(gate.as_bytes());
    let digest = hasher.finalize();
    format!("{digest:x}").chars().take(24).collect()
}

fn run_build_command(
    command: &str,
    root: &Path,
    key: &str,
) -> Result<BuildBatchResult, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .output()?;
    let returncode = output.status.code().unwrap_or(1);
    Ok(BuildBatchResult {
        command: command.to_string(),
        key: key.to_string(),
        owner: false,
        returncode,
        status: if returncode == 0 { "passed" } else { "failed" }.to_string(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        timestamp: now_seconds(),
    })
}

fn record_authoritative_build_result(
    command: &str,
    result: &BuildBatchResult,
    workspace_root: &str,
    files: &str,
    full_workspace: bool,
    tool: &str,
    phase: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut params = Map::new();
    params.insert("workspace_root".to_string(), json!(workspace_root));
    params.insert("agent_id".to_string(), json!(default_agent_id()));
    params.insert("client_id".to_string(), json!(default_client_id()));
    params.insert("now".to_string(), json!(now_seconds()));
    params.insert("message".to_string(), json!(command));
    if !full_workspace && !files.is_empty() {
        params.insert("files".to_string(), json!(files));
    }
    params.insert("event_type".to_string(), json!("test.ran"));
    params.insert("kind".to_string(), json!("test.ran"));
    params.insert(
        "metadata".to_string(),
        json!({
            "status": result.status,
            "targets": command,
            "tool": tool,
            "phase": phase,
            "detector": "hsp-build",
            "batch_key": result.key,
            "batch_owner": result.owner,
        }),
    );
    request("bus.event", params, true)?;
    Ok(())
}

fn build_batch_denial_reason(result: &BuildBatchResult) -> String {
    let action = if result.owner {
        "ran this command once"
    } else {
        "reused the batched result"
    };
    let mut lines = vec![
        format!("HSP build mutex {action} for the project and denied duplicate Bash execution."),
        format!("$ {}", result.command),
        format!("exit: {}", result.returncode),
    ];
    let stdout = truncate_capture(&result.stdout);
    if !stdout.is_empty() {
        lines.push("--- stdout ---".to_string());
        lines.push(stdout);
    }
    let stderr = truncate_capture(&result.stderr);
    if !stderr.is_empty() {
        lines.push("--- stderr ---".to_string());
        lines.push(stderr);
    }
    lines.join("\n").trim().to_string()
}

fn truncate_capture(text: &str) -> String {
    if text.len() <= BUILD_BATCH_CAPTURE_LIMIT {
        return text.trim_end().to_string();
    }
    let mut end = 0;
    for (index, _) in text.char_indices() {
        if index <= BUILD_BATCH_CAPTURE_LIMIT {
            end = index;
        } else {
            break;
        }
    }
    let head = text[..end].trim_end();
    format!(
        "{head}\n... truncated {} char(s)",
        text.len().saturating_sub(end)
    )
}

fn read_fresh_batch_result(path: &Path, ttl: f64) -> Option<BuildBatchResult> {
    let modified = path.metadata().ok()?.modified().ok()?;
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .ok()?
        .as_secs_f64();
    if age > ttl {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    BuildBatchResult::from_json(value)
}

fn wait_for_batch_result(path: &Path, timeout: f64) -> Option<BuildBatchResult> {
    let started = std::time::Instant::now();
    while started.elapsed().as_secs_f64() <= timeout {
        if let Some(result) =
            read_fresh_batch_result(path, timeout.max(BUILD_BATCH_DEFAULT_TTL_SECONDS))
        {
            return Some(result);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

fn write_batch_result(
    path: &Path,
    result: &BuildBatchResult,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(path, serde_json::to_string(&result.to_json())?)?;
    Ok(())
}

fn try_create_lock(path: &Path, ttl: f64) -> Result<bool, Box<dyn std::error::Error>> {
    if let Ok(metadata) = path.metadata()
        && let Ok(modified) = metadata.modified()
        && let Ok(age) = std::time::SystemTime::now().duration_since(modified)
        && age.as_secs_f64() > ttl
    {
        let _ = fs::remove_file(path);
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            writeln!(file, "{}", std::process::id())?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn duration_env(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| parse_duration_seconds(&value).ok())
        .unwrap_or(default)
}

fn build_gate_message(gate: &Value) -> String {
    let reason = gate
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut text = format!("build gate locked: {reason}");
    if let Some(scope) = gate.get("scope").and_then(Value::as_object)
        && let Some(files) = scope.get("files").and_then(Value::as_array)
        && !files.is_empty()
    {
        let joined = files
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(",");
        if !joined.is_empty() {
            text.push_str(&format!("\nscope: {joined}"));
        }
    }
    text
}

fn write_hook_denial(reason: &str) -> Result<(), Box<dyn std::error::Error>> {
    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    });
    print!("{}", serde_json::to_string(&output)?);
    Ok(())
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
    println!("  hsp wrap [options] -- <build-command>");
    println!("  hsp cargo|spaceship <args...>");
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

fn print_wrap_help() {
    println!("hsp wrap [options] -- <build-command>");
    println!("hsp cargo|spaceship <args...>");
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

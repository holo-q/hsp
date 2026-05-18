use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Map, Value, json};

type McpResult<T> = Result<T, Box<dyn std::error::Error>>;

const BUS_ACTIONS: &[&str] = &[
    "event",
    "note",
    "ask",
    "reply",
    "chat",
    "ticket",
    "journal",
    "question",
    "edit_gate",
    "build_gate",
    "recent",
    "recent_all",
    "recent_tree",
    "settle",
    "precommit",
    "postcommit",
    "weather",
    "presence",
    "workgroup",
    "status",
];

pub fn run() -> McpResult<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let message = match serde_json::from_str::<Value>(&line) {
            Ok(message) => message,
            Err(error) => {
                write_response(
                    &mut stdout,
                    json_rpc_error(Value::Null, -32700, format!("parse error: {error}")),
                )?;
                continue;
            }
        };
        let Some(object) = message.as_object() else {
            write_response(
                &mut stdout,
                json_rpc_error(Value::Null, -32600, "request must be a JSON object"),
            )?;
            continue;
        };
        let id = object.get("id").cloned();
        let method = object.get("method").and_then(Value::as_str).unwrap_or("");

        let response = match method {
            "initialize" => id.map(|id| json_rpc_result(id, initialize_result(object))),
            "notifications/initialized" | "notifications/cancelled" => None,
            "ping" => id.map(|id| json_rpc_result(id, json!({}))),
            "tools/list" => id.map(|id| json_rpc_result(id, tools_list())),
            "tools/call" => id.map(|id| match call_tool(object) {
                Ok(result) => json_rpc_result(id, result),
                Err(error) => json_rpc_result(id, tool_error_text(error.to_string())),
            }),
            "" => id.map(|id| json_rpc_error(id, -32600, "request missing method")),
            other => id.map(|id| json_rpc_error(id, -32601, format!("unknown method: {other}"))),
        };

        if let Some(response) = response {
            write_response(&mut stdout, response)?;
        }
    }

    Ok(())
}

fn initialize_result(request: &Map<String, Value>) -> Value {
    let protocol_version = request
        .get("params")
        .and_then(Value::as_object)
        .and_then(|params| params.get("protocolVersion"))
        .cloned()
        .unwrap_or_else(|| json!("2024-11-05"));
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {
                "listChanged": false,
            },
        },
        "serverInfo": {
            "name": "hsp",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "Rust HSP broker/workgroup tools. Ticket titles must be lowercase hyphen-separated slugs prefixed with fix, feat, docs, refactor, test, chore, perf, build, ci, style, revert, review, debug, ops, or release, for example feat-ticket-title. LSP tools are still routed by the Python reference until the Rust runtime wave lands.",
    })
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "lsp_log",
                "description": "Record and inspect agent-bus coordination events.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "action": {"type": "string", "default": "weather"},
                        "message": {"type": "string", "default": ""},
                        "files": {"type": "string", "default": ""},
                        "symbols": {"type": "string", "default": ""},
                        "aliases": {"type": "string", "default": ""},
                        "id": {"type": "string", "default": ""},
                        "timeout": {"type": "string", "default": "3m"},
                        "kind": {"type": "string", "default": ""},
                        "status": {"type": "string", "default": ""},
                        "targets": {"type": "string", "default": ""},
                        "commit": {"type": "string", "default": ""},
                        "workspace_root": {"type": "string"}
                    },
                },
            },
            {
                "name": "ticket",
                "description": "Acquire or release this agent's current work ticket. Starting or joining a ticket requires a scan-friendly title: prefix with fix/feat/docs/refactor/test/chore/perf/build/ci/style/revert/review/debug/ops/release and separate words with hyphens, for example feat-ticket-title. Pass an empty title to release.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["title"],
                    "properties": {
                        "title": {"type": "string", "default": ""},
                        "message": {"type": "string", "default": ""},
                        "files": {"type": "string", "default": ""},
                        "symbols": {"type": "string", "default": ""},
                        "workspace_root": {"type": "string"}
                    },
                },
            },
            {
                "name": "journal",
                "description": "Show compact workgroup journal, tickets, and questions.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "limit": {"type": "integer", "default": 25, "minimum": 1},
                        "workspace_root": {"type": "string"}
                    },
                },
            },
            {
                "name": "ask",
                "description": "Open a workgroup question and wait briefly for replies.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "message": {"type": "string"},
                        "files": {"type": "string", "default": ""},
                        "symbols": {"type": "string", "default": ""},
                        "timeout": {"type": "string", "default": "2m"},
                        "workspace_root": {"type": "string"}
                    },
                    "required": ["message"],
                },
            },
            {
                "name": "chat",
                "description": "Post a chat row, optionally replying to an open question id.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "message": {"type": "string"},
                        "id": {"type": "string", "default": ""},
                        "workspace_root": {"type": "string"}
                    },
                    "required": ["message"],
                },
            },
            {
                "name": "lsp_memory",
                "description": "Inspect and manage render-memory aliases.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "action": {"type": "string", "default": "status"},
                        "target": {"type": "string", "default": ""},
                        "mode": {"type": "string", "default": ""}
                    },
                },
            },
            {
                "name": "lsp_session",
                "description": "Inspect the broker-owned LSP runtime.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "action": {"type": "string", "default": "status"},
                        "path": {"type": "string", "default": ""},
                        "server": {"type": "string", "default": ""}
                    },
                },
            },
            {
                "name": "lsp_outline",
                "description": "Show compact document symbols for one source file.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "file_path": {"type": "string", "default": ""},
                        "pattern": {"type": "string", "default": ""}
                    },
                },
            }
        ],
    })
}

fn call_tool(request: &Map<String, Value>) -> McpResult<Value> {
    let params = request
        .get("params")
        .and_then(Value::as_object)
        .ok_or("tools/call requires object params")?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call requires params.name")?;
    let arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    match name {
        "lsp_log" | "log" => call_lsp_log(&arguments),
        "ticket" => call_ticket(&arguments),
        "journal" => call_journal(&arguments),
        "ask" => call_ask(&arguments),
        "chat" => call_chat(&arguments),
        "lsp_memory" | "memory" => call_lsp_memory(&arguments),
        "lsp_session" | "session" => call_lsp_session(&arguments),
        "lsp_outline" | "outline" => call_lsp_outline(&arguments),
        other => Ok(tool_error_text(format!("unknown HSP tool: {other}"))),
    }
}

fn call_lsp_log(arguments: &Map<String, Value>) -> McpResult<Value> {
    let action = arg_string(arguments, "action")
        .filter(|action| !action.is_empty())
        .unwrap_or_else(|| "weather".to_string())
        .to_ascii_lowercase();
    if !BUS_ACTIONS.contains(&action.as_str()) {
        return Ok(tool_error_text(format!(
            "Unknown action: {action:?}. Valid: {}.",
            BUS_ACTIONS.join(", ")
        )));
    }
    if action == "ask" && arg_text(arguments, "message").trim().is_empty() {
        return Ok(tool_error_text("action=\"ask\" requires message=\"...\""));
    }
    if action == "reply" && arg_text(arguments, "id").trim().is_empty() {
        return Ok(tool_error_text("action=\"reply\" requires id=\"Q<n>\""));
    }

    let method_action = broker_action(&action);
    let mut params = bus_params(arguments, method_action)?;
    if let Some(kind) = arg_string(arguments, "kind").filter(|kind| !kind.is_empty()) {
        if method_action == "event" {
            params.insert("event_type".to_string(), json!(kind));
            params.insert(
                "kind".to_string(),
                params.get("event_type").cloned().unwrap_or(Value::Null),
            );
        } else {
            add_metadata(&mut params, "kind", kind);
        }
    }
    let result = broker_request(&format!("bus.{method_action}"), params)?;
    Ok(tool_text(
        &format!("hsp/{method_action}"),
        render_bus_result(method_action, &result),
        false,
    ))
}

fn call_ticket(arguments: &Map<String, Value>) -> McpResult<Value> {
    let params = bus_params(arguments, "ticket")?;
    let result = broker_request("bus.ticket", params)?;
    Ok(tool_text(
        "hsp/ticket",
        render_bus_result("ticket", &result),
        false,
    ))
}

fn call_journal(arguments: &Map<String, Value>) -> McpResult<Value> {
    let mut params = bus_params(arguments, "journal")?;
    params.insert("limit".to_string(), json!(arg_u64(arguments, "limit", 25)));
    let result = broker_request("bus.journal", params)?;
    Ok(tool_text(
        "hsp/journal",
        render_bus_result("journal", &result),
        false,
    ))
}

fn call_ask(arguments: &Map<String, Value>) -> McpResult<Value> {
    if arg_text(arguments, "message").trim().is_empty() {
        return Ok(tool_error_text("ask requires message=\"...\""));
    }

    let mut params = bus_params(arguments, "ask")?;
    let timeout = arg_string(arguments, "timeout").unwrap_or_else(|| "2m".to_string());
    params.insert("timeout".to_string(), json!(timeout));
    let opened = broker_request("bus.ask", params.clone())?;
    let Some(qid) = opened
        .get("question")
        .and_then(Value::as_object)
        .and_then(|question| question.get("question_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(tool_text(
            "hsp/ask",
            render_bus_result("ask", &opened),
            false,
        ));
    };
    if opened
        .get("no_repliers")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let journal = journal_after(arguments, &params)?;
        let mut text = render_bus_result("ask", &opened);
        text.push('\n');
        text.push_str(&render_bus_result("journal", &journal));
        return Ok(tool_text("hsp/ask", text, false));
    }

    let timeout_seconds = parse_duration_seconds(&timeout, 120.0);
    let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_seconds);
    let delay = Duration::from_secs_f64((timeout_seconds / 20.0).clamp(0.01, 0.25));
    while std::time::Instant::now() < deadline {
        std::thread::sleep(delay);
        let mut status_params = params.clone();
        status_params.insert("id".to_string(), json!(qid));
        let status = broker_request("bus.question", status_params)?;
        let replied = status
            .get("replies")
            .and_then(Value::as_array)
            .is_some_and(|replies| !replies.is_empty());
        let closed = status
            .get("question")
            .and_then(Value::as_object)
            .and_then(|question| question.get("closed_at"))
            .is_some_and(|value| !value.is_null() && value != "");
        if replied || closed {
            let mut lines = vec![format!("ask {qid} answered")];
            for reply in status
                .get("replies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                lines.push(format!("  {}", event_label(reply)));
            }
            let journal = journal_after(arguments, &params)?;
            lines.push(render_bus_result("journal", &journal));
            return Ok(tool_text("hsp/ask", lines.join("\n"), false));
        }
    }

    let journal = journal_after(arguments, &params)?;
    Ok(tool_text(
        "hsp/ask",
        format!(
            "ask {qid} timed out after {timeout}\n{}",
            render_bus_result("journal", &journal)
        ),
        false,
    ))
}

fn call_chat(arguments: &Map<String, Value>) -> McpResult<Value> {
    if arg_text(arguments, "message").trim().is_empty() {
        return Ok(tool_error_text("chat requires message=\"...\""));
    }
    let params = bus_params(arguments, "chat")?;
    let result = broker_request("bus.chat", params)?;
    Ok(tool_text(
        "hsp/chat",
        render_bus_result("chat", &result),
        false,
    ))
}

fn call_lsp_memory(arguments: &Map<String, Value>) -> McpResult<Value> {
    let action = arg_string(arguments, "action")
        .filter(|action| !action.is_empty())
        .unwrap_or_else(|| "status".to_string())
        .to_ascii_lowercase();
    let text = match action.as_str() {
        "status" => render_memory_status(&broker_request("render.status", Map::new())?),
        "legend" => {
            let status = broker_request("render.status", Map::new())?;
            status
                .get("legend")
                .and_then(Value::as_str)
                .filter(|legend| !legend.is_empty())
                .unwrap_or("legend: (empty)")
                .to_string()
        }
        "lookup" | "recall" => {
            let target = arg_text(arguments, "target").trim();
            if target.is_empty() {
                return Ok(tool_error_text(format!(
                    "action=\"{action}\" requires target=\"A1\""
                )));
            }
            let mut params = Map::new();
            params.insert("token".to_string(), json!(target));
            render_memory_lookup(&broker_request("render.lookup", params)?)
        }
        "reset" => render_memory_status(&broker_request("render.reset_session", Map::new())?),
        other => {
            return Ok(tool_error_text(format!(
                "Unknown memory action: {other:?}. Valid: status, legend, lookup, recall, reset."
            )));
        }
    };
    Ok(tool_text("hsp/memory", text, false))
}

fn call_lsp_session(arguments: &Map<String, Value>) -> McpResult<Value> {
    let action = arg_string(arguments, "action")
        .filter(|action| !action.is_empty())
        .unwrap_or_else(|| "status".to_string())
        .to_ascii_lowercase();
    if action != "status" {
        return Ok(tool_error_text(format!(
            "Rust lsp_session currently supports action=\"status\"; got {action:?}."
        )));
    }
    Ok(tool_text(
        "hsp/session",
        render_lsp_status(&broker_request("lsp.status", Map::new())?),
        false,
    ))
}

fn call_lsp_outline(arguments: &Map<String, Value>) -> McpResult<Value> {
    let file_path = arg_string(arguments, "file_path")
        .filter(|path| !path.is_empty())
        .or_else(|| arg_string(arguments, "path").filter(|path| !path.is_empty()))
        .ok_or_else(|| mcp_error("lsp_outline requires file_path"))?;
    let path = std::path::PathBuf::from(&file_path);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let uri = hsp::file_uri(&path)?;
    let mut params = Map::new();
    params.insert("root".to_string(), json!(workspace_root(arguments)?));
    params.insert(
        "lsp_method".to_string(),
        json!("textDocument/documentSymbol"),
    );
    params.insert(
        "lsp_params".to_string(),
        json!({
            "textDocument": {
                "uri": uri,
            },
        }),
    );
    params.insert(
        "empty_fallback_methods".to_string(),
        json!(["textDocument/documentSymbol"]),
    );
    let result = broker_request("lsp.request", params)?;
    Ok(tool_text(
        "textDocument/documentSymbol",
        render_outline_result(&path, &result),
        false,
    ))
}

fn journal_after(arguments: &Map<String, Value>, params: &Map<String, Value>) -> McpResult<Value> {
    let mut journal_params = params.clone();
    journal_params.insert("limit".to_string(), json!(arg_u64(arguments, "limit", 25)));
    Ok(broker_request("bus.journal", journal_params)?)
}

fn render_lsp_status(result: &Value) -> String {
    if !result
        .get("configured")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return format!("lsp: unconfigured\n{}", value_str(result, "error"));
    }
    if let Some(runtime) = result.get("runtime").filter(|value| !value.is_null()) {
        let chain = value_array(runtime, "chain");
        let clients = value_array(runtime, "clients");
        let mut lines = vec![format!(
            "lsp runtime root={} requests={}",
            value_str(runtime, "root"),
            runtime
                .get("request_count")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        )];
        for client in clients {
            lines.push(format!(
                "  {} started={}",
                value_str(client, "server_label"),
                client
                    .get("started")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ));
        }
        if clients.is_empty() {
            for server in chain {
                lines.push(format!("  {}", value_str(server, "label")));
            }
        }
        return lines.join("\n");
    }
    let mut lines = vec!["lsp: configured".to_string()];
    for server in value_array(result, "chain") {
        lines.push(format!(
            "  {} {}",
            value_str(server, "command"),
            string_list(server, "args").join(" ")
        ));
    }
    lines.join("\n")
}

fn render_outline_result(path: &std::path::Path, result: &Value) -> String {
    let server = value_str(result, "server_label");
    let symbols = result
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if symbols.is_empty() {
        return format!("{}: no document symbols ({server})", path.display());
    }
    let mut lines = vec![format!("{} ({server})", path.display())];
    render_document_symbols(&symbols, 0, &mut lines);
    lines.join("\n")
}

fn render_document_symbols(symbols: &[Value], depth: usize, lines: &mut Vec<String>) {
    for symbol in symbols {
        let name = value_str(symbol, "name");
        let kind = symbol.get("kind").and_then(Value::as_u64).unwrap_or(0);
        let line = lsp_symbol_line(symbol).unwrap_or(0);
        lines.push(format!(
            "L{line}  {}{} {}",
            "  ".repeat(depth),
            lsp_symbol_kind_label(kind),
            name
        ));
        if let Some(children) = symbol.get("children").and_then(Value::as_array) {
            render_document_symbols(children, depth + 1, lines);
        }
    }
}

fn lsp_symbol_line(symbol: &Value) -> Option<u64> {
    symbol
        .get("range")
        .or_else(|| {
            symbol
                .get("location")
                .and_then(Value::as_object)
                .and_then(|location| location.get("range"))
        })
        .and_then(Value::as_object)
        .and_then(|range| range.get("start"))
        .and_then(Value::as_object)
        .and_then(|start| start.get("line"))
        .and_then(Value::as_u64)
        .map(|line| line + 1)
}

fn lsp_symbol_kind_label(kind: u64) -> &'static str {
    match kind {
        1 => "File ",
        2 => "Module ",
        3 => "Namespace ",
        4 => "Package ",
        5 => "Class ",
        6 => "Method ",
        7 => "Property ",
        8 => "Field ",
        9 => "Constructor ",
        10 => "Enum ",
        11 => "Interface ",
        12 => "Function ",
        13 => "Variable ",
        14 => "Constant ",
        15 => "String ",
        16 => "Number ",
        17 => "Boolean ",
        18 => "Array ",
        19 => "Object ",
        20 => "Key ",
        21 => "Null ",
        22 => "EnumMember ",
        23 => "Struct ",
        24 => "Event ",
        25 => "Operator ",
        26 => "TypeParameter ",
        _ => "Symbol ",
    }
}

fn render_memory_status(result: &Value) -> String {
    let status = result.get("status").unwrap_or(result);
    let epoch = status.get("epoch_id").and_then(Value::as_u64).unwrap_or(0);
    let generation = status
        .get("generation")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let aliases = value_array(status, "aliases");
    let mut lines = vec![format!(
        "render memory epoch={epoch} gen={generation} aliases={}",
        aliases.len()
    )];
    for alias in aliases.iter().take(10) {
        lines.push(format!(
            "  {} {} {}@L{}",
            value_str(alias, "alias"),
            value_str(alias, "kind"),
            alias
                .get("identity")
                .map(|identity| value_str(identity, "name"))
                .unwrap_or_default(),
            alias
                .get("identity")
                .map(|identity| value_str(identity, "line"))
                .unwrap_or_default()
        ));
    }
    if let Some(legend) = status
        .get("legend")
        .and_then(Value::as_str)
        .filter(|legend| !legend.is_empty())
    {
        lines.push(legend.to_string());
    }
    lines.join("\n")
}

fn render_memory_lookup(result: &Value) -> String {
    if result.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let record = result.get("record").unwrap_or(&Value::Null);
        let identity = record.get("identity").unwrap_or(&Value::Null);
        return format!(
            "{} -> {} {}@L{} {}",
            value_str(record, "alias"),
            value_str(record, "kind"),
            value_str(identity, "name"),
            value_str(identity, "line"),
            value_str(identity, "path"),
        );
    }
    format!(
        "{}: {}",
        value_str(result, "error"),
        value_str(result, "message")
    )
}

fn broker_action(action: &str) -> &str {
    match action {
        "workgroup" => "presence",
        other => other,
    }
}

fn bus_params(arguments: &Map<String, Value>, action: &str) -> McpResult<Map<String, Value>> {
    let mut params = Map::new();
    params.insert("workspace_root".to_string(), json!(workspace_root(arguments)?));
    params.insert("agent_id".to_string(), json!(default_agent_id()));
    params.insert("client_id".to_string(), json!(default_client_id()));
    params.insert("session_id".to_string(), json!(default_session_id()));
    params.insert("now".to_string(), json!(now_seconds()));
    insert_argument(&mut params, arguments, "title");
    insert_argument(&mut params, arguments, "message");
    insert_argument(&mut params, arguments, "files");
    insert_argument(&mut params, arguments, "symbols");
    insert_argument(&mut params, arguments, "aliases");
    if let Some(id) = arg_string(arguments, "id").filter(|id| !id.is_empty()) {
        params.insert("id".to_string(), json!(id));
    }
    if let Some(timeout) = arg_string(arguments, "timeout").filter(|timeout| !timeout.is_empty()) {
        params.insert("timeout".to_string(), json!(timeout));
    }
    if action == "edit_gate" {
        if let Some(status) = arg_string(arguments, "status").filter(|status| !status.is_empty()) {
            params.insert("mode".to_string(), json!(status));
        }
    }

    let mut metadata = BTreeMap::new();
    insert_metadata(&mut metadata, arguments, "status");
    insert_metadata(&mut metadata, arguments, "targets");
    insert_metadata(&mut metadata, arguments, "commit");
    if !metadata.is_empty() {
        params.insert("metadata".to_string(), serde_json::to_value(metadata)?);
    }
    Ok(params)
}

fn broker_request(method: &str, params: Map<String, Value>) -> McpResult<Value> {
    let mut client = hsp::BrokerClient::from_default_path();
    client
        .connect_or_start(Duration::from_millis(250), Duration::from_secs(5))
        .map_err(|error| mcp_error(format!("broker start/connect failed: {error}")))?;
    client
        .request(method, params)
        .map_err(|error| mcp_error(format!("broker {method} failed: {error}")).into())
}

fn render_bus_result(action: &str, result: &Value) -> String {
    match action {
        "status" => format!(
            "bus events={} last=E{} open_questions={}",
            result.get("event_count").and_then(Value::as_u64).unwrap_or(0),
            result.get("last_event_id").and_then(Value::as_u64).unwrap_or(0),
            result
                .get("open_question_count")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
        "weather" => render_weather(result),
        "presence" | "workgroup" => render_presence(result),
        "ticket" => render_ticket(result),
        "journal" => render_journal(result),
        "chat" => render_chat(result),
        "recent" | "recent_all" | "recent_tree" => render_recent(result),
        "build_gate" => render_build_gate(result),
        "edit_gate" => render_edit_gate(result),
        "settle" => render_settle(result),
        "precommit" => render_precommit(result),
        "event" | "note" | "postcommit" => result
            .get("event")
            .map(|event| format!("logged {}", event_label(event)))
            .unwrap_or_else(|| "logged event".to_string()),
        "ask" => render_ask(result),
        "reply" => {
            let qid = result
                .get("question")
                .and_then(Value::as_object)
                .and_then(|question| question.get("question_id"))
                .and_then(Value::as_str)
                .unwrap_or("");
            format!(
                "reply recorded for {qid}: {}",
                event_label(result.get("event").unwrap_or(&Value::Null))
            )
        }
        "question" => serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string()),
        _ => serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string()),
    }
}

fn render_ticket(result: &Value) -> String {
    let mut lines = Vec::new();
    let released = value_array(result, "released");
    if !released.is_empty() {
        lines.push("ticket released:".to_string());
        for event in released {
            lines.push(format!("  {}", event_label(event)));
        }
    } else if let Some(ticket) = result.get("ticket") {
        lines.push(format!(
            "ticket {}: {}",
            value_str(ticket, "ticket_id"),
            value_str(ticket, "message")
        ));
        let holders = value_array(ticket, "holders")
            .iter()
            .map(|holder| value_str(holder, "agent_id"))
            .filter(|holder| !holder.is_empty())
            .collect::<Vec<_>>();
        if !holders.is_empty() {
            lines.push(format!("holders: {}", holders.join(", ")));
        }
    } else {
        lines.push("ticket: none".to_string());
    }
    let active = value_array(result, "active_tickets");
    lines.push(format!("active tickets: {}", active.len()));
    for ticket in active.iter().take(5) {
        lines.push(compact_line(&format!(
            "  {} {} [{}]",
            value_str(ticket, "ticket_id"),
            value_str(ticket, "message"),
            value_array(ticket, "holders")
                .iter()
                .map(|holder| value_str(holder, "agent_id"))
                .collect::<Vec<_>>()
                .join(",")
        )));
    }
    lines.join("\n")
}

fn render_journal(result: &Value) -> String {
    let mut lines = Vec::new();
    let tickets = value_array(result, "active_tickets");
    if !tickets.is_empty() {
        lines.push(format!("tickets: {}", tickets.len()));
        for ticket in tickets.iter().take(5) {
            lines.push(compact_line(&format!(
                "  {} {}",
                value_str(ticket, "ticket_id"),
                value_str(ticket, "message")
            )));
        }
    }
    let questions = value_array(result, "open_questions");
    if !questions.is_empty() {
        lines.push(format!("questions: {}", questions.len()));
        for question in questions.iter().take(5) {
            lines.push(compact_line(&format!(
                "  {} {}",
                value_str(question, "question_id"),
                value_str(question, "message")
            )));
        }
    }
    let events = value_array(result, "events");
    lines.push(format!("journal: {}", events.len()));
    for event in events {
        lines.push(format!("  {}", event_label(event)));
    }
    lines.join("\n")
}

fn render_chat(result: &Value) -> String {
    let mut lines = Vec::new();
    if let Some(event) = result.get("event") {
        lines.push(format!("logged {}", event_label(event)));
    }
    if let Some(question) = result.get("question") {
        let qid = value_str(question, "question_id");
        if !qid.is_empty() {
            lines.push(format!("unlocked {qid}"));
        }
    }
    if let Some(journal) = result.get("journal") {
        lines.push(render_journal(journal));
    }
    lines.join("\n")
}

fn render_ask(result: &Value) -> String {
    let Some(question) = result.get("question") else {
        return result
            .get("event")
            .map(|event| format!("opened {}", event_label(event)))
            .unwrap_or_else(|| "opened question".to_string());
    };
    let qid = value_str(question, "question_id");
    let msg = value_str(question, "message");
    let left = value_f64(question, "seconds_left");
    let scope = scope_label(question);
    if result
        .get("no_repliers")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let notice = result
            .get("notice")
            .and_then(Value::as_str)
            .unwrap_or("no agents can reply");
        return ["ask ".to_string() + &qid + " not waiting", format!("notice: {notice}"), format!("question: {msg}"), scope]
            .into_iter()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
    }
    [
        format!("opened {qid} ({left:.0}s)"),
        format!("question: {msg}"),
        scope,
        format!("reply: chat(id='{qid}', message='...')"),
    ]
    .into_iter()
    .filter(|line| !line.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n")
}

fn render_weather(result: &Value) -> String {
    let mut lines = vec![format!("workspace: {}", value_str(result, "workspace_root"))];
    let agents = value_array(result, "agents");
    lines.push(format!("agents: {}", agents.len()));
    for agent in agents.iter().take(8) {
        lines.push(format!("  {}", agent_label(agent)));
    }
    let questions = value_array(result, "open_questions");
    lines.push(format!("open questions: {}", questions.len()));
    for question in questions.iter().take(5) {
        lines.push(compact_line(&format!(
            "  {} {:.0}s {}",
            value_str(question, "question_id"),
            value_f64(question, "seconds_left"),
            value_str(question, "message")
        )));
    }
    let recent = value_array(result, "recent");
    lines.push(format!("recent: {}", recent.len()));
    for event in recent.iter().rev().take(5).rev() {
        lines.push(format!("  {}", event_label(event)));
    }
    lines.join("\n")
}

fn render_presence(result: &Value) -> String {
    let agents = value_array(result, "agents");
    let mut lines = vec![
        format!("workgroup: {}", value_str(result, "workspace_root")),
        format!("agents: {}", agents.len()),
    ];
    for agent in agents {
        lines.push(format!("  {}", agent_label(agent)));
    }
    lines.join("\n")
}

fn render_recent(result: &Value) -> String {
    let tickets = value_array(result, "active_tickets");
    let questions = value_array(result, "open_questions");
    let events = value_array(result, "events");
    if tickets.is_empty() && questions.is_empty() && events.is_empty() {
        return "recent: (none)".to_string();
    }
    let mut lines = Vec::new();
    if !tickets.is_empty() {
        lines.push(format!("tickets: {}", tickets.len()));
        for ticket in tickets.iter().take(5) {
            lines.push(compact_line(&format!(
                "  {} {}",
                value_str(ticket, "ticket_id"),
                value_str(ticket, "message")
            )));
        }
    }
    if !questions.is_empty() {
        lines.push(format!("questions: {}", questions.len()));
        for question in questions.iter().take(5) {
            lines.push(compact_line(&format!(
                "  {} {:.0}s {}",
                value_str(question, "question_id"),
                value_f64(question, "seconds_left"),
                value_str(question, "message")
            )));
        }
    }
    lines.push(format!("recent: {}", events.len()));
    for event in events {
        lines.push(format!("  {}", event_label(event)));
    }
    if result
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.push("  ... truncated; narrow scope or raise limit".to_string());
    }
    lines.join("\n")
}

fn render_build_gate(result: &Value) -> String {
    let unlocked = result
        .get("unlocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut lines = vec![format!(
        "build gate: {} ({})",
        if unlocked { "unlocked" } else { "waiting" },
        value_str(result, "reason")
    )];
    if result
        .get("full_workspace")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        lines.push("scope: workspace".to_string());
    } else {
        let files = string_list(result, "files");
        if !files.is_empty() {
            lines.push(format!("scope: {}", files.join(", ")));
        }
    }
    for ticket in value_array(result, "active_tickets").iter().take(5) {
        lines.push(compact_line(&format!(
            "  {} {}",
            value_str(ticket, "ticket_id"),
            value_str(ticket, "message")
        )));
    }
    lines.join("\n")
}

fn render_edit_gate(result: &Value) -> String {
    let allowed = result
        .get("allowed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut lines = vec![format!(
        "edit gate: {} ({})",
        if allowed { "allowed" } else { "denied" },
        value_str(result, "reason")
    )];
    if let Some(ticket) = result.get("ticket") {
        lines.push(compact_line(&format!(
            "ticket {}: {}",
            value_str(ticket, "ticket_id"),
            value_str(ticket, "message")
        )));
    }
    let active = value_array(result, "active_tickets");
    if !active.is_empty() {
        lines.push(format!("active tickets: {}", active.len()));
    }
    lines.join("\n")
}

fn render_settle(result: &Value) -> String {
    let closed = value_array(result, "closed");
    if closed.is_empty() {
        return "settle: no expired questions".to_string();
    }
    let mut lines = vec!["closed questions:".to_string()];
    for digest in closed {
        if let Some(question) = digest.get("question") {
            lines.push(format!(
                "  {}: {}",
                value_str(question, "question_id"),
                value_str(question, "message")
            ));
        }
        for event in value_array(digest, "events").iter().rev().take(5).rev() {
            lines.push(format!("    {}", event_label(event)));
        }
    }
    lines.join("\n")
}

fn render_precommit(result: &Value) -> String {
    let mut lines = vec!["precommit weather:".to_string()];
    let recent = value_array(result, "recent");
    if recent.is_empty() {
        lines.push("  (no related recent bus activity)".to_string());
    } else {
        for event in recent.iter().rev().take(8).rev() {
            lines.push(format!("  {}", event_label(event)));
        }
    }
    let suggested = string_list(result, "suggested");
    if !suggested.is_empty() {
        lines.push("suggested checks:".to_string());
        for item in suggested {
            lines.push(format!("  {item}"));
        }
    }
    lines.join("\n")
}

fn event_label(event: &Value) -> String {
    let mut event_id = value_str(event, "event_id");
    if event_id.is_empty() {
        let seq = event.get("seq").and_then(Value::as_u64).unwrap_or(0);
        if seq > 0 {
            event_id = seq.to_string();
        }
    }
    if !event_id.is_empty() && !event_id.starts_with('E') {
        event_id = format!("E{event_id}");
    }
    let kind = value_str(event, "event_type");
    let kind = if kind.is_empty() {
        value_str(event, "kind")
    } else {
        kind
    };
    let message = value_str(event, "message");
    let agent = value_str(event, "agent_id");
    let scope = scope_label(event);
    let mut label = [event_id, kind, message]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !agent.is_empty() {
        label.push_str(&format!(" @{agent}"));
    }
    if !scope.is_empty() {
        label.push_str(&format!(" [{scope}]"));
    }
    compact_line(&label)
}

fn agent_label(agent: &Value) -> String {
    compact_line(&format!(
        "{} {} idle={:.0}s prompts={} last={}",
        value_str(agent, "agent_id"),
        value_str(agent, "state"),
        value_f64(agent, "idle_seconds"),
        agent.get("prompt_count").and_then(Value::as_u64).unwrap_or(0),
        value_str(agent, "last_event_id"),
    ))
}

fn scope_label(item: &Value) -> String {
    ["files", "symbols", "aliases"]
        .into_iter()
        .filter_map(|key| {
            let values = string_list(item, key);
            (!values.is_empty()).then(|| format!("{key}={}", values.join(",")))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn tool_text(header: &str, text: String, is_error: bool) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": format!("[{header}]\n{text}"),
            }
        ],
        "isError": is_error,
    })
}

fn tool_error_text(message: impl Into<String>) -> Value {
    tool_text("hsp/error", message.into(), true)
}

fn write_response(stdout: &mut impl Write, response: Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *stdout, &response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn json_rpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn json_rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

fn insert_argument(params: &mut Map<String, Value>, arguments: &Map<String, Value>, key: &str) {
    if let Some(value) = arguments.get(key).filter(|value| !value.is_null()) {
        params.insert(key.to_string(), value.clone());
    }
}

fn insert_metadata(
    metadata: &mut BTreeMap<String, String>,
    arguments: &Map<String, Value>,
    key: &str,
) {
    if let Some(value) = arg_string(arguments, key).filter(|value| !value.is_empty()) {
        metadata.insert(key.to_string(), value);
    }
}

fn add_metadata(params: &mut Map<String, Value>, key: &str, value: String) {
    let metadata = params
        .entry("metadata".to_string())
        .or_insert_with(|| json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.insert(key.to_string(), json!(value));
    }
}

fn arg_text<'a>(arguments: &'a Map<String, Value>, key: &str) -> &'a str {
    arguments.get(key).and_then(Value::as_str).unwrap_or("")
}

fn arg_string(arguments: &Map<String, Value>, key: &str) -> Option<String> {
    arguments.get(key).and_then(|value| match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    })
}

fn arg_u64(arguments: &Map<String, Value>, key: &str, default: u64) -> u64 {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .or_else(|| arguments.get(key).and_then(Value::as_str)?.parse().ok())
        .unwrap_or(default)
}

fn workspace_root(arguments: &Map<String, Value>) -> Result<String, std::io::Error> {
    if let Some(root) = arg_string(arguments, "workspace_root").filter(|root| !root.is_empty()) {
        return Ok(normalize_root(root));
    }
    if let Ok(root) = std::env::var("LSP_ROOT") {
        if !root.is_empty() {
            return Ok(normalize_root(root));
        }
    }
    Ok(std::env::current_dir()?.to_string_lossy().into_owned())
}

fn normalize_root(raw: String) -> String {
    let path = std::path::PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .expect("current directory is available")
            .join(path)
    };
    path.to_string_lossy().into_owned()
}

fn default_agent_id() -> String {
    std::env::var("HSP_AGENT_ID")
        .or_else(|_| std::env::var("CODEX_AGENT_ID"))
        .unwrap_or_else(|_| format!("hsp-mcp-{}", std::process::id()))
}

fn default_client_id() -> String {
    std::env::var("HSP_CLIENT_ID").unwrap_or_else(|_| "hsp-mcp".to_string())
}

fn default_session_id() -> String {
    std::env::var("HSP_SESSION_ID")
        .or_else(|_| std::env::var("CODEX_THREAD_ID"))
        .or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID"))
        .unwrap_or_else(|_| default_client_id())
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn parse_duration_seconds(raw: &str, default: f64) -> f64 {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return default;
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
    number.parse::<f64>().map(|value| value * scale).unwrap_or(default)
}

fn value_str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|value| match value {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn value_f64(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn value_array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn string_list(value: &Value, key: &str) -> Vec<String> {
    value_array(value, key)
        .iter()
        .filter_map(|item| match item {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        })
        .collect()
}

fn compact_line(value: &str) -> String {
    const LIMIT: usize = 220;
    let value = value.trim_end();
    if value.chars().count() <= LIMIT {
        return value.to_string();
    }
    value.chars().take(LIMIT.saturating_sub(3)).collect::<String>() + "..."
}

fn mcp_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_advertises_tools_capability() {
        let request = json!({
            "params": {
                "protocolVersion": "2024-11-05",
            },
        });
        let result = initialize_result(request.as_object().unwrap());

        assert_eq!(result["protocolVersion"], json!("2024-11-05"));
        assert_eq!(result["capabilities"]["tools"]["listChanged"], json!(false));
        assert_eq!(result["serverInfo"]["name"], json!("hsp"));
    }

    #[test]
    fn tool_list_matches_python_broker_tool_names() {
        let list = tools_list();
        let names = list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "lsp_log",
                "ticket",
                "journal",
                "ask",
                "chat",
                "lsp_memory",
                "lsp_session",
                "lsp_outline"
            ]
        );
    }

    #[test]
    fn non_event_kind_lands_in_metadata() {
        let mut args = Map::new();
        args.insert("workspace_root".to_string(), json!("/repo"));
        args.insert("kind".to_string(), json!("tool.after"));

        let mut params = bus_params(&args, "note").unwrap();
        add_metadata(&mut params, "kind", "tool.after".to_string());

        assert_eq!(params["metadata"]["kind"], json!("tool.after"));
        assert!(params.get("event_type").is_none());
    }
}

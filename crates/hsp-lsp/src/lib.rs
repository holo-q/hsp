use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainServer {
    pub command: String,
    pub args: Vec<String>,
    pub name: String,
    pub label: String,
}

impl ChainServer {
    pub fn new(command: impl Into<String>, args: Vec<String>, label: impl Into<String>) -> Self {
        let command = command.into();
        Self {
            name: command.clone(),
            command,
            args,
            label: label.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainParseError {
    message: String,
}

impl ChainParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ChainParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ChainParseError {}

pub fn parse_replace(raw: &str) -> BTreeMap<String, String> {
    raw.split(',')
        .filter_map(|entry| {
            let (old, new) = entry.trim().split_once('=')?;
            let old = old.trim();
            let new = new.trim();
            (!old.is_empty() && !new.is_empty()).then(|| (old.to_string(), new.to_string()))
        })
        .collect()
}

pub fn parse_chain_from_env() -> Result<Vec<ChainServer>, ChainParseError> {
    parse_chain(|name| std::env::var(name).unwrap_or_default())
}

pub fn parse_prefer_from_env(chain: &[ChainServer]) -> BTreeMap<String, usize> {
    parse_prefer(|name| std::env::var(name).unwrap_or_default(), chain)
}

pub fn parse_chain(env: impl Fn(&str) -> String) -> Result<Vec<ChainServer>, ChainParseError> {
    let replace = parse_replace(&env("LSP_REPLACE"));
    let sub = |command: &str| {
        replace
            .get(command)
            .cloned()
            .unwrap_or_else(|| command.to_string())
    };

    let servers_env = env("LSP_SERVERS");
    let servers_env = servers_env.trim();
    if !servers_env.is_empty() {
        let chain = servers_env
            .split(';')
            .filter_map(|entry| {
                let tokens = entry.split_whitespace().collect::<Vec<_>>();
                let (command, args) = tokens.split_first()?;
                let command = sub(command);
                let args = args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
                Some((command, args))
            })
            .enumerate()
            .map(|(index, (command, args))| {
                let label = if index == 0 {
                    command.clone()
                } else if index == 1 {
                    format!("{command} (fallback)")
                } else {
                    format!("{command} (fallback {index})")
                };
                ChainServer::new(command, args, label)
            })
            .collect::<Vec<_>>();
        if chain.is_empty() {
            return Err(ChainParseError::new("LSP_SERVERS is empty or malformed"));
        }
        return Ok(chain);
    }

    let primary = sub(env("LSP_COMMAND").trim());
    if primary.is_empty() {
        return Err(ChainParseError::new(
            "LSP_SERVERS or LSP_COMMAND environment variable is required",
        ));
    }

    let mut chain = vec![ChainServer::new(
        primary.clone(),
        split_args(&env("LSP_ARGS")),
        primary,
    )];

    let first_fallback = sub(env("LSP_FALLBACK_COMMAND").trim());
    if !first_fallback.is_empty() {
        chain.push(ChainServer::new(
            first_fallback.clone(),
            split_args(&env("LSP_FALLBACK_ARGS")),
            format!("{first_fallback} (fallback)"),
        ));
    }

    let mut index = 2;
    loop {
        let command_name = format!("LSP_FALLBACK_{index}_COMMAND");
        let command = sub(env(&command_name).trim());
        if command.is_empty() {
            break;
        }
        let args_name = format!("LSP_FALLBACK_{index}_ARGS");
        chain.push(ChainServer::new(
            command.clone(),
            split_args(&env(&args_name)),
            format!("{command} (fallback {index})"),
        ));
        index += 1;
    }

    Ok(chain)
}

pub fn parse_prefer(env: impl Fn(&str) -> String, chain: &[ChainServer]) -> BTreeMap<String, usize> {
    let prefer = env("LSP_PREFER");
    let prefer = prefer.trim();
    if prefer.is_empty() {
        return BTreeMap::new();
    }
    let replace = parse_replace(&env("LSP_REPLACE"));

    prefer
        .split(',')
        .filter_map(|entry| {
            let (method, command) = entry.trim().split_once('=')?;
            let method = method.trim();
            let command = command.trim();
            if method.is_empty() || command.is_empty() {
                return None;
            }
            let command = replace
                .get(command)
                .map(String::as_str)
                .unwrap_or(command);
            chain
                .iter()
                .position(|server| server.command == command)
                .map(|index| (method.to_string(), index))
        })
        .collect()
}

pub fn file_uri(path: impl AsRef<Path>) -> Result<String, std::io::Error> {
    let absolute = std::path::absolute(path)?;
    Ok(format!("file://{}", percent_encode_path(&absolute)))
}

pub fn language_id_for_path(path: impl AsRef<Path>) -> &'static str {
    match path.as_ref().extension().and_then(|extension| extension.to_str()) {
        Some("py" | "pyi") => "python",
        Some("rs") => "rust",
        Some("go") => "go",
        Some("js") => "javascript",
        Some("ts") => "typescript",
        Some("jsx") => "javascriptreact",
        Some("tsx") => "typescriptreact",
        Some("java") => "java",
        Some("c" | "h") => "c",
        Some("cpp") => "cpp",
        Some("rb") => "ruby",
        Some("lua") => "lua",
        Some("zig") => "zig",
        _ => "plaintext",
    }
}

pub fn language_id_for_uri(uri: &str) -> &'static str {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    language_id_for_path(path)
}

#[derive(Debug)]
pub enum LspFrameError {
    Io(std::io::Error),
    Json(serde_json::Error),
    InvalidHeader(String),
    MissingContentLength,
}

impl Display for LspFrameError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "LSP frame I/O error: {error}"),
            Self::Json(error) => write!(formatter, "LSP frame JSON error: {error}"),
            Self::InvalidHeader(header) => write!(formatter, "invalid LSP header: {header}"),
            Self::MissingContentLength => formatter.write_str("LSP frame missing Content-Length"),
        }
    }
}

impl Error for LspFrameError {}

impl From<std::io::Error> for LspFrameError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LspFrameError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn encode_lsp_message(message: &Value) -> Result<Vec<u8>, serde_json::Error> {
    let body = serde_json::to_vec(message)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

pub fn read_lsp_message(reader: &mut impl BufRead) -> Result<Option<Value>, LspFrameError> {
    let mut content_length = None;
    let mut saw_header = false;

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return if saw_header {
                Err(LspFrameError::MissingContentLength)
            } else {
                Ok(None)
            };
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        saw_header = true;
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(LspFrameError::InvalidHeader(trimmed.to_string()));
        };
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim().parse::<usize>().map_err(|_| {
                LspFrameError::InvalidHeader(format!("Content-Length: {}", value.trim()))
            })?;
            content_length = Some(value);
        }
    }

    let Some(content_length) = content_length else {
        return Err(LspFrameError::MissingContentLength);
    };
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

#[derive(Debug)]
pub enum LspClientError {
    Io(std::io::Error),
    Frame(LspFrameError),
    Json(serde_json::Error),
    Protocol(String),
    Server { code: i64, message: String, data: Option<Value> },
}

impl Display for LspClientError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "LSP I/O error: {error}"),
            Self::Frame(error) => Display::fmt(error, formatter),
            Self::Json(error) => write!(formatter, "LSP JSON error: {error}"),
            Self::Protocol(message) => formatter.write_str(message),
            Self::Server {
                code,
                message,
                data,
            } => {
                write!(formatter, "LSP error {code}: {message}")?;
                if let Some(data) = data {
                    write!(formatter, "\nData: {data}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for LspClientError {}

impl From<std::io::Error> for LspClientError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<LspFrameError> for LspClientError {
    fn from(error: LspFrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<serde_json::Error> for LspClientError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug)]
pub struct LspClient {
    command: ChainServer,
    root_path: PathBuf,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    capabilities: Value,
    open_documents: HashSet<String>,
}

#[derive(Debug)]
pub struct LspRuntime {
    root_path: PathBuf,
    chain: Vec<ChainServer>,
    prefer: BTreeMap<String, usize>,
    method_handler: BTreeMap<String, usize>,
    clients: Vec<Option<LspClient>>,
    request_count: u64,
    last_method: String,
    last_server_label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LspRequestResult {
    pub result: Value,
    pub server_label: String,
    pub started: Vec<String>,
}

impl LspRequestResult {
    pub fn to_wire(&self) -> Value {
        json!({
            "result": self.result,
            "server_label": self.server_label,
            "started": self.started,
            "workspaces_added": [],
        })
    }
}

impl LspClient {
    pub fn start(command: ChainServer, root_path: impl AsRef<Path>) -> Result<Self, LspClientError> {
        let root_path = std::path::absolute(root_path)?;
        let mut child = Command::new(&command.command)
            .args(&command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    LspClientError::Protocol(format!("missing LSP binary: {}", command.command))
                } else {
                    LspClientError::Io(error)
                }
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspClientError::Protocol("LSP child stdin was not piped".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspClientError::Protocol("LSP child stdout was not piped".to_string()))?;

        let mut client = Self {
            command,
            root_path,
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            capabilities: Value::Null,
            open_documents: HashSet::new(),
        };
        let initialized = client.request("initialize", initialize_params(&client.root_path)?)?;
        client.capabilities = initialized
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    pub fn command(&self) -> &ChainServer {
        &self.command
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn capabilities(&self) -> &Value {
        &self.capabilities
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, LspClientError> {
        if method.starts_with("textDocument/") {
            self.ensure_document_for_params(&params)?;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;

        loop {
            let Some(message) = read_lsp_message(&mut self.stdout)? else {
                return Err(LspClientError::Protocol(format!(
                    "LSP server exited before response to {method}"
                )));
            };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error").and_then(Value::as_object) {
                return Err(server_error(error));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), LspClientError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    pub fn stop(mut self) -> Result<(), LspClientError> {
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
        let _ = self.child.wait();
        Ok(())
    }

    fn write_message(&mut self, message: &Value) -> Result<(), LspClientError> {
        let frame = encode_lsp_message(message)?;
        self.stdin.write_all(&frame)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn ensure_document_for_params(&mut self, params: &Value) -> Result<(), LspClientError> {
        let Some(uri) = params
            .get("textDocument")
            .and_then(Value::as_object)
            .and_then(|text_document| text_document.get("uri"))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        if self.open_documents.contains(uri) {
            return Ok(());
        }
        let path = path_from_file_uri(uri).ok_or_else(|| {
            LspClientError::Protocol(format!("textDocument uri is not a file URI: {uri}"))
        })?;
        let text = std::fs::read_to_string(&path)?;
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id_for_path(&path),
                    "version": 1,
                    "text": text,
                }
            }),
        )?;
        self.open_documents.insert(uri.to_string());
        Ok(())
    }
}

impl LspRuntime {
    pub fn from_env(root_path: impl AsRef<Path>) -> Result<Self, ChainParseError> {
        let chain = parse_chain_from_env()?;
        let prefer = parse_prefer_from_env(&chain);
        Ok(Self::new(root_path, chain, prefer))
    }

    pub fn new(
        root_path: impl AsRef<Path>,
        chain: Vec<ChainServer>,
        prefer: BTreeMap<String, usize>,
    ) -> Self {
        let root_path =
            std::path::absolute(root_path).unwrap_or_else(|_| PathBuf::from("."));
        let clients = (0..chain.len()).map(|_| None).collect();
        Self {
            root_path,
            chain,
            method_handler: prefer.clone(),
            prefer,
            clients,
            request_count: 0,
            last_method: String::new(),
            last_server_label: String::new(),
        }
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn chain(&self) -> &[ChainServer] {
        &self.chain
    }

    pub fn status(&self) -> Value {
        json!({
            "root": self.root_path,
            "chain": self.chain.iter().map(chain_server_to_wire).collect::<Vec<_>>(),
            "clients": self.clients.iter().enumerate().map(|(index, client)| {
                json!({
                    "index": index,
                    "server_label": self.chain[index].label,
                    "started": client.is_some(),
                })
            }).collect::<Vec<_>>(),
            "prefer": self.prefer,
            "method_handler": self.method_handler,
            "request_count": self.request_count,
            "last_method": self.last_method,
            "last_server_label": self.last_server_label,
        })
    }

    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        empty_fallback_methods: &[String],
    ) -> Result<LspRequestResult, LspClientError> {
        if self.chain.is_empty() {
            return Err(LspClientError::Protocol(
                "LSP chain is empty; set LSP_SERVERS or LSP_COMMAND".to_string(),
            ));
        }

        let mut order = Vec::new();
        if let Some(index) = self
            .method_handler
            .get(method)
            .or_else(|| self.prefer.get(method))
            .copied()
            .filter(|index| *index < self.chain.len())
        {
            order.push(index);
        }
        for index in 0..self.chain.len() {
            if !order.contains(&index) {
                order.push(index);
            }
        }

        let mut last_error = None;
        let mut started = Vec::new();
        let allow_empty_fallback = empty_fallback_methods.iter().any(|item| item == method);
        for index in order {
            if self.clients[index].is_none() {
                let client = LspClient::start(self.chain[index].clone(), &self.root_path)?;
                self.clients[index] = Some(client);
                started.push(self.chain[index].label.clone());
            }
            let client = self.clients[index]
                .as_mut()
                .expect("client was just initialized");
            match client.request(method, params.clone()) {
                Ok(result) if allow_empty_fallback && is_empty_lsp_result(&result) => {
                    last_error = Some(LspClientError::Protocol(format!(
                        "{} returned an empty result for {method}",
                        self.chain[index].label
                    )));
                    continue;
                }
                Ok(result) => {
                    self.method_handler.insert(method.to_string(), index);
                    self.request_count += 1;
                    self.last_method = method.to_string();
                    self.last_server_label = self.chain[index].label.clone();
                    return Ok(LspRequestResult {
                        result,
                        server_label: self.chain[index].label.clone(),
                        started,
                    });
                }
                Err(error) => {
                    self.clients[index] = None;
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            LspClientError::Protocol(format!("no LSP server handled {method}"))
        }))
    }

    pub fn stop(&mut self) {
        for client in &mut self.clients {
            if let Some(client) = client.take() {
                let _ = client.stop();
            }
        }
    }
}

impl Drop for LspRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn initialize_params(root_path: &Path) -> Result<Value, std::io::Error> {
    let root_path = std::path::absolute(root_path)?;
    let root_uri = file_uri(&root_path)?;
    let root_name = root_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    Ok(json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "rootPath": root_path.to_string_lossy(),
        "capabilities": {
            "textDocument": {
                "diagnostic": {},
                "codeAction": {},
                "rename": {"prepareSupport": true},
                "signatureHelp": {},
                "completion": {
                    "completionItem": {"snippetSupport": false},
                },
                "formatting": {},
                "typeDefinition": {},
                "documentSymbol": {},
                "publishDiagnostics": {"relatedInformation": true},
                "callHierarchy": {},
                "typeHierarchy": {},
            },
            "workspace": {
                "workspaceFolders": true,
                "configuration": true,
                "fileOperations": {
                    "dynamicRegistration": false,
                    "willRename": true,
                    "didRename": true,
                    "willCreate": true,
                    "didCreate": true,
                    "willDelete": true,
                    "didDelete": true,
                },
                "workspaceEdit": {
                    "documentChanges": true,
                    "resourceOperations": ["create", "rename", "delete"],
                    "failureHandling": "textOnlyTransactional",
                    "normalizesLineEndings": true,
                    "changeAnnotationSupport": {"groupsOnLabel": true},
                },
            },
        },
        "workspaceFolders": [
            {"uri": root_uri, "name": root_name},
        ],
    }))
}

fn server_error(error: &Map<String, Value>) -> LspClientError {
    LspClientError::Server {
        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown LSP error")
            .to_string(),
        data: error.get("data").cloned(),
    }
}

fn chain_server_to_wire(server: &ChainServer) -> Value {
    json!({
        "command": server.command,
        "args": server.args,
        "name": server.name,
        "label": server.label,
    })
}

fn is_empty_lsp_result(result: &Value) -> bool {
    match result {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(items) => items.is_empty(),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let raw = uri.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode_path(raw)?))
}

fn percent_decode_path(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = bytes.get(index + 1).copied()?;
            let lo = bytes.get(index + 2).copied()?;
            decoded.push(hex_byte(hi, lo)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_byte(hi: u8, lo: u8) -> Option<u8> {
    Some(hex_nibble(hi)? << 4 | hex_nibble(lo)?)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn split_args(raw: &str) -> Vec<String> {
    raw.split_whitespace().map(str::to_string).collect()
}

fn percent_encode_path(path: &Path) -> String {
    path.to_string_lossy()
        .as_bytes()
        .iter()
        .flat_map(|byte| match *byte {
            b'/' | b'-' | b'.' | b'_' | b'~' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => {
                vec![*byte as char]
            }
            byte => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::BufReader;

    fn env<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> String + 'a {
        |name| {
            values
                .iter()
                .find_map(|(key, value)| (*key == name).then(|| (*value).to_string()))
                .unwrap_or_default()
        }
    }

    #[test]
    fn parse_replace_ignores_malformed_entries() {
        assert_eq!(
            parse_replace("basedpyright=pylance, nope, ty = ty-nightly "),
            BTreeMap::from([
                ("basedpyright".to_string(), "pylance".to_string()),
                ("ty".to_string(), "ty-nightly".to_string()),
            ])
        );
    }

    #[test]
    fn parse_lsp_servers_chain_with_replacements() {
        let chain = parse_chain(env(&[
            ("LSP_REPLACE", "basedpyright-langserver=pylance-language-server"),
            (
                "LSP_SERVERS",
                "ty server; basedpyright-langserver --stdio; pyright-langserver --stdio",
            ),
        ]))
        .unwrap();

        assert_eq!(chain[0], ChainServer::new("ty", vec!["server".to_string()], "ty"));
        assert_eq!(chain[1].command, "pylance-language-server");
        assert_eq!(chain[1].label, "pylance-language-server (fallback)");
        assert_eq!(chain[2].label, "pyright-langserver (fallback 2)");
    }

    #[test]
    fn parse_legacy_chain_and_numbered_fallbacks() {
        let chain = parse_chain(env(&[
            ("LSP_COMMAND", "rust-analyzer"),
            ("LSP_FALLBACK_COMMAND", "ra-mirror"),
            ("LSP_FALLBACK_ARGS", "--stdio"),
            ("LSP_FALLBACK_2_COMMAND", "ra-cold"),
            ("LSP_FALLBACK_2_ARGS", "--cold"),
        ]))
        .unwrap();

        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].label, "rust-analyzer");
        assert_eq!(chain[1].args, vec!["--stdio"]);
        assert_eq!(chain[2].label, "ra-cold (fallback 2)");
    }

    #[test]
    fn parse_prefer_uses_replaced_command_names() {
        let chain = parse_chain(env(&[
            ("LSP_REPLACE", "basedpyright-langserver=pylance-language-server"),
            ("LSP_SERVERS", "ty server; basedpyright-langserver --stdio"),
        ]))
        .unwrap();
        let prefer = parse_prefer(
            env(&[
                ("LSP_REPLACE", "basedpyright-langserver=pylance-language-server"),
                ("LSP_PREFER", "workspace/willRenameFiles=basedpyright-langserver"),
            ]),
            &chain,
        );

        assert_eq!(prefer["workspace/willRenameFiles"], 1);
    }

    #[test]
    fn language_id_matches_reference_extension_map() {
        assert_eq!(language_id_for_path("x.py"), "python");
        assert_eq!(language_id_for_path("x.tsx"), "typescriptreact");
        assert_eq!(language_id_for_path("x.unknown"), "plaintext");
    }

    #[test]
    fn file_uri_uses_file_scheme_and_percent_encoding() {
        let uri = file_uri(std::path::PathBuf::from("a path.rs")).unwrap();

        assert!(uri.starts_with("file:///"));
        assert!(uri.ends_with("a%20path.rs"));
    }

    #[test]
    fn lsp_frame_round_trips_jsonrpc_messages() {
        let message = json!({"jsonrpc": "2.0", "id": 7, "method": "initialize"});
        let frame = encode_lsp_message(&message).unwrap();
        let mut reader = BufReader::new(frame.as_slice());

        assert_eq!(read_lsp_message(&mut reader).unwrap(), Some(message));
        assert_eq!(read_lsp_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn lsp_frame_accepts_extra_headers_and_lf() {
        let frame = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\nContent-Length: 15\n\n{\"jsonrpc\":\"2\"}";
        let mut reader = BufReader::new(frame.as_slice());

        assert_eq!(
            read_lsp_message(&mut reader).unwrap(),
            Some(json!({"jsonrpc": "2"}))
        );
    }

    #[test]
    fn lsp_frame_requires_content_length() {
        let mut reader = BufReader::new(b"Content-Type: test\r\n\r\n{}".as_slice());

        assert!(matches!(
            read_lsp_message(&mut reader),
            Err(LspFrameError::MissingContentLength)
        ));
    }

    #[test]
    fn initialize_params_match_reference_capability_shape() {
        let params = initialize_params(Path::new(".")).unwrap();

        assert_eq!(params["capabilities"]["workspace"]["workspaceFolders"], json!(true));
        assert_eq!(
            params["capabilities"]["workspace"]["workspaceEdit"]["resourceOperations"],
            json!(["create", "rename", "delete"])
        );
        assert_eq!(
            params["capabilities"]["textDocument"]["rename"]["prepareSupport"],
            json!(true)
        );
        assert!(params["rootUri"].as_str().unwrap().starts_with("file://"));
    }

    #[test]
    fn file_uri_path_round_trip_decodes_percent_escapes() {
        let path = PathBuf::from("a path.rs");
        let uri = file_uri(&path).unwrap();
        let decoded = path_from_file_uri(&uri).unwrap();

        assert!(decoded.ends_with("a path.rs"));
    }

    #[test]
    fn runtime_status_reports_chain_without_starting_clients() {
        let runtime = LspRuntime::new(
            ".",
            vec![ChainServer::new("rust-analyzer", Vec::new(), "rust-analyzer")],
            BTreeMap::from([("textDocument/definition".to_string(), 0)]),
        );
        let status = runtime.status();

        assert_eq!(status["chain"][0]["command"], json!("rust-analyzer"));
        assert_eq!(status["clients"][0]["started"], json!(false));
        assert_eq!(status["method_handler"]["textDocument/definition"], json!(0));
    }

    #[test]
    #[ignore = "requires rust-analyzer on PATH and starts a real language server"]
    fn rust_analyzer_smoke_initializes_and_shuts_down() {
        if std::process::Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let client = LspClient::start(ChainServer::new("rust-analyzer", Vec::new(), "rust-analyzer"), ".");
        let client = client.unwrap();
        assert!(client.capabilities().is_object());
        client.stop().unwrap();
    }
}

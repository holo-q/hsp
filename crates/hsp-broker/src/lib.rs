use std::time::{SystemTime, UNIX_EPOCH};

use hsp_bus::{
    BusEventWire, BusJournal, DEFAULT_JOURNAL_LIMIT, DEFAULT_RECENT_LIMIT, EditGateMode,
    EventQuery, JournalAppend, QuestionClose, QuestionOpen, TicketBoard, TicketEffectKind,
    TicketIntent,
};
use hsp_session::{SessionKey, SessionRegistry};
use hsp_wire::{BusEventKind, BusScope};
use hsp_wire::{BrokerErrorCode, BrokerRequest, BrokerResponse, BrokerWireError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct BrokerCore {
    started_at: f64,
    registry: SessionRegistry,
    bus: BusJournal,
    tickets: TicketBoard,
    shutting_down: bool,
}

impl BrokerCore {
    pub fn new() -> Self {
        Self::new_at(now_seconds())
    }

    pub fn new_at(started_at: f64) -> Self {
        Self {
            started_at,
            registry: SessionRegistry::new(),
            bus: BusJournal::new(),
            tickets: TicketBoard::new(),
            shutting_down: false,
        }
    }

    pub fn handle_value(&mut self, value: Value) -> BrokerResponse {
        let id = value
            .as_object()
            .and_then(|object| object.get("id"))
            .cloned()
            .unwrap_or(Value::Null);

        let request = match BrokerRequest::from_value(value) {
            Ok(request) => request,
            Err(error) => return BrokerResponse::error(id, error),
        };

        let result = match request.method.as_str() {
            "ping" => Ok(json!({"pong": true})),
            "status" => Ok(self.status_value()),
            "shutdown" => {
                self.shutting_down = true;
                Ok(json!({"shutting_down": true}))
            }
            "session.get_or_create" => self.session_get_or_create(&request),
            "session.list" => Ok(json!(self.session_records())),
            "session.stop" => self.session_stop(&request),
            "bus.status" => Ok(self.bus_status_value(now_seconds())),
            "bus.append" | "bus.event" => self.bus_event(&request, None),
            "bus.note" => self.bus_event(&request, Some(BusEventKind::NotePosted)),
            "bus.chat" => self.bus_chat(&request),
            "bus.ask" => self.bus_ask(&request),
            "bus.reply" => self.bus_reply(&request, false),
            "bus.question" => self.bus_question(&request),
            "bus.recent" => self.bus_recent(&request),
            "bus.journal" => self.bus_journal(&request),
            "bus.settle" => self.bus_settle(&request),
            "bus.weather" => self.bus_weather(&request),
            "bus.precommit" => self.bus_precommit(&request),
            "bus.postcommit" => self.bus_event(&request, Some(BusEventKind::CommitCreated)),
            "bus.ticket" => self.bus_ticket(&request),
            "bus.build_gate" => self.bus_build_gate(&request),
            "bus.edit_gate" => self.bus_edit_gate(&request),
            method => Err(BrokerWireError::new(
                BrokerErrorCode::UnknownMethod,
                format!("unknown method: {method}"),
            )),
        };

        match result {
            Ok(result) => BrokerResponse::result(request.id, result),
            Err(error) => BrokerResponse::error(request.id, error),
        }
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    pub fn session_count(&self) -> usize {
        self.registry.len()
    }

    fn status_value(&self) -> Value {
        json!({
            "pid": std::process::id(),
            "started_at": self.started_at,
            "uptime": (now_seconds() - self.started_at).max(0.0),
            "session_count": self.registry.len(),
            "sessions": self.session_records(),
            "bus": self.bus_status_value(now_seconds()),
            "devtools": {
                "enabled": false,
            },
            "babel_bridge": {
                "enabled": false,
            },
        })
    }

    fn session_records(&self) -> Vec<hsp_session::SessionRecord> {
        self.registry
            .all_sessions()
            .into_iter()
            .map(|session| session.to_record())
            .collect()
    }

    fn bus_status_value(&self, now: f64) -> Value {
        let open_questions = self
            .bus
            .open_questions()
            .into_iter()
            .map(|question| question.to_wire(now))
            .collect::<Vec<_>>();
        json!({
            "event_count": self.bus.event_count(),
            "last_event_id": self.bus.last_event_id(),
            "open_question_count": self.bus.open_question_count(),
            "open_questions": open_questions,
            "open_ticket_count": self.tickets.open_ticket_count(),
            "agent_count": 0,
        })
    }

    fn session_get_or_create(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let root = required_string(&request.params, "root")?;
        let config_hash = required_string(&request.params, "config_hash")?;
        let server_label = optional_string(&request.params, "server_label")?;
        let session = self.registry.get_or_create(
            SessionKey::new(root, config_hash),
            server_label.unwrap_or_default(),
        );
        serde_json::to_value(session.to_record()).map_err(internal_error)
    }

    fn session_stop(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let session_id = required_string(&request.params, "session_id")?;
        Ok(json!({"stopped": self.registry.stop(&session_id)}))
    }

    fn bus_ticket(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let workspace_root = workspace_root(&request.params);
        let agent_id = agent_id(&request.params);
        let message = optional_string(&request.params, "message")?.unwrap_or_default();
        let mut intent = TicketIntent::new(workspace_root.clone(), agent_id.clone(), message);
        intent.scope = scope_from_params(&request.params);
        intent.projects = project_roots(&request.params);
        intent.now = optional_f64(&request.params, "now")?.unwrap_or(0.0);

        let hold = self.tickets.hold_with_effects(intent);
        for effect in &hold.effects {
            let kind = match effect.kind {
                TicketEffectKind::Started => BusEventKind::TicketStarted,
                TicketEffectKind::Joined => BusEventKind::TicketJoined,
                TicketEffectKind::Released => BusEventKind::TicketReleased,
                TicketEffectKind::Closed => BusEventKind::TicketClosed,
            };
            let mut append = journal_append_from_params(&request.params, kind)?;
            append.workspace_root = effect.ticket.workspace_root.clone();
            append.workspace_id = workspace_id(&append.workspace_root);
            append.agent_id = agent_id.clone();
            append.message = effect.ticket.message.clone();
            append.scope = effect.ticket.scope.clone();
            append
                .metadata
                .insert("ticket_id".to_string(), effect.ticket.ticket_id.clone());
            self.bus.append(append);
        }
        Ok(json!({
            "ticket": hold.ticket,
            "active_tickets": self.tickets.active_tickets(&workspace_root),
        }))
    }

    fn bus_event(
        &mut self,
        request: &BrokerRequest,
        forced_kind: Option<BusEventKind>,
    ) -> Result<Value, BrokerWireError> {
        let kind = match forced_kind {
            Some(kind) => kind,
            None => {
                let event_type = optional_string(&request.params, "event_type")?
                    .or(optional_string(&request.params, "kind")?)
                    .unwrap_or_else(|| BusEventKind::TaskIntent.as_wire().to_string());
                BusEventKind::from_wire(&event_type).map_err(|error| {
                    BrokerWireError::new(BrokerErrorCode::InvalidParams, error.to_string())
                })?
            }
        };
        let event = self.bus.append(journal_append_from_params(&request.params, kind)?);
        serde_json::to_value(json!({"event": BusEventWire::from(event)})).map_err(internal_error)
    }

    fn bus_chat(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let question_id = question_id(&request.params);
        if question_id.is_empty() {
            return self.bus_event(request, Some(BusEventKind::ChatMessage));
        }
        self.bus_reply(request, true)
    }

    fn bus_ask(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let root = workspace_root(&request.params);
        let busy_agents = busy_agent_ids(&self.tickets.active_tickets(&root));
        let no_repliers = busy_agents.is_empty();
        let mut open = QuestionOpen::new(timeout_seconds(&request.params)?);
        open.close_immediately = no_repliers;
        let (event, question) = self
            .bus
            .ask(journal_append_from_params(&request.params, BusEventKind::BusAsk)?, open);
        let now = event.timestamp;
        Ok(json!({
            "event": BusEventWire::from(&event),
            "question": question.to_wire(now),
            "no_repliers": no_repliers,
            "notice": if no_repliers {
                "no agents can reply; no agents are currently busy in this workgroup"
            } else {
                ""
            },
            "busy_agents": busy_agents,
            "active_tickets": self.tickets.active_tickets(&root),
        }))
    }

    fn bus_reply(
        &mut self,
        request: &BrokerRequest,
        close_question: bool,
    ) -> Result<Value, BrokerWireError> {
        let question_id = question_id(&request.params);
        if question_id.is_empty() {
            return Err(BrokerWireError::new(
                BrokerErrorCode::InvalidParams,
                "reply requires id or question_id",
            ));
        }
        let (event, question) = self
            .bus
            .reply(
                &question_id,
                journal_append_from_params(&request.params, BusEventKind::BusReply)?,
                close_question,
            )
            .map_err(|error| {
                BrokerWireError::new(BrokerErrorCode::InvalidParams, error.to_string())
            })?;
        Ok(json!({
            "event": BusEventWire::from(&event),
            "question": question.to_wire(event.timestamp),
        }))
    }

    fn bus_question(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let question_id = question_id(&request.params);
        if question_id.is_empty() {
            return Err(BrokerWireError::new(
                BrokerErrorCode::InvalidParams,
                "question requires id or question_id",
            ));
        }
        let now = optional_f64(&request.params, "now")?.unwrap_or_else(now_seconds);
        let question = self.bus.question(&question_id).ok_or_else(|| {
            BrokerWireError::new(
                BrokerErrorCode::InvalidParams,
                format!("unknown question: {question_id}"),
            )
        })?;
        let replies = self
            .bus
            .replies_for_question(&question_id)
            .into_iter()
            .map(BusEventWire::from)
            .collect::<Vec<_>>();
        Ok(json!({
            "question": question.to_wire(now),
            "replies": replies,
        }))
    }

    fn bus_recent(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let query = event_query_from_params(&request.params, DEFAULT_RECENT_LIMIT)?;
        let now = optional_f64(&request.params, "now")?.unwrap_or_else(now_seconds);
        let append = journal_append_from_params(&request.params, BusEventKind::BusClosed)?;
        self.bus.settle(&query.workspace_root, now, &append);
        let events = self.bus.recent(&query);
        let selected_count = events.len();
        let truncated = self.bus.recent_is_truncated(&query, selected_count);
        let active_tickets = self
            .tickets
            .active_tickets(&query.workspace_root)
            .into_iter()
            .filter(|ticket| {
                query.scope.is_empty()
                    || ticket.scope.is_empty()
                    || ticket.scope.overlaps(&query.scope)
            })
            .collect::<Vec<_>>();
        let event_wires = events
            .into_iter()
            .map(BusEventWire::from)
            .collect::<Vec<_>>();
        let open_questions = self
            .bus
            .open_questions_for_workspace(&query.workspace_root)
            .into_iter()
            .map(|question| question.to_wire(now))
            .collect::<Vec<_>>();
        Ok(json!({
            "events": event_wires,
            "open_questions": open_questions,
            "active_tickets": active_tickets,
            "truncated": truncated,
            "last_event_id": self.bus.last_event_id(),
        }))
    }

    fn bus_journal(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let root = workspace_root(&request.params);
        let now = optional_f64(&request.params, "now")?.unwrap_or_else(now_seconds);
        let append = journal_append_from_params(&request.params, BusEventKind::BusClosed)?;
        self.bus.settle(&root, now, &append);
        let limit = bounded_limit(&request.params, DEFAULT_JOURNAL_LIMIT, 100)?;
        let events = self
            .bus
            .events_for_workspace(&root, limit)
            .into_iter()
            .map(BusEventWire::from)
            .collect::<Vec<_>>();
        let open_questions = self
            .bus
            .open_questions_for_workspace(&root)
            .into_iter()
            .map(|question| question.to_wire(now))
            .collect::<Vec<_>>();
        Ok(json!({
            "workspace_root": root,
            "events": events,
            "active_tickets": self.tickets.active_tickets(&root),
            "open_questions": open_questions,
        }))
    }

    fn bus_settle(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let root = workspace_root(&request.params);
        let now = optional_f64(&request.params, "now")?.unwrap_or_else(now_seconds);
        let append = journal_append_from_params(&request.params, BusEventKind::BusClosed)?;
        let closed = self
            .bus
            .settle(&root, now, &append)
            .into_iter()
            .map(|close| question_close_value(&close, now))
            .collect::<Vec<_>>();
        Ok(json!({"closed": closed}))
    }

    fn bus_weather(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let root = workspace_root(&request.params);
        let now = optional_f64(&request.params, "now")?.unwrap_or_else(now_seconds);
        let append = journal_append_from_params(&request.params, BusEventKind::BusClosed)?;
        self.bus.settle(&root, now, &append);
        let events = self
            .bus
            .events_for_workspace(&root, 10)
            .into_iter()
            .map(BusEventWire::from)
            .collect::<Vec<_>>();
        let open_questions = self
            .bus
            .open_questions_for_workspace(&root)
            .into_iter()
            .map(|question| question.to_wire(now))
            .collect::<Vec<_>>();
        Ok(json!({
            "workspace_root": root,
            "open_questions": open_questions.clone(),
            "recent": events,
            "agents": [],
            "status": self.bus_status_value(now),
        }))
    }

    fn bus_precommit(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let mut query = event_query_from_params(&request.params, 10)?;
        if query.limit == DEFAULT_RECENT_LIMIT {
            query.limit = 10;
        }
        let recent = self.bus.recent(&query);
        let suggested = recent
            .iter()
            .filter(|event| event.kind == BusEventKind::TestRan)
            .filter_map(|event| event.metadata.get("targets"))
            .flat_map(|targets| targets.split_whitespace().map(ToOwned::to_owned))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let events = recent.into_iter().map(BusEventWire::from).collect::<Vec<_>>();
        Ok(json!({"recent": events, "suggested": suggested}))
    }

    fn bus_build_gate(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let workspace_root = workspace_root(&request.params);
        let agent_id = agent_id(&request.params);
        let gate = self.tickets.build_gate(
            workspace_root,
            (!agent_id.is_empty()).then_some(agent_id.as_str()),
            scope_from_params(&request.params),
            project_roots(&request.params),
            optional_bool(&request.params, "full_workspace")?.unwrap_or(false),
        );
        serde_json::to_value(gate).map_err(internal_error)
    }

    fn bus_edit_gate(&mut self, request: &BrokerRequest) -> Result<Value, BrokerWireError> {
        let workspace_root = workspace_root(&request.params);
        let agent_id = agent_id(&request.params);
        let mode = match optional_string(&request.params, "mode")?.as_deref() {
            Some("agent") => EditGateMode::Agent,
            _ => EditGateMode::Workgroup,
        };
        serde_json::to_value(self.tickets.edit_gate(workspace_root, agent_id, mode))
            .map_err(internal_error)
    }
}

impl Default for BrokerCore {
    fn default() -> Self {
        Self::new()
    }
}

fn required_string(
    params: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<String, BrokerWireError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            BrokerWireError::new(
                BrokerErrorCode::InvalidParams,
                format!("missing or non-string param: {name}"),
            )
        })
}

fn optional_string(
    params: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<String>, BrokerWireError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(BrokerWireError::new(
            BrokerErrorCode::InvalidParams,
            format!("{name} must be a string"),
        )),
    }
}

fn optional_bool(
    params: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<bool>, BrokerWireError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(BrokerWireError::new(
            BrokerErrorCode::InvalidParams,
            format!("{name} must be a boolean"),
        )),
    }
}

fn optional_f64(
    params: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<f64>, BrokerWireError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value.as_f64().map(Some).ok_or_else(|| {
            BrokerWireError::new(BrokerErrorCode::InvalidParams, format!("{name} must be a number"))
        }),
        _ => Err(BrokerWireError::new(
            BrokerErrorCode::InvalidParams,
            format!("{name} must be a number"),
        )),
    }
}

fn optional_u64(
    params: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<u64>, BrokerWireError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value.as_u64().map(Some).ok_or_else(|| {
            BrokerWireError::new(
                BrokerErrorCode::InvalidParams,
                format!("{name} must be an unsigned integer"),
            )
        }),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Value::String(value)) => value.trim().parse::<u64>().map(Some).map_err(|_| {
            BrokerWireError::new(
                BrokerErrorCode::InvalidParams,
                format!("{name} must be an unsigned integer"),
            )
        }),
        _ => Err(BrokerWireError::new(
            BrokerErrorCode::InvalidParams,
            format!("{name} must be an unsigned integer"),
        )),
    }
}

fn workspace_root(params: &serde_json::Map<String, Value>) -> String {
    let raw = params
        .get("workspace_root")
        .or_else(|| params.get("root"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let path = if raw.is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        let path = std::path::PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    };
    path.to_string_lossy().into_owned()
}

fn agent_id(params: &serde_json::Map<String, Value>) -> String {
    ["agent_id", "client_id", "session_id"]
        .iter()
        .find_map(|key| params.get(*key).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn scope_from_params(params: &serde_json::Map<String, Value>) -> BusScope {
    BusScope {
        files: strings(params.get("files")),
        symbols: strings(params.get("symbols")),
        aliases: strings(params.get("aliases")),
    }
}

fn project_roots(params: &serde_json::Map<String, Value>) -> Vec<String> {
    let roots = strings(params.get("project_roots"));
    if roots.is_empty() {
        strings(params.get("projects"))
    } else {
        roots
    }
}

fn question_id(params: &serde_json::Map<String, Value>) -> String {
    params
        .get("id")
        .or_else(|| params.get("question_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn timeout_seconds(params: &serde_json::Map<String, Value>) -> Result<f64, BrokerWireError> {
    match params.get("timeout") {
        None | Some(Value::Null) => Ok(180.0),
        Some(Value::Number(value)) => value.as_f64().map(|value| value.max(0.0)).ok_or_else(|| {
            BrokerWireError::new(BrokerErrorCode::InvalidParams, "timeout must be a number")
        }),
        Some(Value::String(value)) => parse_timeout(value),
        _ => Err(BrokerWireError::new(
            BrokerErrorCode::InvalidParams,
            "timeout must be a number or duration string",
        )),
    }
}

fn parse_timeout(value: &str) -> Result<f64, BrokerWireError> {
    let raw = value.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return Ok(180.0);
    }
    let (number, scale) = if let Some(number) = raw.strip_suffix("ms") {
        (number, 0.001)
    } else if let Some(number) = raw.strip_suffix('s') {
        (number, 1.0)
    } else if let Some(number) = raw.strip_suffix('m') {
        (number, 60.0)
    } else if let Some(number) = raw.strip_suffix('h') {
        (number, 3600.0)
    } else {
        (raw.as_str(), 1.0)
    };
    number
        .parse::<f64>()
        .map(|value| (value * scale).max(0.0))
        .map_err(|_| {
            BrokerWireError::new(
                BrokerErrorCode::InvalidParams,
                format!("invalid timeout: {value}"),
            )
        })
}

fn busy_agent_ids(tickets: &[hsp_bus::Ticket]) -> Vec<String> {
    tickets
        .iter()
        .flat_map(|ticket| ticket.holders.keys().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(value)) => value
            .replace([',', '\n'], " ")
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn metadata(
    params: &serde_json::Map<String, Value>,
) -> Result<std::collections::BTreeMap<String, String>, BrokerWireError> {
    match params.get("metadata") {
        None | Some(Value::Null) => Ok(std::collections::BTreeMap::new()),
        Some(Value::Object(object)) => Ok(object
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                )
            })
            .collect()),
        _ => Err(BrokerWireError::new(
            BrokerErrorCode::InvalidParams,
            "metadata must be an object",
        )),
    }
}

fn journal_append_from_params(
    params: &serde_json::Map<String, Value>,
    kind: BusEventKind,
) -> Result<JournalAppend, BrokerWireError> {
    let workspace_root = workspace_root(params);
    let mut append = JournalAppend::new(kind);
    append.timestamp = optional_f64(params, "now")?.unwrap_or_else(now_seconds);
    append.workspace_id = workspace_id(&workspace_root);
    append.workspace_root = workspace_root;
    append.agent_id = params
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    append.client_id = params
        .get("client_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    append.session_id = params
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    append.task_id = params
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    append.git_head = params
        .get("git_head")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    append.dirty_hash = params
        .get("dirty_hash")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    append.scope = scope_from_params(params);
    append.message = optional_string(params, "message")?.unwrap_or_default();
    append.metadata = metadata(params)?;
    append.question_id = params
        .get("question_id")
        .or_else(|| params.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(append)
}

fn event_query_from_params(
    params: &serde_json::Map<String, Value>,
    default_limit: usize,
) -> Result<EventQuery, BrokerWireError> {
    let mut query = EventQuery::new(workspace_root(params));
    query.scope = scope_from_params(params);
    query.after_seq = optional_u64(params, "after_id")?
        .or(optional_u64(params, "after_seq")?)
        .unwrap_or(0);
    query.limit = bounded_limit(params, default_limit, 100)?;
    Ok(query)
}

fn bounded_limit(
    params: &serde_json::Map<String, Value>,
    default: usize,
    max: usize,
) -> Result<usize, BrokerWireError> {
    let limit = optional_u64(params, "limit")?.unwrap_or(default as u64);
    Ok((limit as usize).clamp(1, max))
}

fn workspace_id(workspace_root: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace_root.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    hex[..12].to_string()
}

fn question_close_value(close: &QuestionClose, now: f64) -> Value {
    let events = close
        .events
        .iter()
        .map(BusEventWire::from)
        .collect::<Vec<_>>();
    let replies = close
        .replies
        .iter()
        .map(BusEventWire::from)
        .collect::<Vec<_>>();
    json!({
        "question": close.question.to_wire(now),
        "close_event": BusEventWire::from(&close.close_event),
        "events": events,
        "replies": replies,
    })
}

fn internal_error(error: serde_json::Error) -> BrokerWireError {
    BrokerWireError::new(BrokerErrorCode::Internal, error.to_string())
}

fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;

    fn handle(core: &mut BrokerCore, request: Value) -> Value {
        serde_json::to_value(core.handle_value(request)).expect("response json")
    }

    fn error(response: &Value) -> &Map<String, Value> {
        response["error"].as_object().expect("error object")
    }

    #[test]
    fn ping_returns_pong() {
        let mut core = BrokerCore::new_at(10.0);
        assert_eq!(
            handle(&mut core, json!({"id": "c1", "method": "ping"})),
            json!({"id": "c1", "result": {"pong": true}})
        );
    }

    #[test]
    fn status_response_shape_matches_python_broker() {
        let mut core = BrokerCore::new_at(10.0);
        let response = handle(&mut core, json!({"id": "c2", "method": "status"}));
        let result = response["result"].as_object().expect("result object");

        assert_eq!(
            result.keys().cloned().collect::<std::collections::BTreeSet<_>>(),
            [
                "babel_bridge",
                "bus",
                "devtools",
                "pid",
                "session_count",
                "sessions",
                "started_at",
                "uptime",
            ]
            .into_iter()
            .map(ToOwned::to_owned)
            .collect()
        );
        assert_eq!(result["session_count"], json!(0));
        assert_eq!(result["sessions"], json!([]));
        assert_eq!(result["bus"]["event_count"], json!(0));
        assert_eq!(result["devtools"]["enabled"], json!(false));
        assert_eq!(result["babel_bridge"]["enabled"], json!(false));
    }

    #[test]
    fn workspace_id_matches_bus_registry_sha256_prefix() {
        assert_eq!(workspace_id("/repo"), "816fc349d3fa");
    }

    #[test]
    fn malformed_requests_return_structured_errors() {
        let mut core = BrokerCore::new_at(10.0);

        let response = handle(&mut core, json!({"id": "c3", "method": "does_not_exist"}));
        assert_eq!(error(&response)["code"], json!("unknown_method"));

        let response = handle(&mut core, json!({"id": "c4"}));
        assert_eq!(error(&response)["code"], json!("invalid_request"));

        let response = handle(&mut core, json!({"id": "c5", "method": "ping", "params": [1, 2]}));
        assert_eq!(error(&response)["code"], json!("invalid_request"));
    }

    #[test]
    fn session_get_or_create_reuses_records() {
        let mut core = BrokerCore::new_at(10.0);
        let request = json!({
            "id": "c7",
            "method": "session.get_or_create",
            "params": {"root": "/repo", "config_hash": "abc", "server_label": "ty"},
        });

        let first = handle(&mut core, request.clone());
        let second = handle(&mut core, request);

        assert_eq!(first["result"]["root"], json!("/repo"));
        assert_eq!(first["result"]["config_hash"], json!("abc"));
        assert_eq!(first["result"]["server_label"], json!("ty"));
        assert_eq!(first["result"]["session_id"], second["result"]["session_id"]);
        assert_eq!(core.session_count(), 1);
    }

    #[test]
    fn session_get_or_create_requires_root_and_hash() {
        let mut core = BrokerCore::new_at(10.0);
        let response = handle(
            &mut core,
            json!({"id": "c6", "method": "session.get_or_create", "params": {}}),
        );
        assert_eq!(error(&response)["code"], json!("invalid_params"));
    }

    #[test]
    fn session_list_and_stop_use_registry() {
        let mut core = BrokerCore::new_at(10.0);
        let created = handle(
            &mut core,
            json!({
                "id": "c1",
                "method": "session.get_or_create",
                "params": {"root": "/repo", "config_hash": "abc"},
            }),
        );
        let session_id = created["result"]["session_id"].clone();

        let listed = handle(&mut core, json!({"id": "c2", "method": "session.list"}));
        assert_eq!(listed["result"].as_array().expect("sessions").len(), 1);

        let stopped = handle(
            &mut core,
            json!({"id": "c3", "method": "session.stop", "params": {"session_id": session_id}}),
        );
        assert_eq!(stopped["result"], json!({"stopped": true}));
        assert_eq!(core.session_count(), 0);
    }

    #[test]
    fn shutdown_sets_state_and_response_id_is_echoed() {
        let mut core = BrokerCore::new_at(10.0);
        let response = handle(&mut core, json!({"id": 17, "method": "shutdown"}));

        assert_eq!(response, json!({"id": 17, "result": {"shutting_down": true}}));
        assert!(core.is_shutting_down());
    }

    #[test]
    fn bus_ticket_and_build_gate_round_trip() {
        let mut core = BrokerCore::new_at(10.0);
        handle(
            &mut core,
            json!({
                "id": "t1",
                "method": "bus.ticket",
                "params": {
                    "workspace_root": "/repo",
                    "agent_id": "agent-a",
                    "message": "edit server",
                },
            }),
        );
        handle(
            &mut core,
            json!({
                "id": "t2",
                "method": "bus.ticket",
                "params": {
                    "workspace_root": "/repo",
                    "agent_id": "agent-b",
                    "message": "edit server",
                },
            }),
        );

        let cold = handle(
            &mut core,
            json!({"id": "g1", "method": "bus.build_gate", "params": {"workspace_root": "/repo"}}),
        );
        let one_waiting = handle(
            &mut core,
            json!({
                "id": "g2",
                "method": "bus.build_gate",
                "params": {"workspace_root": "/repo", "agent_id": "agent-a"},
            }),
        );
        let all_waiting = handle(
            &mut core,
            json!({
                "id": "g3",
                "method": "bus.build_gate",
                "params": {"workspace_root": "/repo", "agent_id": "agent-b"},
            }),
        );

        assert_eq!(cold["result"]["reason"], json!("active_tickets"));
        assert_eq!(cold["result"]["unlocked"], json!(false));
        assert_eq!(one_waiting["result"]["unlocked"], json!(false));
        assert_eq!(all_waiting["result"]["reason"], json!("all_waiting"));
        assert_eq!(all_waiting["result"]["unlocked"], json!(true));
    }

    #[test]
    fn bus_append_recent_and_workspace_scoping_match_python_contract() {
        let mut core = BrokerCore::new_at(10.0);
        for (root, message) in [("/repo/a", "alpha"), ("/repo/b", "beta"), ("/repo/a", "gamma")] {
            handle(
                &mut core,
                json!({
                    "id": message,
                    "method": "bus.append",
                    "params": {
                        "workspace_root": root,
                        "event_type": "file.touched",
                        "message": message,
                        "files": ["src/server.py"],
                    },
                }),
            );
        }

        let recent = handle(
            &mut core,
            json!({
                "id": "recent",
                "method": "bus.recent",
                "params": {"workspace_root": "/repo/a", "files": "server.py", "limit": 10},
            }),
        );

        let events = recent["result"]["events"].as_array().expect("events");
        assert_eq!(
            events
                .iter()
                .map(|event| event["message"].as_str().expect("message"))
                .collect::<Vec<_>>(),
            vec!["alpha", "gamma"]
        );
        assert_eq!(events[0]["event_type"], json!("file.touched"));
        assert_eq!(events[0]["files"], json!(["src/server.py"]));
        assert_eq!(recent["result"]["truncated"], json!(false));
    }

    #[test]
    fn bus_journal_records_ticket_transitions_as_events() {
        let mut core = BrokerCore::new_at(10.0);
        for agent_id in ["agent-a", "agent-b"] {
            handle(
                &mut core,
                json!({
                    "id": agent_id,
                    "method": "bus.ticket",
                    "params": {
                        "workspace_root": "/repo",
                        "agent_id": agent_id,
                        "message": "coordinate journal",
                    },
                }),
            );
        }
        handle(
            &mut core,
            json!({
                "id": "release-a",
                "method": "bus.ticket",
                "params": {"workspace_root": "/repo", "agent_id": "agent-a", "message": ""},
            }),
        );
        handle(
            &mut core,
            json!({
                "id": "release-b",
                "method": "bus.ticket",
                "params": {"workspace_root": "/repo", "agent_id": "agent-b", "message": ""},
            }),
        );

        let journal = handle(
            &mut core,
            json!({
                "id": "journal",
                "method": "bus.journal",
                "params": {"workspace_root": "/repo"},
            }),
        );
        let kinds = journal["result"]["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|event| event["event_type"].as_str().expect("event type"))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "ticket.started",
                "ticket.joined",
                "ticket.released",
                "ticket.released",
                "ticket.closed",
            ]
        );
        assert_eq!(journal["result"]["active_tickets"], json!([]));
    }

    #[test]
    fn bus_precommit_suggests_targets_from_test_ran_metadata() {
        let mut core = BrokerCore::new_at(10.0);
        handle(
            &mut core,
            json!({
                "id": "test",
                "method": "bus.append",
                "params": {
                    "workspace_root": "/repo",
                    "event_type": "test.ran",
                    "message": "cargo test -p hsp-bus",
                    "metadata": {"targets": "cargo-test hsp-bus"},
                },
            }),
        );

        let response = handle(
            &mut core,
            json!({"id": "pre", "method": "bus.precommit", "params": {"workspace_root": "/repo"}}),
        );

        assert_eq!(response["result"]["suggested"], json!(["cargo-test", "hsp-bus"]));
    }

    #[test]
    fn bus_ask_reply_settle_and_question_round_trip() {
        let mut core = BrokerCore::new_at(10.0);
        handle(
            &mut core,
            json!({
                "id": "ticket",
                "method": "bus.ticket",
                "params": {
                    "workspace_root": "/repo",
                    "agent_id": "agent-b",
                    "message": "editing server",
                    "now": 99.0,
                },
            }),
        );
        let opened = handle(
            &mut core,
            json!({
                "id": "ask",
                "method": "bus.ask",
                "params": {
                    "workspace_root": "/repo",
                    "agent_id": "agent-a",
                    "message": "anyone touching server.py?",
                    "files": ["src/server.py"],
                    "timeout": 0,
                    "now": 100.0,
                },
            }),
        );
        let qid = opened["result"]["question"]["question_id"]
            .as_str()
            .expect("question id")
            .to_string();
        assert_eq!(opened["result"]["busy_agents"], json!(["agent-b"]));
        assert_eq!(opened["result"]["no_repliers"], json!(false));

        let reply = handle(
            &mut core,
            json!({
                "id": "reply",
                "method": "bus.reply",
                "params": {
                    "workspace_root": "/repo",
                    "id": qid,
                    "agent_id": "agent-b",
                    "message": "yes",
                    "now": 101.0,
                },
            }),
        );
        assert_eq!(reply["result"]["event"]["event_type"], json!("bus.reply"));

        let settled = handle(
            &mut core,
            json!({
                "id": "settle",
                "method": "bus.settle",
                "params": {"workspace_root": "/repo", "now": 102.0},
            }),
        );
        let closed = settled["result"]["closed"].as_array().expect("closed");
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0]["close_event"]["event_type"], json!("bus.closed"));
        assert_eq!(closed[0]["replies"][0]["event_type"], json!("bus.reply"));

        let question = handle(
            &mut core,
            json!({
                "id": "question",
                "method": "bus.question",
                "params": {"workspace_root": "/repo", "id": qid, "now": 103.0},
            }),
        );
        assert_eq!(question["result"]["replies"][0]["message"], json!("yes"));
        assert_eq!(question["result"]["question"]["closed_at"], json!(102.0));
    }

    #[test]
    fn bus_ask_without_busy_agents_closes_immediately() {
        let mut core = BrokerCore::new_at(10.0);
        let opened = handle(
            &mut core,
            json!({
                "id": "ask",
                "method": "bus.ask",
                "params": {
                    "workspace_root": "/repo",
                    "agent_id": "agent-a",
                    "message": "anyone editing?",
                    "timeout": "2m",
                    "now": 100.0,
                },
            }),
        );

        assert_eq!(opened["result"]["no_repliers"], json!(true));
        assert_eq!(opened["result"]["question"]["closed_at"], json!(100.0));

        let weather = handle(
            &mut core,
            json!({
                "id": "weather",
                "method": "bus.weather",
                "params": {"workspace_root": "/repo", "now": 101.0},
            }),
        );
        assert_eq!(weather["result"]["open_questions"], json!([]));
    }

    #[test]
    fn bus_chat_with_question_id_records_reply_and_closes_question() {
        let mut core = BrokerCore::new_at(10.0);
        handle(
            &mut core,
            json!({
                "id": "ticket",
                "method": "bus.ticket",
                "params": {
                    "workspace_root": "/repo",
                    "agent_id": "agent-b",
                    "message": "editing",
                    "now": 99.0,
                },
            }),
        );
        let opened = handle(
            &mut core,
            json!({
                "id": "ask",
                "method": "bus.ask",
                "params": {
                    "workspace_root": "/repo",
                    "agent_id": "agent-a",
                    "message": "build?",
                    "timeout": "30s",
                    "now": 100.0,
                },
            }),
        );
        let qid = opened["result"]["question"]["question_id"]
            .as_str()
            .expect("question id")
            .to_string();

        let replied = handle(
            &mut core,
            json!({
                "id": "chat",
                "method": "bus.chat",
                "params": {
                    "workspace_root": "/repo",
                    "agent_id": "agent-b",
                    "id": qid,
                    "message": "go",
                    "now": 101.0,
                },
            }),
        );

        assert_eq!(replied["result"]["event"]["event_type"], json!("bus.reply"));
        assert_eq!(replied["result"]["question"]["closed_at"], json!(101.0));
    }

    #[test]
    fn bus_edit_gate_respects_workgroup_and_agent_modes() {
        let mut core = BrokerCore::new_at(10.0);
        let denied = handle(
            &mut core,
            json!({
                "id": "e1",
                "method": "bus.edit_gate",
                "params": {"workspace_root": "/repo", "agent_id": "agent-a"},
            }),
        );
        handle(
            &mut core,
            json!({
                "id": "t1",
                "method": "bus.ticket",
                "params": {"workspace_root": "/repo", "agent_id": "agent-b", "message": "editing"},
            }),
        );
        let workgroup = handle(
            &mut core,
            json!({
                "id": "e2",
                "method": "bus.edit_gate",
                "params": {"workspace_root": "/repo", "agent_id": "agent-a"},
            }),
        );
        let agent = handle(
            &mut core,
            json!({
                "id": "e3",
                "method": "bus.edit_gate",
                "params": {"workspace_root": "/repo", "agent_id": "agent-b", "mode": "agent"},
            }),
        );

        assert_eq!(denied["result"]["allowed"], json!(false));
        assert_eq!(denied["result"]["reason"], json!("missing_ticket"));
        assert_eq!(workgroup["result"]["allowed"], json!(true));
        assert_eq!(workgroup["result"]["reason"], json!("ticket_active"));
        assert_eq!(agent["result"]["allowed"], json!(true));
        assert_eq!(agent["result"]["reason"], json!("ticket_held"));
    }
}

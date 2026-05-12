use std::time::{SystemTime, UNIX_EPOCH};

use hsp_bus::{BusJournal, EditGateMode, TicketBoard, TicketIntent};
use hsp_session::{SessionKey, SessionRegistry};
use hsp_wire::BusScope;
use hsp_wire::{BrokerErrorCode, BrokerRequest, BrokerResponse, BrokerWireError};
use serde_json::{Value, json};

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
            "bus.status" => Ok(json!({
                "event_count": self.bus.events().len(),
            })),
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
            "bus": {
                "event_count": self.bus.events().len(),
            },
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

        let ticket = self.tickets.hold(intent);
        Ok(json!({
            "ticket": ticket,
            "active_tickets": self.tickets.active_tickets(&workspace_root),
        }))
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

fn workspace_root(params: &serde_json::Map<String, Value>) -> String {
    params
        .get("workspace_root")
        .or_else(|| params.get("root"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
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

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(value)) => value
            .replace(',', " ")
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
                "params": {"workspace_root": "/repo", "agent_id": "agent-a", "message": "edit server"},
            }),
        );
        handle(
            &mut core,
            json!({
                "id": "t2",
                "method": "bus.ticket",
                "params": {"workspace_root": "/repo", "agent_id": "agent-b", "message": "edit server"},
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

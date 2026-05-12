use std::time::{SystemTime, UNIX_EPOCH};

use hsp_bus::BusJournal;
use hsp_session::{SessionKey, SessionRegistry};
use hsp_wire::{BrokerErrorCode, BrokerRequest, BrokerResponse, BrokerWireError};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct BrokerCore {
    started_at: f64,
    registry: SessionRegistry,
    bus: BusJournal,
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
}

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerErrorCode {
    UnknownMethod,
    InvalidRequest,
    InvalidParams,
    Transport,
    Internal,
    BrokerUnreachable,
    NotConnected,
    InvalidResponse,
    Lsp(String),
    Other(String),
}

impl BrokerErrorCode {
    pub fn as_wire(&self) -> &str {
        match self {
            Self::UnknownMethod => "unknown_method",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidParams => "invalid_params",
            Self::Transport => "transport",
            Self::Internal => "internal",
            Self::BrokerUnreachable => "broker_unreachable",
            Self::NotConnected => "not_connected",
            Self::InvalidResponse => "invalid_response",
            Self::Lsp(value) | Self::Other(value) => value.as_str(),
        }
    }

    pub fn from_wire(value: &str) -> Self {
        match value {
            "unknown_method" => Self::UnknownMethod,
            "invalid_request" => Self::InvalidRequest,
            "invalid_params" => Self::InvalidParams,
            "transport" => Self::Transport,
            "internal" => Self::Internal,
            "broker_unreachable" => Self::BrokerUnreachable,
            "not_connected" => Self::NotConnected,
            "invalid_response" => Self::InvalidResponse,
            code if code.starts_with("lsp:") => Self::Lsp(code.to_string()),
            other => Self::Other(other.to_string()),
        }
    }
}

impl Display for BrokerErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerWireError {
    pub code: String,
    pub message: String,
}

impl BrokerWireError {
    pub fn new(code: BrokerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.as_wire().to_string(),
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(BrokerErrorCode::InvalidRequest, message)
    }

    pub fn kind(&self) -> BrokerErrorCode {
        BrokerErrorCode::from_wire(&self.code)
    }
}

impl Display for BrokerWireError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BrokerWireError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerRequest {
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Map<String, Value>,
}

impl BrokerRequest {
    pub fn from_value(value: Value) -> Result<Self, BrokerWireError> {
        let mut object = match value {
            Value::Object(object) => object,
            _ => {
                return Err(BrokerWireError::invalid_request(
                    "frame must be a JSON object",
                ));
            }
        };

        let id = object.remove("id").unwrap_or(Value::Null);
        let method = object
            .remove("method")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .ok_or_else(|| BrokerWireError::invalid_request("missing method"))?;
        let params = match object.remove("params").unwrap_or(Value::Object(Map::new())) {
            Value::Object(params) => params,
            _ => return Err(BrokerWireError::invalid_request("params must be an object")),
        };

        Ok(Self { id, method, params })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerResponse {
    #[serde(default)]
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BrokerWireError>,
}

impl BrokerResponse {
    pub fn result(id: Value, result: Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, error: BrokerWireError) -> Self {
        Self {
            id,
            result: None,
            error: Some(error),
        }
    }
}

pub fn encode_message(message: &Value) -> Result<Vec<u8>, BrokerWireError> {
    if !message.is_object() {
        return Err(BrokerWireError::invalid_request(
            "frame must be a JSON object",
        ));
    }
    let mut encoded = serde_json::to_vec(message)
        .map_err(|error| BrokerWireError::invalid_request(error.to_string()))?;
    encoded.push(b'\n');
    Ok(encoded)
}

pub fn decode_message(bytes: &[u8]) -> Result<Value, BrokerWireError> {
    let text = String::from_utf8_lossy(bytes);
    decode_message_str(&text)
}

pub fn decode_message_str(text: &str) -> Result<Value, BrokerWireError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(BrokerWireError::invalid_request("empty frame"));
    }

    let value: Value = serde_json::from_str(text)
        .map_err(|error| BrokerWireError::invalid_request(format!("malformed json: {error}")))?;
    if !value.is_object() {
        return Err(BrokerWireError::invalid_request(
            "frame must be a JSON object",
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn roundtrip_preserves_payload() {
        let message = json!({
            "id": "c1",
            "method": "ping",
            "params": {"a": 1, "b": [1, 2]},
        });

        let wire = encode_message(&message).expect("encode");
        assert!(wire.ends_with(b"\n"));
        assert_eq!(decode_message(&wire).expect("decode"), message);
    }

    #[test]
    fn decode_accepts_string_input() {
        let message = json!({"id": 1, "method": "status"});
        let wire = String::from_utf8(encode_message(&message).expect("encode")).expect("utf8");
        assert_eq!(decode_message_str(&wire).expect("decode"), message);
    }

    #[test]
    fn decode_rejects_invalid_frames() {
        assert_eq!(
            decode_message(b"not json\n").expect_err("non-json").kind(),
            BrokerErrorCode::InvalidRequest
        );
        assert_eq!(
            decode_message(b"\n").expect_err("empty").kind(),
            BrokerErrorCode::InvalidRequest
        );
        assert_eq!(
            decode_message(b"[1,2,3]\n").expect_err("array").kind(),
            BrokerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn encode_is_deterministic() {
        let a = encode_message(&json!({"b": 2, "a": 1})).expect("encode a");
        let b = encode_message(&json!({"a": 1, "b": 2})).expect("encode b");
        assert_eq!(a, b);
    }

    #[test]
    fn request_extracts_id_method_and_params() {
        let request = BrokerRequest::from_value(json!({
            "id": "c1",
            "method": "session.get_or_create",
            "params": {"root": "/repo", "config_hash": "abc"},
        }))
        .expect("request");

        assert_eq!(request.id, json!("c1"));
        assert_eq!(request.method, "session.get_or_create");
        assert_eq!(request.params["root"], json!("/repo"));
    }

    #[test]
    fn request_rejects_missing_method_and_non_object_params() {
        assert_eq!(
            BrokerRequest::from_value(json!({"id": "c1"}))
                .expect_err("missing method")
                .kind(),
            BrokerErrorCode::InvalidRequest
        );
        assert_eq!(
            BrokerRequest::from_value(json!({"method": "ping", "params": [1, 2]}))
                .expect_err("bad params")
                .kind(),
            BrokerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn response_serializes_result_or_error_shape() {
        let response = BrokerResponse::result(json!("c1"), json!({"pong": true}));
        let json = serde_json::to_value(response).expect("response json");
        assert_eq!(json, json!({"id": "c1", "result": {"pong": true}}));

        let response = BrokerResponse::error(
            json!("c2"),
            BrokerWireError::new(BrokerErrorCode::UnknownMethod, "unknown method: nope"),
        );
        let json = serde_json::to_value(response).expect("error json");
        assert_eq!(
            json,
            json!({
                "id": "c2",
                "error": {"code": "unknown_method", "message": "unknown method: nope"},
            })
        );
    }
}

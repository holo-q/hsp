use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hsp_wire::{
    BrokerErrorCode, BrokerResponse, BrokerWireError, decode_message_str, encode_message,
};
use serde_json::{Map, Value, json};

#[derive(Debug)]
pub struct BrokerClient {
    path: PathBuf,
    stream: Option<UnixStream>,
    next_id: u64,
}

impl BrokerClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            stream: None,
            next_id: 1,
        }
    }

    pub fn from_default_path() -> Self {
        Self::new(hsp_protocol::socket_path())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connect(&mut self) -> Result<(), BrokerWireError> {
        let stream = UnixStream::connect(&self.path).map_err(transport_error)?;
        self.stream = Some(stream);
        Ok(())
    }

    pub fn connect_with_retry(&mut self, timeout: Duration) -> Result<(), BrokerWireError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.connect() {
                Ok(()) => return Ok(()),
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn connect_or_start(
        &mut self,
        connect_timeout: Duration,
        start_timeout: Duration,
    ) -> Result<Option<Child>, BrokerWireError> {
        if self.connect_with_retry(connect_timeout).is_ok() {
            return Ok(None);
        }

        let child = start_broker_subprocess()?;
        self.connect_with_retry(start_timeout).map_err(|error| {
            BrokerWireError::new(
                BrokerErrorCode::BrokerUnreachable,
                format!("broker failed to become reachable after start: {error}"),
            )
        })?;
        Ok(Some(child))
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    pub fn request(
        &mut self,
        method: &str,
        params: Map<String, Value>,
    ) -> Result<Value, BrokerWireError> {
        let id = format!("c{}", self.next_id);
        self.next_id += 1;
        let request = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.request_value(request)?;
        response.result.ok_or_else(|| {
            response.error.unwrap_or_else(|| {
                BrokerWireError::new(
                    BrokerErrorCode::InvalidResponse,
                    "broker response contained neither result nor error",
                )
            })
        })
    }

    pub fn request_value(&mut self, request: Value) -> Result<BrokerResponse, BrokerWireError> {
        let stream = self.stream.as_mut().ok_or_else(|| {
            BrokerWireError::new(BrokerErrorCode::NotConnected, "broker client is not connected")
        })?;
        let encoded = encode_message(&request)?;
        stream.write_all(&encoded).map_err(transport_error)?;
        stream.flush().map_err(transport_error)?;

        let reader_stream = stream.try_clone().map_err(transport_error)?;
        let mut reader = BufReader::new(reader_stream);
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(transport_error)?;
        if read == 0 {
            return Err(BrokerWireError::new(
                BrokerErrorCode::Transport,
                "broker closed connection without a response",
            ));
        }
        let value = decode_message_str(&line)?;
        let response: BrokerResponse = serde_json::from_value(value).map_err(|error| {
            BrokerWireError::new(BrokerErrorCode::InvalidResponse, error.to_string())
        })?;
        if let Some(error) = response.error.clone() {
            return Err(error);
        }
        Ok(response)
    }
}

impl Drop for BrokerClient {
    fn drop(&mut self) {
        self.stream.take();
    }
}

fn transport_error(error: std::io::Error) -> BrokerWireError {
    BrokerWireError::new(BrokerErrorCode::Transport, error.to_string())
}

pub fn start_broker_subprocess() -> Result<Child, BrokerWireError> {
    let log_path = hsp_protocol::broker_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(transport_error)?;
    }
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(transport_error)?;
    let stderr = stdout.try_clone().map_err(transport_error)?;
    let mut command = Command::new("hsp-broker");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    command.spawn().map_err(|error| {
        BrokerWireError::new(
            BrokerErrorCode::BrokerUnreachable,
            format!("failed to start hsp-broker: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn disconnected_request_returns_not_connected() {
        let mut client = BrokerClient::new("/no/such/socket");
        let error = client
            .request("ping", Map::new())
            .expect_err("not connected");

        assert_eq!(error.kind(), BrokerErrorCode::NotConnected);
    }

    #[test]
    fn request_ids_are_monotonic_json_strings() {
        let mut client = BrokerClient::new("/no/such/socket");
        client.next_id = 41;

        let request = json!({
            "id": format!("c{}", client.next_id),
            "method": "ping",
            "params": {},
        });
        client.next_id += 1;

        assert_eq!(request["id"], json!("c41"));
        assert_eq!(client.next_id, 42);
    }
}

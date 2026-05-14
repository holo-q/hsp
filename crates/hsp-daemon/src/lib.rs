use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use hsp_broker::BrokerCore;
use hsp_store::{BrokerMode, WorkspaceStore};
use hsp_wire::{BrokerErrorCode, BrokerResponse, BrokerWireError, decode_message_str, encode_message};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOptions {
    pub socket_path: PathBuf,
    pub poll_interval: Duration,
}

impl ServeOptions {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            poll_interval: Duration::from_millis(25),
        }
    }

    pub fn from_default_path() -> Self {
        Self::new(hsp_protocol::socket_path())
    }
}

pub fn serve_default() -> Result<(), BrokerWireError> {
    serve_unix(
        ServeOptions::from_default_path(),
        BrokerCore::with_store(WorkspaceStore::new(BrokerMode::Broker)),
    )
}

pub fn serve_unix(options: ServeOptions, mut core: BrokerCore) -> Result<(), BrokerWireError> {
    prepare_socket_path(&options.socket_path)?;
    let listener = UnixListener::bind(&options.socket_path).map_err(transport_error)?;
    listener.set_nonblocking(true).map_err(transport_error)?;

    while !core.is_shutting_down() {
        match listener.accept() {
            Ok((stream, _addr)) => handle_connection(&mut core, stream)?,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(options.poll_interval);
            }
            Err(error) => return Err(transport_error(error)),
        }
    }

    let _ = std::fs::remove_file(&options.socket_path);
    Ok(())
}

fn handle_connection(core: &mut BrokerCore, stream: UnixStream) -> Result<(), BrokerWireError> {
    let mut writer = stream.try_clone().map_err(transport_error)?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(transport_error)?;
        if read == 0 {
            return Ok(());
        }

        let response = match decode_message_str(&line) {
            Ok(value) => core.handle_value(value),
            Err(error) => BrokerResponse::error(Value::Null, error),
        };
        let response_value = serde_json::to_value(response).map_err(internal_error)?;
        let encoded = encode_message(&response_value)?;
        writer.write_all(&encoded).map_err(transport_error)?;
        writer.flush().map_err(transport_error)?;

        if core.is_shutting_down() {
            return Ok(());
        }
    }
}

fn prepare_socket_path(path: &Path) -> Result<(), BrokerWireError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(transport_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    if path.exists() {
        std::fs::remove_file(path).map_err(transport_error)?;
    }
    Ok(())
}

fn transport_error(error: std::io::Error) -> BrokerWireError {
    BrokerWireError::new(BrokerErrorCode::Transport, error.to_string())
}

fn internal_error(error: serde_json::Error) -> BrokerWireError {
    BrokerWireError::new(BrokerErrorCode::Internal, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use hsp_client::BrokerClient;
    use serde_json::{Map, json};

    use super::*;

    fn socket_path(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::current_dir()
            .expect("current dir")
            .join("target")
            .join(format!("hsp-{name}-{}-{stamp}.sock", std::process::id()))
    }

    fn start_server(path: PathBuf) -> thread::JoinHandle<Result<(), BrokerWireError>> {
        thread::spawn(move || serve_unix(ServeOptions::new(path), BrokerCore::ephemeral()))
    }

    fn params(items: &[(&str, &str)]) -> Map<String, serde_json::Value> {
        items
            .iter()
            .map(|(key, value)| (key.to_string(), json!(value)))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect()
    }

    #[test]
    fn ping_status_session_and_shutdown_round_trip_over_socket() {
        let path = socket_path("round-trip");
        let server = start_server(path.clone());
        let mut client = BrokerClient::new(path);
        client
            .connect_with_retry(Duration::from_secs(5))
            .expect("connect");

        assert_eq!(client.request("ping", Map::new()).expect("ping"), json!({"pong": true}));
        let status = client.request("status", Map::new()).expect("status");
        assert_eq!(status["session_count"], json!(0));

        let request = params(&[
            ("root", "/repo-x"),
            ("config_hash", "h1"),
            ("server_label", "ty"),
        ]);
        let first = client
            .request("session.get_or_create", request.clone())
            .expect("first session");
        let second = client
            .request("session.get_or_create", request)
            .expect("second session");
        assert_eq!(first["session_id"], second["session_id"]);

        let status = client.request("status", Map::new()).expect("status");
        assert_eq!(status["session_count"], json!(1));
        assert_eq!(
            client.request("shutdown", Map::new()).expect("shutdown"),
            json!({"shutting_down": true})
        );
        drop(client);

        server
            .join()
            .expect("server thread")
            .expect("server exits cleanly");
    }
}

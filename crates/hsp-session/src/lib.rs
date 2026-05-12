use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub root: String,
    pub config_hash: String,
}

impl SessionKey {
    pub fn new(root: impl Into<String>, config_hash: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            config_hash: config_hash.into(),
        }
    }
}

pub fn config_hash(
    server_label: &str,
    command: &str,
    args: impl IntoIterator<Item = impl AsRef<str>>,
    env: impl IntoIterator<Item = (impl AsRef<str>, impl AsRef<str>)>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(server_label.as_bytes());
    hash.update(b"\0");
    hash.update(command.as_bytes());
    hash.update(b"\0");
    for arg in args {
        hash.update(arg.as_ref().as_bytes());
        hash.update(b"\0");
    }

    let mut env = env
        .into_iter()
        .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
        .collect::<Vec<_>>();
    env.sort_by(|left, right| left.0.cmp(&right.0));
    for (key, value) in env {
        hash.update(key.as_bytes());
        hash.update(b"=");
        hash.update(value.as_bytes());
        hash.update(b"\0");
    }

    let digest = hash.finalize();
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrokerSession {
    pub session_id: String,
    pub key: SessionKey,
    pub server_label: String,
    pub started_at: f64,
    pub last_used_at: f64,
    pub client_count: u64,
}

impl BrokerSession {
    pub fn new(
        session_id: impl Into<String>,
        key: SessionKey,
        server_label: impl Into<String>,
        now: f64,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            key,
            server_label: server_label.into(),
            started_at: now,
            last_used_at: now,
            client_count: 0,
        }
    }

    pub fn touch(&mut self, now: f64) {
        self.last_used_at = now;
    }

    pub fn to_record(&self) -> SessionRecord {
        SessionRecord {
            session_id: self.session_id.clone(),
            root: self.key.root.clone(),
            config_hash: self.key.config_hash.clone(),
            server_label: self.server_label.clone(),
            started_at: self.started_at,
            last_used_at: self.last_used_at,
            client_count: self.client_count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub root: String,
    pub config_hash: String,
    pub server_label: String,
    pub started_at: f64,
    pub last_used_at: f64,
    pub client_count: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SessionRegistry {
    sessions: HashMap<SessionKey, BrokerSession>,
    counter: u64,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(&mut self, key: SessionKey, server_label: impl Into<String>) -> &BrokerSession {
        self.get_or_create_at(key, server_label, now_seconds())
    }

    pub fn get_or_create_at(
        &mut self,
        key: SessionKey,
        server_label: impl Into<String>,
        now: f64,
    ) -> &BrokerSession {
        if self.sessions.contains_key(&key) {
            let session = self.sessions.get_mut(&key).expect("session exists");
            session.touch(now);
            return session;
        }

        self.counter += 1;
        let session_id = format!("s{}", self.counter);
        let session = BrokerSession::new(session_id, key.clone(), server_label, now);
        self.sessions.insert(key.clone(), session);
        self.sessions.get(&key).expect("session was just inserted")
    }

    pub fn get(&self, session_id: &str) -> Option<&BrokerSession> {
        self.sessions
            .values()
            .find(|session| session.session_id == session_id)
    }

    pub fn all_sessions(&self) -> Vec<BrokerSession> {
        self.sessions.values().cloned().collect()
    }

    pub fn stop(&mut self, session_id: &str) -> bool {
        let key = self
            .sessions
            .iter()
            .find_map(|(key, session)| (session.session_id == session_id).then(|| key.clone()));
        if let Some(key) = key {
            self.sessions.remove(&key);
            return true;
        }
        false
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_compare_and_hash_by_root_and_config() {
        let a = SessionKey::new("/repo", "abc");
        let b = SessionKey::new("/repo", "abc");
        let c = SessionKey::new("/repo", "def");
        let mut bag = HashMap::new();
        bag.insert(a.clone(), 1);

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(bag.get(&b), Some(&1));
    }

    #[test]
    fn config_hash_is_stable_short_hex_and_env_sensitive() {
        let a = config_hash("ty", "ty", ["server"], [("PYTHONPATH", "/x")]);
        let b = config_hash("ty", "ty", ["server"], [("PYTHONPATH", "/x")]);
        let different_command = config_hash(
            "ty",
            "basedpyright",
            ["server"],
            std::iter::empty::<(&str, &str)>(),
        );
        let different_env = config_hash("ty", "ty", std::iter::empty::<&str>(), [("FOO", "2")]);

        assert_eq!(a, b);
        assert_ne!(a, different_command);
        assert_ne!(
            config_hash("ty", "ty", std::iter::empty::<&str>(), [("FOO", "1")]),
            different_env
        );
        assert_eq!(a.len(), 12);
        u64::from_str_radix(&a, 16).expect("short hex hash");
    }

    #[test]
    fn get_or_create_reuses_session_for_same_key() {
        let mut registry = SessionRegistry::new();
        let key = SessionKey::new("/repo", "abc");
        let first = registry.get_or_create_at(key.clone(), "ty", 10.0).clone();
        let second = registry.get_or_create_at(key, "ty", 20.0).clone();

        assert_eq!(first.session_id, second.session_id);
        assert_eq!(second.last_used_at, 20.0);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn different_keys_yield_distinct_sessions() {
        let mut registry = SessionRegistry::new();
        let a = registry
            .get_or_create_at(SessionKey::new("/repo-a", "h"), "", 1.0)
            .session_id
            .clone();
        let b = registry
            .get_or_create_at(SessionKey::new("/repo-b", "h"), "", 1.0)
            .session_id
            .clone();
        let c = registry
            .get_or_create_at(SessionKey::new("/repo-b", "h2"), "", 1.0)
            .session_id
            .clone();

        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn stop_removes_session_and_get_finds_by_id() {
        let mut registry = SessionRegistry::new();
        let session_id = registry
            .get_or_create_at(SessionKey::new("/repo", "abc"), "", 1.0)
            .session_id
            .clone();

        assert!(registry.get(&session_id).is_some());
        assert!(registry.stop(&session_id));
        assert!(registry.is_empty());
        assert!(!registry.stop(&session_id));
    }

    #[test]
    fn all_sessions_returns_snapshot() {
        let mut registry = SessionRegistry::new();
        registry.get_or_create_at(SessionKey::new("/a", "x"), "", 1.0);
        registry.get_or_create_at(SessionKey::new("/b", "x"), "", 1.0);
        let mut sessions = registry.all_sessions();
        sessions.clear();

        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn session_record_matches_broker_wire_shape() {
        let session = BrokerSession::new("s1", SessionKey::new("/repo", "abc"), "ty", 10.0);
        let value = serde_json::to_value(session.to_record()).expect("record json");

        assert_eq!(
            value,
            serde_json::json!({
                "session_id": "s1",
                "root": "/repo",
                "config_hash": "abc",
                "server_label": "ty",
                "started_at": 10.0,
                "last_used_at": 10.0,
                "client_count": 0,
            })
        );
    }
}

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use hsp_wire::BusEvent;
use sha2::{Digest, Sha256};

pub const WORKSPACE_ID_LENGTH: usize = 12;
pub const LOG_FILE_NAME: &str = "events.jsonl";
pub const BUS_DIR_ENV: &str = "HSP_BUS_DIR";
pub const DIRECT_TMP_DIR: &str = "tmp/hsp-bus";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerMode {
    Direct,
    Broker,
}

pub fn workspace_id_for(root: impl AsRef<Path>) -> std::io::Result<String> {
    let absolute = absolute_path(root)?;
    let mut hasher = Sha256::new();
    hasher.update(absolute.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    Ok(hex[..WORKSPACE_ID_LENGTH].to_string())
}

pub fn bus_dir_for(root: impl AsRef<Path>, mode: BrokerMode) -> std::io::Result<PathBuf> {
    let root = absolute_path(root)?;
    let workspace_id = workspace_id_for(&root)?;
    if let Some(override_dir) = env_path(BUS_DIR_ENV) {
        return Ok(override_dir.join(workspace_id));
    }
    if mode == BrokerMode::Broker {
        return Ok(state_home().join("hsp").join("bus").join(workspace_id));
    }
    Ok(root.join(DIRECT_TMP_DIR))
}

pub fn log_path_for(root: impl AsRef<Path>, mode: BrokerMode) -> std::io::Result<PathBuf> {
    Ok(bus_dir_for(root, mode)?.join(LOG_FILE_NAME))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusLog {
    path: PathBuf,
}

impl BusLog {
    pub fn new(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, event: &BusEvent) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }

    pub fn replay(&self) -> std::io::Result<Vec<BusEvent>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for line in reader.lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<BusEvent>(line) {
                events.push(event);
            }
        }

        Ok(events)
    }

    pub fn tail(&self, after_seq: u64) -> std::io::Result<Vec<BusEvent>> {
        Ok(self
            .replay()?
            .into_iter()
            .filter(|event| event.seq > after_seq)
            .collect())
    }

    pub fn next_seq(&self) -> std::io::Result<u64> {
        Ok(self
            .replay()?
            .into_iter()
            .map(|event| event.seq)
            .max()
            .unwrap_or(0)
            + 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceStore {
    mode: BrokerMode,
}

impl WorkspaceStore {
    pub fn new(mode: BrokerMode) -> Self {
        Self { mode }
    }

    pub fn mode(&self) -> BrokerMode {
        self.mode
    }

    pub fn log_for(&self, workspace_root: impl AsRef<Path>) -> std::io::Result<BusLog> {
        BusLog::new(log_path_for(workspace_root, self.mode)?)
    }

    pub fn replay(&self, workspace_root: impl AsRef<Path>) -> std::io::Result<Vec<BusEvent>> {
        self.log_for(workspace_root)?.replay()
    }

    pub fn append(
        &self,
        workspace_root: impl AsRef<Path>,
        event: &BusEvent,
    ) -> std::io::Result<()> {
        self.log_for(workspace_root)?.append(event)
    }
}

fn absolute_path(root: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let root = root.as_ref();
    let path = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    Ok(path)
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn state_home() -> PathBuf {
    env_path("XDG_STATE_HOME")
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use hsp_wire::{BusEvent, BusEventKind, BusScope, SCHEMA_VERSION};

    use super::*;

    fn test_log_path(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let dir = std::env::current_dir()
            .expect("current dir")
            .join("target/hsp-store-tests")
            .join(format!("{name}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test dir");
        dir.join("events.jsonl")
    }

    fn event(seq: u64, message: &str) -> BusEvent {
        BusEvent {
            seq,
            event_id: format!("E{seq}"),
            kind: BusEventKind::NotePosted,
            timestamp: 1000.0 + seq as f64,
            workspace_id: "wsid".to_string(),
            workspace_root: "/repo".to_string(),
            agent_id: "noesis".to_string(),
            client_id: format!("cli-{seq}"),
            session_id: "sess".to_string(),
            task_id: String::new(),
            git_head: String::new(),
            dirty_hash: String::new(),
            scope: BusScope::parse(&format!("src/{seq}.py"), "", ""),
            message: message.to_string(),
            metadata: BTreeMap::new(),
            question_id: String::new(),
            truncated: false,
            schema_version: SCHEMA_VERSION,
        }
    }

    #[test]
    fn workspace_id_is_sha256_prefix_of_absolute_root() {
        assert_eq!(workspace_id_for("/repo").expect("workspace id"), "816fc349d3fa");
    }

    #[test]
    fn bus_dir_policy_supports_direct_broker_and_override() {
        let prior_override = std::env::var_os(BUS_DIR_ENV);
        let prior_state = std::env::var_os("XDG_STATE_HOME");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let base = std::env::current_dir()
            .expect("current dir")
            .join("target/hsp-store-tests")
            .join(format!("paths-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&base).expect("test dir");
        let root = base.join("repo");
        std::fs::create_dir_all(&root).expect("root dir");
        let override_dir = base.join("override");
        let state_dir = base.join("state");

        unsafe {
            std::env::remove_var(BUS_DIR_ENV);
            std::env::set_var("XDG_STATE_HOME", &state_dir);
        }
        assert_eq!(
            bus_dir_for(&root, BrokerMode::Direct).expect("direct dir"),
            root.join(DIRECT_TMP_DIR)
        );
        let broker = bus_dir_for(&root, BrokerMode::Broker).expect("broker dir");
        assert!(broker.starts_with(state_dir.join("hsp/bus")));

        unsafe {
            std::env::set_var(BUS_DIR_ENV, &override_dir);
        }
        assert!(bus_dir_for(&root, BrokerMode::Broker)
            .expect("override dir")
            .starts_with(&override_dir));

        unsafe {
            match prior_override {
                Some(value) => std::env::set_var(BUS_DIR_ENV, value),
                None => std::env::remove_var(BUS_DIR_ENV),
            }
            match prior_state {
                Some(value) => std::env::set_var("XDG_STATE_HOME", value),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }

    #[test]
    fn empty_log_replay_is_empty() {
        let log = BusLog::new(test_log_path("empty")).expect("log");

        assert_eq!(log.replay().expect("replay"), Vec::<BusEvent>::new());
        assert_eq!(log.next_seq().expect("next seq"), 1);
    }

    #[test]
    fn append_then_replay_round_trips() {
        let log = BusLog::new(test_log_path("round-trip")).expect("log");
        let first = event(1, "first");
        let second = event(2, "second");

        log.append(&first).expect("append first");
        log.append(&second).expect("append second");

        assert_eq!(log.replay().expect("replay"), vec![first, second]);
    }

    #[test]
    fn workspace_store_wraps_mode_and_bus_log_policy() {
        let base = std::env::current_dir()
            .expect("current dir")
            .join("target/hsp-store-tests")
            .join(format!("workspace-store-{}", std::process::id()));
        let root = base.join("repo");
        std::fs::create_dir_all(&root).expect("root dir");

        let store = WorkspaceStore::new(BrokerMode::Direct);
        store.append(&root, &event(1, "stored")).expect("append");
        let replayed = store.replay(&root).expect("replay");
        assert_eq!(store.mode(), BrokerMode::Direct);
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].message, "stored");
    }

    #[test]
    fn next_seq_reads_disk_state() {
        let path = test_log_path("next-seq");
        let writer = BusLog::new(path.clone()).expect("writer");
        writer.append(&event(1, "x")).expect("append first");
        writer.append(&event(7, "x")).expect("append gap");

        let reader = BusLog::new(path).expect("reader");
        assert_eq!(reader.next_seq().expect("next seq"), 8);
    }

    #[test]
    fn tail_filters_by_after_seq() {
        let log = BusLog::new(test_log_path("tail")).expect("log");
        for seq in 1..=4 {
            log.append(&event(seq, "x")).expect("append");
        }

        let tail = log.tail(2).expect("tail");
        assert_eq!(
            tail.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn replay_skips_malformed_lines() {
        let log = BusLog::new(test_log_path("malformed")).expect("log");
        log.append(&event(1, "x")).expect("append first");

        let mut file = OpenOptions::new()
            .append(true)
            .open(log.path())
            .expect("open log");
        file.write_all(b"{not valid json\n\n")
            .expect("write malformed json");
        file.write_all(br#"{"kind":"not.a.kind","seq":99}"#)
            .expect("write unknown kind");
        file.write_all(b"\n").expect("write newline");
        drop(file);

        log.append(&event(2, "x")).expect("append second");

        let replayed = log.replay().expect("replay");
        assert_eq!(
            replayed.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}

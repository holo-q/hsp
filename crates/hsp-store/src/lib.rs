use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use hsp_wire::BusEvent;

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

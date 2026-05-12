use std::collections::BTreeMap;

use hsp_wire::{BusEvent, BusEventKind, BusScope, MAX_MESSAGE_BYTES, truncate_message};

#[derive(Debug, Clone, PartialEq)]
pub struct JournalAppend {
    pub kind: BusEventKind,
    pub timestamp: f64,
    pub workspace_id: String,
    pub workspace_root: String,
    pub agent_id: String,
    pub client_id: String,
    pub session_id: String,
    pub task_id: String,
    pub git_head: String,
    pub dirty_hash: String,
    pub scope: BusScope,
    pub message: String,
    pub metadata: BTreeMap<String, String>,
    pub question_id: String,
}

impl JournalAppend {
    pub fn new(kind: BusEventKind) -> Self {
        Self {
            kind,
            timestamp: 0.0,
            workspace_id: String::new(),
            workspace_root: String::new(),
            agent_id: String::new(),
            client_id: String::new(),
            session_id: String::new(),
            task_id: String::new(),
            git_head: String::new(),
            dirty_hash: String::new(),
            scope: BusScope::empty(),
            message: String::new(),
            metadata: BTreeMap::new(),
            question_id: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BusJournal {
    events: Vec<BusEvent>,
}

impl BusJournal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, append: JournalAppend) -> &BusEvent {
        let seq = self.events.len() as u64 + 1;
        let clipped = truncate_message(&append.message, MAX_MESSAGE_BYTES);

        self.events.push(BusEvent {
            seq,
            event_id: format!("E{seq}"),
            kind: append.kind,
            timestamp: append.timestamp,
            workspace_id: append.workspace_id,
            workspace_root: append.workspace_root,
            agent_id: append.agent_id,
            client_id: append.client_id,
            session_id: append.session_id,
            task_id: append.task_id,
            git_head: append.git_head,
            dirty_hash: append.dirty_hash,
            scope: append.scope,
            message: clipped.message,
            metadata: append.metadata,
            question_id: append.question_id,
            truncated: clipped.truncated,
            schema_version: hsp_wire::SCHEMA_VERSION,
        });

        self.events.last().expect("event was just pushed")
    }

    pub fn events(&self) -> &[BusEvent] {
        &self.events
    }

    pub fn recent_for_scope(&self, scope: &BusScope, limit: usize) -> Vec<&BusEvent> {
        self.events
            .iter()
            .rev()
            .filter(|event| event.scope.overlaps(scope))
            .take(limit)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_assigns_event_handles_without_confusing_seq() {
        let mut journal = BusJournal::new();
        let first = journal.append(JournalAppend::new(BusEventKind::NotePosted));
        assert_eq!(first.seq, 1);
        assert_eq!(first.event_id, "E1");

        let second = journal.append(JournalAppend::new(BusEventKind::NotePosted));
        assert_eq!(second.seq, 2);
        assert_eq!(second.event_id, "E2");
    }

    #[test]
    fn append_truncates_message_at_wire_limit() {
        let mut append = JournalAppend::new(BusEventKind::NotePosted);
        append.message = "a".repeat(MAX_MESSAGE_BYTES + 1);

        let mut journal = BusJournal::new();
        let event = journal.append(append);

        assert!(event.truncated);
        assert_eq!(event.message.len(), MAX_MESSAGE_BYTES);
    }

    #[test]
    fn recent_filters_by_scope_overlap() {
        let mut journal = BusJournal::new();
        let mut server = JournalAppend::new(BusEventKind::NotePosted);
        server.scope = BusScope::parse("src/hsp/server.py", "", "");
        journal.append(server);

        let mut client = JournalAppend::new(BusEventKind::NotePosted);
        client.scope = BusScope::parse("src/hsp/client.py", "", "");
        journal.append(client);

        let recent = journal.recent_for_scope(&BusScope::parse("server.py", "", ""), 10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].seq, 1);
    }
}

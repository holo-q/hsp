mod presence;
mod question;
mod ticket;

use std::collections::BTreeMap;

use hsp_wire::{BusEvent, BusEventKind, BusScope, MAX_MESSAGE_BYTES, truncate_message};
use serde::Serialize;

pub use presence::{
    ACTIVE_WINDOW_SECONDS, PIN_PROMPT_THRESHOLD, PRUNE_WINDOW_SECONDS, PresenceEntry,
    PresenceEntryWire, PresenceStatus, PresenceTracker,
};
pub use question::{BusQuestion, BusQuestionWire, QuestionOpen};
pub use ticket::{
    BuildGate, EditGate, EditGateMode, Ticket, TicketBoard, TicketEffect, TicketEffectKind,
    TicketHold, TicketIntent,
};

pub const DEFAULT_RECENT_LIMIT: usize = 20;
pub const DEFAULT_JOURNAL_LIMIT: usize = 25;

#[derive(Debug, Clone, Serialize)]
pub struct BusEventWire<'a> {
    #[serde(flatten)]
    pub event: &'a BusEvent,
    pub event_type: &'static str,
    pub files: &'a [String],
    pub symbols: &'a [String],
    pub aliases: &'a [String],
}

impl<'a> From<&'a BusEvent> for BusEventWire<'a> {
    fn from(event: &'a BusEvent) -> Self {
        Self {
            event,
            event_type: event.kind.as_wire(),
            files: &event.scope.files,
            symbols: &event.scope.symbols,
            aliases: &event.scope.aliases,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuery {
    pub workspace_root: String,
    pub scope: BusScope,
    pub after_seq: u64,
    pub limit: usize,
}

impl EventQuery {
    pub fn new(workspace_root: impl Into<String>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            scope: BusScope::empty(),
            after_seq: 0,
            limit: DEFAULT_RECENT_LIMIT,
        }
    }
}

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

#[derive(Debug, Clone)]
pub struct BusJournal {
    events: Vec<BusEvent>,
    questions: BTreeMap<String, BusQuestion>,
    presence: PresenceTracker,
    next_event_seq: u64,
    next_question_id: u64,
}

impl Default for BusJournal {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            questions: BTreeMap::new(),
            presence: PresenceTracker::new(),
            next_event_seq: 1,
            next_question_id: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestionClose {
    pub question: BusQuestion,
    pub close_event: BusEvent,
    pub events: Vec<BusEvent>,
    pub replies: Vec<BusEvent>,
}

impl BusJournal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_events(events: impl IntoIterator<Item = BusEvent>) -> Self {
        let mut journal = Self::new();
        for event in events {
            journal.absorb_event(event);
        }
        journal
    }

    pub fn append(&mut self, append: JournalAppend) -> &BusEvent {
        let seq = self.next_event_seq;
        self.next_event_seq += 1;
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

        let event = self.events.last().expect("event was just pushed");
        self.presence.observe(event);
        event
    }

    pub fn heartbeat(&mut self, append: JournalAppend) -> Option<PresenceEntry> {
        let clipped = truncate_message(&append.message, MAX_MESSAGE_BYTES);
        let event = BusEvent {
            seq: 0,
            event_id: "heartbeat".to_string(),
            kind: BusEventKind::AgentHeartbeat,
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
        };
        self.presence.observe(&event)
    }

    pub fn events(&self) -> &[BusEvent] {
        &self.events
    }

    pub fn last_event_id(&self) -> &str {
        self.events
            .last()
            .map(|event| event.event_id.as_str())
            .unwrap_or("")
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn events_for_workspace(&self, workspace_root: &str, limit: usize) -> Vec<&BusEvent> {
        let limit = limit.max(1);
        let matching = self
            .events
            .iter()
            .filter(|event| event.workspace_root == workspace_root)
            .collect::<Vec<_>>();
        matching
            .into_iter()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn recent(&self, query: &EventQuery) -> Vec<&BusEvent> {
        let limit = query.limit.max(1);
        let matching = self
            .events
            .iter()
            .filter(|event| {
                event.seq > query.after_seq
                    && event.workspace_root == query.workspace_root
                    && event.scope.overlaps(&query.scope)
            })
            .collect::<Vec<_>>();
        matching
            .into_iter()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn recent_all(&self, after_seq: u64, limit: usize) -> Vec<&BusEvent> {
        tail(
            self.events
                .iter()
                .filter(|event| event.seq > after_seq)
                .collect(),
            limit,
        )
    }

    pub fn recent_under_roots(
        &self,
        roots: &[String],
        after_seq: u64,
        limit: usize,
    ) -> Vec<&BusEvent> {
        tail(
            self.events
                .iter()
                .filter(|event| {
                    event.seq > after_seq
                        && roots
                            .iter()
                            .any(|root| same_or_descendant(&event.workspace_root, root))
                })
                .collect(),
            limit,
        )
    }

    pub fn recent_is_truncated(&self, query: &EventQuery, selected_count: usize) -> bool {
        self.events
            .iter()
            .filter(|event| {
                event.seq > query.after_seq
                    && event.workspace_root == query.workspace_root
                    && event.scope.overlaps(&query.scope)
            })
            .count()
            > selected_count
    }

    pub fn recent_for_scope(&self, scope: &BusScope, limit: usize) -> Vec<&BusEvent> {
        self.events
            .iter()
            .rev()
            .filter(|event| event.scope.overlaps(scope))
            .take(limit)
            .collect()
    }

    pub fn ask(
        &mut self,
        mut append: JournalAppend,
        open: QuestionOpen,
    ) -> (BusEvent, BusQuestion) {
        let question_id = format!("Q{}", self.next_question_id);
        self.next_question_id += 1;
        append.kind = BusEventKind::BusAsk;
        append.question_id = question_id.clone();
        append.metadata.insert(
            "timeout_seconds".to_string(),
            open.timeout_seconds.max(0.0).to_string(),
        );
        let event = self.append(append).clone();
        let question = BusQuestion {
            question_id: question_id.clone(),
            opened_event_id: event.event_id.clone(),
            opened_at: event.timestamp,
            expires_at: event.timestamp + open.timeout_seconds.max(0.0),
            workspace_root: event.workspace_root.clone(),
            agent_id: event.agent_id.clone(),
            scope: event.scope.clone(),
            message: event.message.clone(),
            closed_at: open.close_immediately.then_some(event.timestamp),
            replies: Vec::new(),
        };
        self.questions.insert(question_id, question.clone());
        (event, question)
    }

    pub fn reply(
        &mut self,
        question_id: &str,
        mut append: JournalAppend,
        close_question: bool,
    ) -> Result<(BusEvent, BusQuestion), UnknownQuestion> {
        let (question_scope, was_closed) = self
            .questions
            .get(question_id)
            .map(|question| (question.scope.clone(), question.closed_at.is_some()))
            .ok_or_else(|| UnknownQuestion::new(question_id))?;
        append.kind = BusEventKind::BusReply;
        append.question_id = question_id.to_string();
        if append.scope.is_empty() {
            append.scope = question_scope;
        }
        if was_closed {
            append
                .metadata
                .entry("late".to_string())
                .or_insert_with(|| "true".to_string());
        }
        let event = self.append(append).clone();
        let question = self
            .questions
            .get_mut(question_id)
            .expect("question existed before reply append");
        question.replies.push(event.seq);
        if close_question {
            question.closed_at = Some(event.timestamp);
        }
        Ok((event, question.clone()))
    }

    pub fn settle(
        &mut self,
        workspace_root: &str,
        now: f64,
        append_base: &JournalAppend,
    ) -> Vec<QuestionClose> {
        let expired = self
            .questions
            .values()
            .filter(|question| {
                question.workspace_root == workspace_root
                    && question.is_open()
                    && question.expires_at <= now
            })
            .cloned()
            .collect::<Vec<_>>();

        let mut closed = Vec::new();
        for question in expired {
            let events = self.related_events(&question, now);
            let replies = events
                .iter()
                .filter(|event| event.kind == BusEventKind::BusReply)
                .cloned()
                .collect::<Vec<_>>();
            let mut append = append_base.clone();
            append.kind = BusEventKind::BusClosed;
            append.timestamp = now;
            append.workspace_root = question.workspace_root.clone();
            append.workspace_id = self
                .events
                .iter()
                .find(|event| event.event_id == question.opened_event_id)
                .map(|event| event.workspace_id.clone())
                .unwrap_or_default();
            append.agent_id = question.agent_id.clone();
            append.scope = question.scope.clone();
            append.question_id = question.question_id.clone();
            append.message = digest_message(&question, replies.len(), events.len());
            append
                .metadata
                .insert("question_id".to_string(), question.question_id.clone());
            append
                .metadata
                .insert("reply_count".to_string(), replies.len().to_string());
            append
                .metadata
                .insert("related_count".to_string(), events.len().to_string());
            append
                .metadata
                .insert("opener_event_id".to_string(), question.opened_event_id.clone());
            let close_event = self.append(append).clone();
            let updated = self
                .questions
                .get_mut(&question.question_id)
                .expect("expired question still exists");
            updated.closed_at = Some(close_event.timestamp);
            closed.push(QuestionClose {
                question: updated.clone(),
                close_event,
                events,
                replies,
            });
        }
        closed
    }

    pub fn question(&self, question_id: &str) -> Option<&BusQuestion> {
        self.questions.get(question_id)
    }

    pub fn replies_for_question(&self, question_id: &str) -> Vec<&BusEvent> {
        self.events
            .iter()
            .filter(|event| {
                event.question_id == question_id && event.kind == BusEventKind::BusReply
            })
            .collect()
    }

    pub fn open_question_count(&self) -> usize {
        self.questions
            .values()
            .filter(|question| question.is_open())
            .count()
    }

    pub fn open_questions(&self) -> Vec<&BusQuestion> {
        self.questions
            .values()
            .filter(|question| question.is_open())
            .collect()
    }

    pub fn open_questions_for_workspace(
        &self,
        workspace_root: &str,
    ) -> Vec<&BusQuestion> {
        self.questions
            .values()
            .filter(|question| question.workspace_root == workspace_root && question.is_open())
            .collect()
    }

    pub fn observe_presence(&mut self, event: &BusEvent) -> Option<PresenceEntry> {
        self.presence.observe(event)
    }

    pub fn visible_presence(&self, now: f64) -> Vec<PresenceEntry> {
        self.presence.visible(now)
    }

    pub fn presence_snapshot_for_workspace(
        &self,
        workspace_root: &str,
        now: f64,
    ) -> Vec<PresenceEntry> {
        self.presence
            .snapshot(now)
            .into_iter()
            .filter(|entry| entry.workspace_root == workspace_root)
            .collect()
    }

    pub fn visible_presence_for_workspace(
        &self,
        workspace_root: &str,
        now: f64,
    ) -> Vec<PresenceEntry> {
        self.visible_presence(now)
            .into_iter()
            .filter(|entry| entry.workspace_root == workspace_root)
            .collect()
    }

    fn related_events(&self, question: &BusQuestion, now: f64) -> Vec<BusEvent> {
        self.events
            .iter()
            .filter(|event| {
                event.workspace_root == question.workspace_root
                    && event.timestamp >= question.opened_at
                    && event.timestamp <= now
                    && (event.question_id == question.question_id
                        || event.scope.overlaps(&question.scope))
            })
            .cloned()
            .collect()
    }

    pub fn absorb_event(&mut self, event: BusEvent) {
        self.next_event_seq = self.next_event_seq.max(event.seq + 1);
        self.track_question_id(&event.question_id);
        match event.kind {
            BusEventKind::BusAsk if !event.question_id.is_empty() => {
                self.questions.insert(
                    event.question_id.clone(),
                    BusQuestion {
                        question_id: event.question_id.clone(),
                        opened_event_id: event.event_id.clone(),
                        opened_at: event.timestamp,
                        expires_at: event.timestamp + timeout_seconds_from(&event),
                        workspace_root: event.workspace_root.clone(),
                        agent_id: event.agent_id.clone(),
                        scope: event.scope.clone(),
                        message: event.message.clone(),
                        closed_at: None,
                        replies: Vec::new(),
                    },
                );
            }
            BusEventKind::BusReply if !event.question_id.is_empty() => {
                if let Some(question) = self.questions.get_mut(&event.question_id) {
                    question.replies.push(event.seq);
                }
            }
            BusEventKind::BusClosed if !event.question_id.is_empty() => {
                if let Some(question) = self.questions.get_mut(&event.question_id) {
                    question.closed_at = Some(event.timestamp);
                }
            }
            _ => {}
        }
        self.presence.observe(&event);
        self.events.push(event);
    }

    fn track_question_id(&mut self, question_id: &str) {
        let Some(index) = question_id.strip_prefix('Q') else {
            return;
        };
        let Ok(index) = index.parse::<u64>() else {
            return;
        };
        self.next_question_id = self.next_question_id.max(index + 1);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownQuestion {
    question_id: String,
}

impl UnknownQuestion {
    pub fn new(question_id: impl Into<String>) -> Self {
        Self {
            question_id: question_id.into(),
        }
    }
}

impl std::fmt::Display for UnknownQuestion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown question: {}", self.question_id)
    }
}

impl std::error::Error for UnknownQuestion {}

fn digest_message(question: &BusQuestion, reply_count: usize, related_count: usize) -> String {
    let mut message = format!("{} closed: {}", question.question_id, question.message);
    if reply_count > 0 {
        message.push_str(&format!(" | replies={reply_count}"));
    }
    if related_count > 0 {
        message.push_str(&format!(" | related={related_count}"));
    }
    message
}

fn tail(events: Vec<&BusEvent>, limit: usize) -> Vec<&BusEvent> {
    events
        .into_iter()
        .rev()
        .take(limit.max(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn same_or_descendant(child: &str, parent: &str) -> bool {
    let child = normalize_root(child);
    let parent = normalize_root(parent);
    child == parent || child.starts_with(&format!("{parent}/"))
}

fn normalize_root(root: &str) -> String {
    root.trim().replace('\\', "/").trim_end_matches('/').to_string()
}

fn timeout_seconds_from(event: &BusEvent) -> f64 {
    event
        .metadata
        .get("timeout_seconds")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(180.0)
        .max(0.0)
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

    #[test]
    fn question_reply_and_settle_emit_close_digest() {
        let mut journal = BusJournal::new();
        let mut ask = JournalAppend::new(BusEventKind::BusAsk);
        ask.timestamp = 100.0;
        ask.workspace_root = "/repo".to_string();
        ask.workspace_id = "wsid".to_string();
        ask.agent_id = "agent-a".to_string();
        ask.scope = BusScope::parse("src/server.py", "", "");
        ask.message = "anyone touching server?".to_string();
        let (_event, question) = journal.ask(ask, QuestionOpen::new(5.0));

        let mut reply = JournalAppend::new(BusEventKind::BusReply);
        reply.timestamp = 101.0;
        reply.workspace_root = "/repo".to_string();
        reply.message = "yes".to_string();
        journal
            .reply(&question.question_id, reply, false)
            .expect("reply attaches");

        let base = JournalAppend::new(BusEventKind::BusClosed);
        let closed = journal.settle("/repo", 106.0, &base);

        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].close_event.kind, BusEventKind::BusClosed);
        assert_eq!(closed[0].question.question_id, "Q1");
        assert_eq!(closed[0].question.closed_at, Some(106.0));
        assert_eq!(closed[0].replies.len(), 1);
        assert_eq!(
            closed[0].close_event.metadata.get("reply_count"),
            Some(&"1".to_string())
        );
    }

    #[test]
    fn late_reply_is_marked_without_reopening_question() {
        let mut journal = BusJournal::new();
        let mut ask = JournalAppend::new(BusEventKind::BusAsk);
        ask.timestamp = 100.0;
        ask.workspace_root = "/repo".to_string();
        let (_event, question) = journal.ask(ask, QuestionOpen::new(0.0));
        let base = JournalAppend::new(BusEventKind::BusClosed);
        journal.settle("/repo", 100.0, &base);

        let mut reply = JournalAppend::new(BusEventKind::BusReply);
        reply.timestamp = 101.0;
        reply.workspace_root = "/repo".to_string();
        let (event, updated) = journal
            .reply(&question.question_id, reply, false)
            .expect("late reply still records");

        assert_eq!(event.metadata.get("late"), Some(&"true".to_string()));
        assert_eq!(updated.closed_at, Some(100.0));
    }

    #[test]
    fn presence_tracks_active_asleep_pruned_and_session_stop() {
        let mut journal = BusJournal::new();
        let mut append = JournalAppend::new(BusEventKind::AgentStarted);
        append.timestamp = 1000.0;
        append.workspace_root = "/repo".to_string();
        append.agent_id = "agent-a".to_string();
        journal.append(append);

        assert_eq!(
            journal.visible_presence_for_workspace("/repo", 1059.0)[0].status,
            PresenceStatus::Active
        );
        assert_eq!(
            journal.visible_presence_for_workspace("/repo", 1060.0)[0].status,
            PresenceStatus::Asleep
        );
        assert!(journal
            .visible_presence_for_workspace("/repo", 1601.0)
            .is_empty());

        let mut stop = JournalAppend::new(BusEventKind::SessionStop);
        stop.timestamp = 2000.0;
        stop.workspace_root = "/repo".to_string();
        stop.agent_id = "agent-b".to_string();
        journal.append(stop);

        assert_eq!(
            journal.visible_presence_for_workspace("/repo", 2000.0)[0].status,
            PresenceStatus::Asleep
        );
    }

    #[test]
    fn prompt_count_pins_presence_past_prune_window() {
        let mut journal = BusJournal::new();
        let mut prompt = JournalAppend::new(BusEventKind::UserPrompt);
        prompt.timestamp = 1000.0;
        prompt.workspace_root = "/repo".to_string();
        prompt.agent_id = "main-thread".to_string();
        prompt
            .metadata
            .insert("prompt_count".to_string(), "2".to_string());
        journal.append(prompt);

        let agents = journal.visible_presence_for_workspace("/repo", 2000.0);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, PresenceStatus::Asleep);
        assert!(agents[0].pinned);
    }

    #[test]
    fn heartbeat_updates_presence_without_appending_event() {
        let mut journal = BusJournal::new();
        let mut heartbeat = JournalAppend::new(BusEventKind::AgentHeartbeat);
        heartbeat.timestamp = 1000.0;
        heartbeat.workspace_root = "/repo".to_string();
        heartbeat.agent_id = "agent-heart".to_string();

        let entry = journal.heartbeat(heartbeat).expect("presence entry");

        assert_eq!(entry.agent_id, "agent-heart");
        assert!(journal.events().is_empty());
        assert_eq!(
            journal.visible_presence_for_workspace("/repo", 1000.0)[0].status,
            PresenceStatus::Active
        );
    }

    #[test]
    fn recent_all_and_tree_watch_order() {
        let mut journal = BusJournal::new();
        for (root, message) in [
            ("/workspace", "umbrella"),
            ("/workspace/domain", "domain"),
            ("/workspace-other", "other"),
        ] {
            let mut append = JournalAppend::new(BusEventKind::NotePosted);
            append.workspace_root = root.to_string();
            append.message = message.to_string();
            journal.append(append);
        }

        assert_eq!(
            journal
                .recent_all(1, 10)
                .into_iter()
                .map(|event| event.message.as_str())
                .collect::<Vec<_>>(),
            vec!["domain", "other"]
        );
        assert_eq!(
            journal
                .recent_under_roots(&["/workspace".to_string()], 0, 10)
                .into_iter()
                .map(|event| event.message.as_str())
                .collect::<Vec<_>>(),
            vec!["umbrella", "domain"]
        );
    }

    #[test]
    fn replay_rehydrates_sequence_questions_and_presence() {
        let mut journal = BusJournal::new();
        let mut ask = JournalAppend::new(BusEventKind::BusAsk);
        ask.timestamp = 100.0;
        ask.workspace_root = "/repo".to_string();
        ask.agent_id = "agent-a".to_string();
        ask.message = "coordinate?".to_string();
        let (_event, question) = journal.ask(ask, QuestionOpen::new(1.0));

        let mut reply = JournalAppend::new(BusEventKind::BusReply);
        reply.timestamp = 100.5;
        reply.workspace_root = "/repo".to_string();
        reply.agent_id = "agent-b".to_string();
        reply.message = "yes".to_string();
        journal
            .reply(&question.question_id, reply, false)
            .expect("reply");

        let base = JournalAppend::new(BusEventKind::BusClosed);
        journal.settle("/repo", 101.0, &base);

        let mut replayed = BusJournal::from_events(journal.events().to_vec());
        let question = replayed.question("Q1").expect("question rehydrated");
        assert_eq!(question.closed_at, Some(101.0));
        assert_eq!(question.replies, vec![2]);
        assert_eq!(
            replayed
                .visible_presence_for_workspace("/repo", 101.0)
                .iter()
                .map(|entry| entry.agent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["agent-a", "agent-b"]
        );

        let event = replayed.append(JournalAppend::new(BusEventKind::NotePosted));
        assert_eq!(event.seq, 4);
    }
}

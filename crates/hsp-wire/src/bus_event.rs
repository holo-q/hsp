use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownBusEventKind {
    value: String,
}

impl UnknownBusEventKind {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

impl Display for UnknownBusEventKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown bus event kind: {:?}", self.value)
    }
}

impl std::error::Error for UnknownBusEventKind {}

/// Closed event-kind vocabulary for durable bus rows.
///
/// Hook adapters may accept looser names, but this enum is the canonical wire
/// surface. Unknown values are rejected at the DTO boundary so malformed hook
/// kinds do not silently become new protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusEventKind {
    AgentStarted,
    AgentHeartbeat,
    SessionStart,
    SessionStop,
    TicketStarted,
    TicketJoined,
    TicketReleased,
    TicketClosed,
    Prompt,
    UserPrompt,
    TaskIntent,
    ToolBefore,
    ToolAfter,
    Notification,
    SubagentStop,
    CompactBefore,
    EditBefore,
    EditAfter,
    ConfirmBefore,
    ConfirmAfter,
    FileTouched,
    SymbolTouched,
    Test,
    TestRan,
    CommitBefore,
    CommitAfter,
    CommitCreated,
    PushBefore,
    PushAfter,
    NotePosted,
    ChatMessage,
    BusAsk,
    BusReply,
    BusClosed,
    BabelEvent,
}

impl BusEventKind {
    pub const ALL: &'static [Self] = &[
        Self::AgentStarted,
        Self::AgentHeartbeat,
        Self::SessionStart,
        Self::SessionStop,
        Self::TicketStarted,
        Self::TicketJoined,
        Self::TicketReleased,
        Self::TicketClosed,
        Self::Prompt,
        Self::UserPrompt,
        Self::TaskIntent,
        Self::ToolBefore,
        Self::ToolAfter,
        Self::Notification,
        Self::SubagentStop,
        Self::CompactBefore,
        Self::EditBefore,
        Self::EditAfter,
        Self::ConfirmBefore,
        Self::ConfirmAfter,
        Self::FileTouched,
        Self::SymbolTouched,
        Self::Test,
        Self::TestRan,
        Self::CommitBefore,
        Self::CommitAfter,
        Self::CommitCreated,
        Self::PushBefore,
        Self::PushAfter,
        Self::NotePosted,
        Self::ChatMessage,
        Self::BusAsk,
        Self::BusReply,
        Self::BusClosed,
        Self::BabelEvent,
    ];

    pub fn as_wire(self) -> &'static str {
        match self {
            Self::AgentStarted => "agent.started",
            Self::AgentHeartbeat => "agent.heartbeat",
            Self::SessionStart => "session.start",
            Self::SessionStop => "session.stop",
            Self::TicketStarted => "ticket.started",
            Self::TicketJoined => "ticket.joined",
            Self::TicketReleased => "ticket.released",
            Self::TicketClosed => "ticket.closed",
            Self::Prompt => "prompt",
            Self::UserPrompt => "user.prompt",
            Self::TaskIntent => "task.intent",
            Self::ToolBefore => "tool.before",
            Self::ToolAfter => "tool.after",
            Self::Notification => "notification",
            Self::SubagentStop => "subagent.stop",
            Self::CompactBefore => "compact.before",
            Self::EditBefore => "edit.before",
            Self::EditAfter => "edit.after",
            Self::ConfirmBefore => "confirm.before",
            Self::ConfirmAfter => "confirm.after",
            Self::FileTouched => "file.touched",
            Self::SymbolTouched => "symbol.touched",
            Self::Test => "test",
            Self::TestRan => "test.ran",
            Self::CommitBefore => "commit.before",
            Self::CommitAfter => "commit.after",
            Self::CommitCreated => "commit.created",
            Self::PushBefore => "push.before",
            Self::PushAfter => "push.after",
            Self::NotePosted => "note.posted",
            Self::ChatMessage => "chat.message",
            Self::BusAsk => "bus.ask",
            Self::BusReply => "bus.reply",
            Self::BusClosed => "bus.closed",
            Self::BabelEvent => "babel.event",
        }
    }

    pub fn from_wire(value: &str) -> Result<Self, UnknownBusEventKind> {
        let normalized = match value {
            "prompt.start" => "prompt",
            "session.started" => "session.start",
            "session.ended" | "stop" => "session.stop",
            "pre_tool" => "tool.before",
            "post_tool" => "tool.after",
            "pre_compact" => "compact.before",
            "subagent_stop" => "subagent.stop",
            "lsp_confirm.before" => "confirm.before",
            "lsp_confirm.after" => "confirm.after",
            "test.result" => "test",
            "git.commit" => "commit.after",
            "git.push" => "push.after",
            other => other,
        };

        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_wire() == normalized)
            .ok_or_else(|| UnknownBusEventKind::new(value))
    }
}

impl Display for BusEventKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

impl FromStr for BusEventKind {
    type Err = UnknownBusEventKind;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_wire(value)
    }
}

impl Serialize for BusEventKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for BusEventKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_wire(&value).map_err(serde::de::Error::custom)
    }
}

/// Files, symbols, and render aliases touched by a bus row or question.
///
/// Empty scope is a wildcard by design. It represents a workspace-wide event,
/// not "nothing happened".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusScope {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub symbols: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl BusScope {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn parse(files: &str, symbols: &str, aliases: &str) -> Self {
        Self {
            files: split_scope_items(files),
            symbols: split_scope_items(symbols),
            aliases: split_scope_items(aliases),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.symbols.is_empty() && self.aliases.is_empty()
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        if self.is_empty() || other.is_empty() {
            return true;
        }
        overlaps_any(&self.files, &other.files, file_scope_overlaps)
            || overlaps_any(&self.symbols, &other.symbols, |a, b| a == b)
            || overlaps_any(&self.aliases, &other.aliases, |a, b| a == b)
    }
}

fn split_scope_items(value: &str) -> Vec<String> {
    value
        .replace('\n', ",")
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn overlaps_any<F>(left: &[String], right: &[String], predicate: F) -> bool
where
    F: Fn(&str, &str) -> bool,
{
    left.iter()
        .any(|a| right.iter().any(|b| predicate(a.as_str(), b.as_str())))
}

fn file_scope_overlaps(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }

    let left = normalize_scope_path(left);
    let right = normalize_scope_path(right);
    left == right
        || left.ends_with(&format!("/{right}"))
        || right.ends_with(&format!("/{left}"))
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

fn normalize_scope_path(value: &str) -> String {
    value.trim().replace('\\', "/").trim_matches('/').to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedMessage {
    pub message: String,
    pub truncated: bool,
}

pub fn truncate_message(message: &str, limit: usize) -> TruncatedMessage {
    if message.is_empty() {
        return TruncatedMessage {
            message: String::new(),
            truncated: false,
        };
    }
    if message.len() <= limit {
        return TruncatedMessage {
            message: message.to_string(),
            truncated: false,
        };
    }

    let mut end = limit;
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }

    TruncatedMessage {
        message: message[..end].to_string(),
        truncated: true,
    }
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
}

/// One durable agent-bus event in the JSONL wire shape.
///
/// `seq` is numeric ordering; `event_id` is the human handle (`E{seq}`). The
/// split is deliberate and is called out in the Python preservation ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusEvent {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub event_id: String,
    pub kind: BusEventKind,
    #[serde(default)]
    pub timestamp: f64,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub workspace_root: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub git_head: String,
    #[serde(default)]
    pub dirty_hash: String,
    #[serde(default)]
    pub scope: BusScope,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub question_id: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default = "schema_version")]
    pub schema_version: u32,
}

impl BusEvent {
    pub fn new(kind: BusEventKind) -> Self {
        Self {
            seq: 0,
            event_id: String::new(),
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
            truncated: false,
            schema_version: SCHEMA_VERSION,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_wire_name() {
        for kind in BusEventKind::ALL {
            assert_eq!(BusEventKind::from_wire(kind.as_wire()), Ok(*kind));
        }
    }

    #[test]
    fn aliases_normalize_to_canonical_kinds() {
        assert_eq!(
            BusEventKind::from_wire("test.result"),
            Ok(BusEventKind::Test)
        );
        assert_eq!(
            BusEventKind::from_wire("lsp_confirm.after"),
            Ok(BusEventKind::ConfirmAfter)
        );
        assert_eq!(
            BusEventKind::from_wire("git.push"),
            Ok(BusEventKind::PushAfter)
        );
    }

    #[test]
    fn empty_scope_is_wildcard() {
        let empty = BusScope::empty();
        let scoped = BusScope::parse("src/server.py", "", "");
        assert!(empty.overlaps(&scoped));
        assert!(scoped.overlaps(&empty));
        assert!(empty.overlaps(&empty));
    }

    #[test]
    fn file_scope_overlap_accepts_exact_tail_and_prefix() {
        let owner = BusScope::parse("src/hsp/server.py", "", "");
        assert!(owner.overlaps(&BusScope::parse("server.py", "", "")));
        assert!(owner.overlaps(&BusScope::parse("src/hsp", "", "")));
        assert!(!owner.overlaps(&BusScope::parse("src/hsp/client.py", "", "")));
    }

    #[test]
    fn parse_accepts_commas_and_newlines() {
        let scope = BusScope::parse("src/server.py, src/x.py", "Foo\nBar\n", "");
        assert_eq!(scope.files, vec!["src/server.py", "src/x.py"]);
        assert_eq!(scope.symbols, vec!["Foo", "Bar"]);
        assert!(scope.aliases.is_empty());
    }

    #[test]
    fn event_deserialization_ignores_unknown_keys_and_defaults_missing_fields() {
        let event: BusEvent = serde_json::from_str(
            r#"{"kind":"agent.started","future_field":{"hello":"world"}}"#,
        )
        .expect("minimal event deserializes");
        assert_eq!(event.kind, BusEventKind::AgentStarted);
        assert_eq!(event.seq, 0);
        assert_eq!(event.message, "");
        assert_eq!(event.scope, BusScope::empty());
        assert_eq!(event.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn serialized_event_uses_canonical_wire_kind() {
        let event = BusEvent::new(BusEventKind::NotePosted);
        let json = serde_json::to_string(&event).expect("event serializes");
        assert!(json.contains(r#""kind":"note.posted""#));
        assert!(json.contains(r#""schema_version":1"#));
    }

    #[test]
    fn truncation_is_utf8_safe() {
        let clipped = truncate_message(&"☃".repeat(5), 4);
        assert!(clipped.truncated);
        assert!(clipped.message.len() <= 4);
        assert!(std::str::from_utf8(clipped.message.as_bytes()).is_ok());
    }
}

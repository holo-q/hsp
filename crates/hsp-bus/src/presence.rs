use std::collections::BTreeMap;

use hsp_wire::{BusEvent, BusEventKind};
use serde::Serialize;

pub const ACTIVE_WINDOW_SECONDS: f64 = 60.0;
pub const PRUNE_WINDOW_SECONDS: f64 = 600.0;
pub const PIN_PROMPT_THRESHOLD: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresenceStatus {
    Active,
    Asleep,
    Pruned,
}

impl PresenceStatus {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Asleep => "asleep",
            Self::Pruned => "pruned",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresenceEntry {
    pub agent_id: String,
    pub client_id: String,
    pub session_id: String,
    pub workspace_root: String,
    pub first_seen_at: f64,
    pub last_seen_at: f64,
    pub last_prompt_at: f64,
    pub prompt_count: u64,
    pub last_event_id: String,
    pub pinned: bool,
    pub status: PresenceStatus,
}

impl PresenceEntry {
    pub fn to_wire(&self, now: f64) -> PresenceEntryWire<'_> {
        let idle_seconds = (now - self.last_seen_at).max(0.0);
        PresenceEntryWire {
            agent_id: &self.agent_id,
            client_id: &self.client_id,
            session_id: &self.session_id,
            workspace_root: &self.workspace_root,
            state: self.status.as_wire(),
            status: self.status.as_wire(),
            idle_seconds,
            first_seen_at: self.first_seen_at,
            last_seen_at: self.last_seen_at,
            last_prompt_at: self.last_prompt_at,
            prompt_count: self.prompt_count,
            last_event_id: &self.last_event_id,
            pinned: self.pinned,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PresenceEntryWire<'a> {
    pub agent_id: &'a str,
    pub client_id: &'a str,
    pub session_id: &'a str,
    pub workspace_root: &'a str,
    pub state: &'static str,
    pub status: &'static str,
    pub idle_seconds: f64,
    pub first_seen_at: f64,
    pub last_seen_at: f64,
    pub last_prompt_at: f64,
    pub prompt_count: u64,
    pub last_event_id: &'a str,
    pub pinned: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PresenceTracker {
    entries: BTreeMap<String, PresenceEntry>,
}

impl PresenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, event: &BusEvent) -> Option<PresenceEntry> {
        let key = presence_key(event)?;
        let entry = self.entries.entry(key).or_insert_with(|| PresenceEntry {
            agent_id: event.agent_id.clone(),
            client_id: event.client_id.clone(),
            session_id: event.session_id.clone(),
            workspace_root: event.workspace_root.clone(),
            first_seen_at: event.timestamp,
            last_seen_at: event.timestamp,
            last_prompt_at: 0.0,
            prompt_count: 0,
            last_event_id: String::new(),
            pinned: false,
            status: PresenceStatus::Active,
        });

        if event.timestamp >= entry.last_seen_at {
            entry.last_seen_at = event.timestamp;
        }
        if !event.agent_id.is_empty() && entry.agent_id.is_empty() {
            entry.agent_id = event.agent_id.clone();
        }
        if !event.client_id.is_empty() && entry.client_id.is_empty() {
            entry.client_id = event.client_id.clone();
        }
        if !event.session_id.is_empty() && entry.session_id.is_empty() {
            entry.session_id = event.session_id.clone();
        }
        entry.last_event_id = event.event_id.clone();
        if event.kind == BusEventKind::SessionStop {
            entry.last_seen_at = entry
                .last_seen_at
                .min(event.timestamp - ACTIVE_WINDOW_SECONDS);
        }
        if matches!(event.kind, BusEventKind::Prompt | BusEventKind::UserPrompt) {
            entry.last_prompt_at = event.timestamp;
            entry.prompt_count = (entry.prompt_count + 1).max(prompt_count(event));
            if entry.prompt_count >= PIN_PROMPT_THRESHOLD {
                entry.pinned = true;
            }
        }
        Some(entry.clone())
    }

    pub fn status_at(&self, entry: &PresenceEntry, now: f64) -> PresenceStatus {
        let elapsed = (now - entry.last_seen_at).max(0.0);
        if elapsed >= PRUNE_WINDOW_SECONDS && !entry.pinned {
            PresenceStatus::Pruned
        } else if elapsed >= ACTIVE_WINDOW_SECONDS {
            PresenceStatus::Asleep
        } else {
            PresenceStatus::Active
        }
    }

    pub fn snapshot(&self, now: f64) -> Vec<PresenceEntry> {
        self.entries
            .values()
            .cloned()
            .map(|mut entry| {
                entry.status = self.status_at(&entry, now);
                entry
            })
            .collect()
    }

    pub fn visible(&self, now: f64) -> Vec<PresenceEntry> {
        self.snapshot(now)
            .into_iter()
            .filter(|entry| entry.status != PresenceStatus::Pruned || entry.pinned)
            .collect()
    }
}

fn presence_key(event: &BusEvent) -> Option<String> {
    if !event.client_id.is_empty() {
        Some(event.client_id.clone())
    } else if !event.agent_id.is_empty() {
        Some(event.agent_id.clone())
    } else if !event.session_id.is_empty() {
        Some(event.session_id.clone())
    } else {
        None
    }
}

fn prompt_count(event: &BusEvent) -> u64 {
    event.metadata
        .get("prompt_count")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

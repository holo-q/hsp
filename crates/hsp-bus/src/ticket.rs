use std::collections::{BTreeMap, BTreeSet, HashMap};

use hsp_wire::BusScope;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ticket {
    pub ticket_id: String,
    pub message: String,
    pub workspace_root: String,
    pub opened_at: f64,
    pub closed_at: Option<f64>,
    pub holders: BTreeMap<String, f64>,
    pub scope: BusScope,
    pub projects: Vec<String>,
}

impl Ticket {
    pub fn is_open(&self) -> bool {
        self.closed_at.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TicketIntent {
    pub workspace_root: String,
    pub agent_id: String,
    pub message: String,
    pub scope: BusScope,
    pub projects: Vec<String>,
    pub now: f64,
}

impl TicketIntent {
    pub fn new(
        workspace_root: impl Into<String>,
        agent_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            agent_id: agent_id.into(),
            message: message.into(),
            scope: BusScope::empty(),
            projects: Vec::new(),
            now: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildGate {
    pub workspace_root: String,
    pub gate_key: String,
    pub unlocked: bool,
    pub reason: &'static str,
    pub holders: Vec<String>,
    pub waiting: Vec<String>,
    pub active_tickets: Vec<Ticket>,
    pub full_workspace: bool,
    pub projects: Vec<String>,
    pub scope: BusScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditGateMode {
    Workgroup,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EditGate {
    pub workspace_root: String,
    pub allowed: bool,
    pub reason: &'static str,
    pub agent_id: String,
    pub active_tickets: Vec<Ticket>,
    pub ticket: Option<Ticket>,
}

#[derive(Debug, Clone, Default)]
pub struct TicketBoard {
    tickets: BTreeMap<String, Ticket>,
    agent_tickets: HashMap<String, String>,
    build_waiters: HashMap<String, BTreeSet<String>>,
    next_ticket_id: u64,
}

impl TicketBoard {
    pub fn new() -> Self {
        Self {
            next_ticket_id: 1,
            ..Self::default()
        }
    }

    pub fn hold(&mut self, intent: TicketIntent) -> Option<Ticket> {
        if intent.message.trim().is_empty() {
            self.release(&intent.workspace_root, &intent.agent_id, intent.now);
            return None;
        }

        if let Some(ticket_id) = self.agent_tickets.get(&intent.agent_id).cloned() {
            if let Some(ticket) = self.tickets.get_mut(&ticket_id) {
                if ticket.is_open()
                    && ticket.workspace_root == intent.workspace_root
                    && ticket.message == intent.message
                {
                    merge_scope(&mut ticket.scope, &intent.scope);
                    merge_items(&mut ticket.projects, &intent.projects);
                    ticket.holders.insert(intent.agent_id, intent.now);
                    return Some(ticket.clone());
                }
            }
        }

        self.release(&intent.workspace_root, &intent.agent_id, intent.now);
        self.discard_build_waiter(&intent.workspace_root, &intent.agent_id);

        let ticket_id = self
            .find_open_ticket(&intent.workspace_root, &intent.message)
            .map(|ticket| ticket.ticket_id.clone())
            .unwrap_or_else(|| self.create_ticket(&intent));

        let ticket = self
            .tickets
            .get_mut(&ticket_id)
            .expect("ticket exists after create/find");
        merge_scope(&mut ticket.scope, &intent.scope);
        merge_items(&mut ticket.projects, &intent.projects);
        ticket.holders.insert(intent.agent_id.clone(), intent.now);
        self.agent_tickets.insert(intent.agent_id, ticket_id);
        Some(ticket.clone())
    }

    pub fn release(&mut self, workspace_root: &str, agent_id: &str, now: f64) -> Vec<Ticket> {
        let Some(ticket_id) = self.agent_tickets.remove(agent_id) else {
            return Vec::new();
        };
        let Some(ticket) = self.tickets.get_mut(&ticket_id) else {
            return Vec::new();
        };
        if !ticket.is_open() || ticket.workspace_root != workspace_root {
            return Vec::new();
        }

        ticket.holders.remove(agent_id);
        if ticket.holders.is_empty() {
            ticket.closed_at = Some(now);
        }
        vec![ticket.clone()]
    }

    pub fn active_tickets(&self, workspace_root: &str) -> Vec<Ticket> {
        self.tickets
            .values()
            .filter(|ticket| ticket.workspace_root == workspace_root && ticket.is_open())
            .cloned()
            .collect()
    }

    pub fn build_gate(
        &mut self,
        workspace_root: impl Into<String>,
        agent_id: Option<&str>,
        scope: BusScope,
        projects: Vec<String>,
        full_workspace: bool,
    ) -> BuildGate {
        let workspace_root = workspace_root.into();
        let full_workspace = full_workspace || scope.is_empty();
        let gate_key = build_gate_key(&workspace_root, &projects);
        if let Some(agent_id) = agent_id.filter(|agent_id| !agent_id.is_empty()) {
            self.build_waiters
                .entry(gate_key.clone())
                .or_default()
                .insert(agent_id.to_string());
        }

        let active_tickets = self
            .tickets
            .values()
            .filter(|ticket| {
                ticket.workspace_root == workspace_root
                    && ticket.is_open()
                    && ticket_blocks_project(ticket, &projects)
                    && ticket_blocks_scope(ticket, &scope, full_workspace)
            })
            .cloned()
            .collect::<Vec<_>>();
        let holders = active_tickets
            .iter()
            .flat_map(|ticket| ticket.holders.keys().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let waiters = self.build_waiters.get(&gate_key).cloned().unwrap_or_default();
        let waiting = holders
            .iter()
            .filter(|holder| waiters.contains(*holder))
            .cloned()
            .collect::<Vec<_>>();
        let unlocked = holders.is_empty() || holders.iter().all(|holder| waiters.contains(holder));
        let reason = if holders.is_empty() {
            "clear"
        } else if unlocked {
            "all_waiting"
        } else {
            "active_tickets"
        };

        BuildGate {
            workspace_root,
            gate_key,
            unlocked,
            reason,
            holders,
            waiting,
            active_tickets,
            full_workspace,
            projects,
            scope,
        }
    }

    pub fn edit_gate(
        &self,
        workspace_root: impl Into<String>,
        agent_id: impl Into<String>,
        mode: EditGateMode,
    ) -> EditGate {
        let workspace_root = workspace_root.into();
        let agent_id = agent_id.into();
        let active_tickets = self.active_tickets(&workspace_root);
        match mode {
            EditGateMode::Workgroup => EditGate {
                workspace_root,
                allowed: !active_tickets.is_empty(),
                reason: if active_tickets.is_empty() {
                    "missing_ticket"
                } else {
                    "ticket_active"
                },
                agent_id,
                active_tickets,
                ticket: None,
            },
            EditGateMode::Agent => {
                let ticket = self
                    .agent_tickets
                    .get(&agent_id)
                    .and_then(|ticket_id| self.tickets.get(ticket_id))
                    .filter(|ticket| ticket.workspace_root == workspace_root && ticket.is_open())
                    .cloned();
                EditGate {
                    workspace_root,
                    allowed: ticket.is_some(),
                    reason: if ticket.is_some() {
                        "ticket_held"
                    } else {
                        "missing_ticket"
                    },
                    agent_id,
                    active_tickets,
                    ticket,
                }
            }
        }
    }

    fn find_open_ticket(&self, workspace_root: &str, message: &str) -> Option<&Ticket> {
        self.tickets
            .values()
            .find(|ticket| ticket.workspace_root == workspace_root && ticket.message == message && ticket.is_open())
    }

    fn create_ticket(&mut self, intent: &TicketIntent) -> String {
        let ticket_id = format!("T{}", self.next_ticket_id);
        self.next_ticket_id += 1;
        self.tickets.insert(
            ticket_id.clone(),
            Ticket {
                ticket_id: ticket_id.clone(),
                message: intent.message.clone(),
                workspace_root: intent.workspace_root.clone(),
                opened_at: intent.now,
                closed_at: None,
                holders: BTreeMap::new(),
                scope: intent.scope.clone(),
                projects: intent.projects.clone(),
            },
        );
        ticket_id
    }

    fn discard_build_waiter(&mut self, workspace_root: &str, agent_id: &str) {
        let prefix = format!("{workspace_root}\n");
        self.build_waiters.retain(|key, waiters| {
            if key == workspace_root || key.starts_with(&prefix) {
                waiters.remove(agent_id);
            }
            !waiters.is_empty()
        });
    }
}

fn merge_scope(target: &mut BusScope, incoming: &BusScope) {
    merge_items(&mut target.files, &incoming.files);
    merge_items(&mut target.symbols, &incoming.symbols);
    merge_items(&mut target.aliases, &incoming.aliases);
}

fn merge_items(target: &mut Vec<String>, incoming: &[String]) {
    for item in incoming {
        if !target.contains(item) {
            target.push(item.clone());
        }
    }
}

fn build_gate_key(workspace_root: &str, projects: &[String]) -> String {
    if projects.is_empty() {
        return workspace_root.to_string();
    }
    let mut projects = projects.to_vec();
    projects.sort();
    format!("{workspace_root}\n{}", projects.join("\n"))
}

fn ticket_blocks_project(ticket: &Ticket, projects: &[String]) -> bool {
    projects.is_empty() || ticket.projects.is_empty() || scope_items_overlap(&ticket.projects, projects)
}

fn ticket_blocks_scope(ticket: &Ticket, scope: &BusScope, full_workspace: bool) -> bool {
    full_workspace
        || ticket.scope.is_empty()
        || scope_items_overlap(&ticket.scope.files, &scope.files)
        || scope_items_overlap(&ticket.scope.symbols, &scope.symbols)
        || scope_items_overlap(&ticket.scope.aliases, &scope.aliases)
}

fn scope_items_overlap(left: &[String], right: &[String]) -> bool {
    !left.is_empty()
        && !right.is_empty()
        && left
            .iter()
            .any(|left| right.iter().any(|right| scope_item_overlaps(left, right)))
}

fn scope_item_overlaps(left: &str, right: &str) -> bool {
    let left = left.trim().trim_start_matches("./").trim_end_matches('/');
    let right = right.trim().trim_start_matches("./").trim_end_matches('/');
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if left == right {
        return true;
    }
    if !left.contains('/') && !right.contains('/') {
        return false;
    }
    left.ends_with(&format!("/{right}"))
        || right.ends_with(&format!("/{left}"))
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
        || component_suffix_prefix_overlaps(left, right)
}

fn component_suffix_prefix_overlaps(left: &str, right: &str) -> bool {
    let left = left.split('/').filter(|part| !part.is_empty()).collect::<Vec<_>>();
    let right = right.split('/').filter(|part| !part.is_empty()).collect::<Vec<_>>();
    suffix_prefix_overlaps(&left, &right) || suffix_prefix_overlaps(&right, &left)
}

fn suffix_prefix_overlaps(left: &[&str], right: &[&str]) -> bool {
    (0..left.len()).any(|index| {
        let suffix = &left[index..];
        right.len() >= suffix.len() && right[..suffix.len()] == *suffix
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(agent_id: &str, message: &str) -> TicketIntent {
        TicketIntent::new("/repo", agent_id, message)
    }

    #[test]
    fn tickets_join_release_and_close() {
        let mut board = TicketBoard::new();
        let first = board.hold(intent("agent-a", "wire team tickets")).expect("ticket");
        let second = board.hold(intent("agent-b", "wire team tickets")).expect("ticket");

        assert_eq!(first.ticket_id, "T1");
        assert_eq!(second.ticket_id, "T1");
        assert_eq!(
            second.holders.keys().cloned().collect::<Vec<_>>(),
            vec!["agent-a", "agent-b"]
        );

        assert!(!board.release("/repo", "agent-a", 1.0)[0].holders.is_empty());
        let released = board.release("/repo", "agent-b", 2.0);
        assert_eq!(released[0].closed_at, Some(2.0));
        assert!(board.active_tickets("/repo").is_empty());
    }

    #[test]
    fn reposting_same_ticket_is_idempotent_and_merges_scope() {
        let mut board = TicketBoard::new();
        let mut first = intent("agent-a", "same ticket");
        first.scope = BusScope::parse("src/a.rs", "", "");
        board.hold(first);

        let mut second = intent("agent-a", "same ticket");
        second.scope = BusScope::parse("src/b.rs", "", "");
        let ticket = board.hold(second).expect("ticket");

        assert_eq!(ticket.ticket_id, "T1");
        assert_eq!(board.active_tickets("/repo").len(), 1);
        assert_eq!(ticket.scope.files, vec!["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn build_gate_unlocks_when_every_holder_is_waiting() {
        let mut board = TicketBoard::new();
        board.hold(intent("agent-a", "edit server"));
        board.hold(intent("agent-b", "edit server"));

        let cold = board.build_gate("/repo", None, BusScope::empty(), Vec::new(), false);
        let one_waiting = board.build_gate("/repo", Some("agent-a"), BusScope::empty(), Vec::new(), false);
        let all_waiting = board.build_gate("/repo", Some("agent-b"), BusScope::empty(), Vec::new(), false);

        assert!(!cold.unlocked);
        assert_eq!(cold.reason, "active_tickets");
        assert!(!one_waiting.unlocked);
        assert!(all_waiting.unlocked);
        assert_eq!(all_waiting.reason, "all_waiting");
    }

    #[test]
    fn scoped_build_gate_uses_file_overlap_and_unknown_scope_blocks() {
        let mut board = TicketBoard::new();
        let mut docs = intent("agent-a", "edit docs");
        docs.scope = BusScope::parse("docs/guide.md", "", "");
        board.hold(docs);

        let unrelated = board.build_gate(
            "/repo",
            None,
            BusScope::parse("src/server.py", "", ""),
            Vec::new(),
            false,
        );
        assert!(unrelated.unlocked);
        assert!(unrelated.holders.is_empty());

        let mut server = intent("agent-b", "edit server");
        server.scope = BusScope::parse("src/server.py", "", "");
        board.hold(server);
        let related = board.build_gate(
            "/repo",
            None,
            BusScope::parse("/repo/src", "", ""),
            Vec::new(),
            false,
        );
        assert!(!related.unlocked);
        assert_eq!(related.holders, vec!["agent-b"]);

        board.hold(intent("agent-c", "unknown scope"));
        let unknown = board.build_gate(
            "/repo",
            None,
            BusScope::parse("src/client.py", "", ""),
            Vec::new(),
            false,
        );
        assert!(unknown.holders.contains(&"agent-c".to_string()));
    }

    #[test]
    fn project_scoped_build_gate_ignores_unrelated_projects() {
        let mut board = TicketBoard::new();
        let mut app = TicketIntent::new("/workspace/domain", "agent-a", "edit app");
        app.projects = vec!["/workspace/domain/app".to_string()];
        board.hold(app);

        let unrelated = board.build_gate(
            "/workspace/domain",
            Some("agent-b"),
            BusScope::empty(),
            vec!["/workspace/domain/service".to_string()],
            false,
        );
        let related = board.build_gate(
            "/workspace/domain",
            Some("agent-b"),
            BusScope::empty(),
            vec!["/workspace/domain/app".to_string()],
            false,
        );

        assert!(unrelated.unlocked);
        assert!(unrelated.holders.is_empty());
        assert!(!related.unlocked);
        assert_eq!(related.holders, vec!["agent-a"]);
    }

    #[test]
    fn new_ticket_clears_stale_build_wait_state_for_agent() {
        let mut board = TicketBoard::new();
        board.hold(intent("agent-a", "old ticket"));
        assert!(board
            .build_gate("/repo", Some("agent-a"), BusScope::empty(), Vec::new(), false)
            .unlocked);

        board.hold(intent("agent-a", "new ticket"));
        let gate = board.build_gate("/repo", None, BusScope::empty(), Vec::new(), false);

        assert!(!gate.unlocked);
        assert!(gate.waiting.is_empty());
    }

    #[test]
    fn edit_gate_supports_workgroup_and_agent_modes() {
        let mut board = TicketBoard::new();
        let denied = board.edit_gate("/repo", "agent-a", EditGateMode::Workgroup);
        board.hold(intent("agent-b", "editing"));
        let workgroup_allowed = board.edit_gate("/repo", "agent-a", EditGateMode::Workgroup);
        let agent_denied = board.edit_gate("/repo", "agent-a", EditGateMode::Agent);
        let agent_allowed = board.edit_gate("/repo", "agent-b", EditGateMode::Agent);

        assert!(!denied.allowed);
        assert_eq!(denied.reason, "missing_ticket");
        assert!(workgroup_allowed.allowed);
        assert_eq!(workgroup_allowed.reason, "ticket_active");
        assert!(!agent_denied.allowed);
        assert!(agent_allowed.allowed);
        assert_eq!(agent_allowed.reason, "ticket_held");
    }
}

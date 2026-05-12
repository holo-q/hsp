use hsp_wire::BusScope;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq)]
pub struct BusQuestion {
    pub question_id: String,
    pub opened_event_id: String,
    pub opened_at: f64,
    pub expires_at: f64,
    pub workspace_root: String,
    pub agent_id: String,
    pub scope: BusScope,
    pub message: String,
    pub closed_at: Option<f64>,
    pub replies: Vec<u64>,
}

impl BusQuestion {
    pub fn is_open(&self) -> bool {
        self.closed_at.is_none()
    }

    pub fn to_wire(&self, now: f64) -> BusQuestionWire<'_> {
        BusQuestionWire {
            question_id: &self.question_id,
            opened_event_id: &self.opened_event_id,
            opened_at: self.opened_at,
            expires_at: self.expires_at,
            seconds_left: if self.is_open() {
                (self.expires_at - now).max(0.0)
            } else {
                0.0
            },
            workspace_root: &self.workspace_root,
            agent_id: &self.agent_id,
            files: &self.scope.files,
            symbols: &self.scope.symbols,
            aliases: &self.scope.aliases,
            message: &self.message,
            closed_at: self.closed_at,
            replies: &self.replies,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BusQuestionWire<'a> {
    pub question_id: &'a str,
    pub opened_event_id: &'a str,
    pub opened_at: f64,
    pub expires_at: f64,
    pub seconds_left: f64,
    pub workspace_root: &'a str,
    pub agent_id: &'a str,
    pub files: &'a [String],
    pub symbols: &'a [String],
    pub aliases: &'a [String],
    pub message: &'a str,
    pub closed_at: Option<f64>,
    pub replies: &'a [u64],
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestionOpen {
    pub timeout_seconds: f64,
    pub close_immediately: bool,
}

impl QuestionOpen {
    pub fn new(timeout_seconds: f64) -> Self {
        Self {
            timeout_seconds,
            close_immediately: false,
        }
    }
}

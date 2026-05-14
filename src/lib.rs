pub const PY_REFERENCE_DIR: &str = "references/hsp-py";
pub const PY_FEATURE_LEDGER: &str = "references/hsp-py/README.md#feature-preservation-ledger";

pub use hsp_broker::BrokerCore;
pub use hsp_bus::{
    BusJournal, BusQuestion, JournalAppend, PresenceEntry, PresenceStatus, PresenceTracker,
    QuestionOpen,
};
pub use hsp_client::{BrokerClient, start_broker_subprocess};
pub use hsp_daemon::{ServeOptions, serve_default, serve_unix};
pub use hsp_org::HspWorkspace;
pub use hsp_protocol::{
    BROKER_MODE_ENV, DEFAULT_IDLE_TTL_SECONDS, DEFAULT_SOCKET_NAME, IDLE_TTL_ENV,
    LOG_ENV_OVERRIDE, ProtocolEnv, SOCKET_ENV_OVERRIDE, broker_log_path,
    broker_log_path_with, idle_ttl_seconds, idle_ttl_seconds_with, socket_path,
    socket_path_with,
};
pub use hsp_session::{BrokerSession, SessionKey, SessionRecord, SessionRegistry, config_hash};
pub use hsp_store::{
    BUS_DIR_ENV, BrokerMode, BusLog, LOG_FILE_NAME, WORKSPACE_ID_LENGTH, bus_dir_for,
    log_path_for, workspace_id_for,
};
pub use hsp_wire::{BusEvent, BusEventKind, BusScope};

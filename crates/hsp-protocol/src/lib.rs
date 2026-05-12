use std::path::PathBuf;

pub const SOCKET_ENV_OVERRIDE: &str = "HSP_BROKER_SOCKET";
pub const LOG_ENV_OVERRIDE: &str = "HSP_BROKER_LOG";
pub const BROKER_MODE_ENV: &str = "HSP_BROKER";
pub const IDLE_TTL_ENV: &str = "HSP_BROKER_IDLE_TTL_SECONDS";
pub const DEFAULT_SOCKET_NAME: &str = "hsp-broker.sock";
pub const DEFAULT_IDLE_TTL_SECONDS: u64 = 4 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolEnv {
    pub socket_override: Option<PathBuf>,
    pub log_override: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub user: Option<String>,
    pub uid: u32,
    pub run_user_exists: bool,
    pub idle_ttl_seconds: Option<u64>,
}

impl ProtocolEnv {
    pub fn current() -> Self {
        let uid = current_uid();
        let run_user = PathBuf::from(format!("/run/user/{uid}"));
        Self {
            socket_override: env_path(SOCKET_ENV_OVERRIDE),
            log_override: env_path(LOG_ENV_OVERRIDE),
            xdg_runtime_dir: env_path("XDG_RUNTIME_DIR"),
            xdg_state_home: env_path("XDG_STATE_HOME"),
            home: env_path("HOME"),
            user: std::env::var("USER")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            uid,
            run_user_exists: run_user.exists(),
            idle_ttl_seconds: std::env::var(IDLE_TTL_ENV)
                .ok()
                .and_then(|value| value.parse::<u64>().ok()),
        }
    }
}

pub fn socket_path() -> PathBuf {
    socket_path_with(&ProtocolEnv::current())
}

pub fn socket_path_with(env: &ProtocolEnv) -> PathBuf {
    if let Some(path) = &env.socket_override {
        return path.clone();
    }
    if let Some(runtime) = &env.xdg_runtime_dir {
        return runtime.join(DEFAULT_SOCKET_NAME);
    }
    if env.run_user_exists {
        return PathBuf::from(format!("/run/user/{}", env.uid)).join(DEFAULT_SOCKET_NAME);
    }
    PathBuf::from(format!(
        "/tmp/hsp-broker-{}",
        env.user.clone().unwrap_or_else(|| env.uid.to_string())
    ))
    .join(DEFAULT_SOCKET_NAME)
}

pub fn broker_log_path() -> PathBuf {
    broker_log_path_with(&ProtocolEnv::current())
}

pub fn broker_log_path_with(env: &ProtocolEnv) -> PathBuf {
    if let Some(path) = &env.log_override {
        return path.clone();
    }
    let base = env
        .xdg_state_home
        .clone()
        .or_else(|| env.home.as_ref().map(|home| home.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    base.join("hsp").join("broker.log")
}

pub fn idle_ttl_seconds() -> u64 {
    idle_ttl_seconds_with(&ProtocolEnv::current())
}

pub fn idle_ttl_seconds_with(env: &ProtocolEnv) -> u64 {
    env.idle_ttl_seconds.unwrap_or(DEFAULT_IDLE_TTL_SECONDS)
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> ProtocolEnv {
        ProtocolEnv {
            socket_override: None,
            log_override: None,
            xdg_runtime_dir: None,
            xdg_state_home: None,
            home: Some(PathBuf::from("/home/noesis")),
            user: Some("noesis".to_string()),
            uid: 4242,
            run_user_exists: false,
            idle_ttl_seconds: None,
        }
    }

    #[test]
    fn socket_override_wins() {
        let mut env = env();
        env.socket_override = Some(PathBuf::from("/isolated/hsp.sock"));
        env.xdg_runtime_dir = Some(PathBuf::from("/run/user/4242"));

        assert_eq!(socket_path_with(&env), PathBuf::from("/isolated/hsp.sock"));
    }

    #[test]
    fn xdg_runtime_dir_is_preferred() {
        let mut env = env();
        env.xdg_runtime_dir = Some(PathBuf::from("/run/user/4242"));

        assert_eq!(
            socket_path_with(&env),
            PathBuf::from("/run/user/4242/hsp-broker.sock")
        );
    }

    #[test]
    fn run_user_dir_prevents_split_broker_when_env_is_stripped() {
        let mut env = env();
        env.run_user_exists = true;

        assert_eq!(
            socket_path_with(&env),
            PathBuf::from("/run/user/4242/hsp-broker.sock")
        );
    }

    #[test]
    fn tmp_fallback_is_per_user() {
        assert_eq!(
            socket_path_with(&env()),
            PathBuf::from("/tmp/hsp-broker-noesis/hsp-broker.sock")
        );
    }

    #[test]
    fn broker_log_path_uses_override_or_state_home_or_home() {
        let mut env = env();
        env.log_override = Some(PathBuf::from("/var/log/hsp.log"));
        assert_eq!(broker_log_path_with(&env), PathBuf::from("/var/log/hsp.log"));

        env.log_override = None;
        env.xdg_state_home = Some(PathBuf::from("/state"));
        assert_eq!(
            broker_log_path_with(&env),
            PathBuf::from("/state/hsp/broker.log")
        );

        env.xdg_state_home = None;
        assert_eq!(
            broker_log_path_with(&env),
            PathBuf::from("/home/noesis/.local/state/hsp/broker.log")
        );
    }

    #[test]
    fn idle_ttl_defaults_to_four_hours() {
        let mut env = env();
        assert_eq!(idle_ttl_seconds_with(&env), DEFAULT_IDLE_TTL_SECONDS);
        env.idle_ttl_seconds = Some(5);
        assert_eq!(idle_ttl_seconds_with(&env), 5);
    }
}

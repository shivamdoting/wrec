use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const SOCKET_NAME: &str = "wrec.sock";
const DAEMON_LOG_NAME: &str = "daemon.log";

// Dev (debug-profile) builds run their own daemon under a separate home so a
// dev daemon never fights the installed one for the socket.
#[cfg(debug_assertions)]
const HOME_DIR_NAME: &str = ".wrec-dev";
#[cfg(not(debug_assertions))]
const HOME_DIR_NAME: &str = ".wrec";

pub fn wrec_home() -> PathBuf {
    std::env::var_os("WREC_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(HOME_DIR_NAME)))
        .unwrap_or_else(|| PathBuf::from(HOME_DIR_NAME))
}

pub fn socket_path() -> PathBuf {
    wrec_home().join(SOCKET_NAME)
}

pub fn daemon_log_path() -> PathBuf {
    wrec_home().join(DAEMON_LOG_NAME)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

use std::path::PathBuf;

use crate::paths::{facetime_container_data, home_dir};

pub fn bridge_ready_lock() -> PathBuf {
    facetime_container_data().join(super::protocol::READY_LOCK)
}

pub fn rpc_inbox() -> PathBuf {
    facetime_container_data()
        .join(super::protocol::RPC_DIR)
        .join(super::protocol::INBOX)
}

pub fn rpc_outbox() -> PathBuf {
    facetime_container_data()
        .join(super::protocol::RPC_DIR)
        .join(super::protocol::OUTBOX)
}

pub fn dylib_search_paths() -> Vec<PathBuf> {
    let name = super::protocol::DEFAULT_DYLIB_NAME;
    let mut paths = Vec::new();
    if let Ok(custom) = std::env::var("RS_FACETIME_BRIDGE_DYLIB") {
        paths.push(PathBuf::from(custom));
    }
    if let Ok(prefix) = std::env::var("HOMEBREW_PREFIX") {
        paths.push(PathBuf::from(prefix).join("lib").join(name));
    }
    paths.push(PathBuf::from("/opt/homebrew/lib").join(name));
    paths.push(PathBuf::from("/usr/local/lib").join(name));
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().unwrap_or(std::path::Path::new("."));
        paths.push(dir.join(name));
        paths.push(dir.join("../lib").join(name));
    }
    let _ = home_dir();
    paths
}

pub fn resolve_dylib() -> Option<PathBuf> {
    dylib_search_paths().into_iter().find(|p| p.is_file())
}

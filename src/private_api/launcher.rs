use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Result, RsFacetimeError};
use crate::private_api::paths::{bridge_ready_lock, resolve_dylib, rpc_inbox, rpc_outbox};
use crate::private_api::sip::require_sip_disabled;

const FACETIME_BIN: &str = "/System/Applications/FaceTime.app/Contents/MacOS/FaceTime";

pub struct Launcher {
    pub dylib_path: PathBuf,
}

impl Launcher {
    pub fn discover() -> Result<Self> {
        let dylib_path = resolve_dylib().ok_or_else(|| {
            RsFacetimeError::PrivateApi(format!(
                "{} not found; build a FaceTime helper dylib or set RS_FACETIME_BRIDGE_DYLIB",
                crate::private_api::protocol::DEFAULT_DYLIB_NAME
            ))
        })?;
        Ok(Self { dylib_path })
    }

    pub fn is_ready(&self) -> bool {
        bridge_ready_lock().is_file()
    }

    pub fn ensure_launched(&self) -> Result<()> {
        if self.is_ready() {
            return Ok(());
        }
        require_sip_disabled()?;
        let _ = Command::new("/usr/bin/killall").arg("FaceTime").status();
        thread::sleep(Duration::from_secs(1));
        let _ = fs::remove_file(bridge_ready_lock());
        ensure_queue_dir(&rpc_inbox())?;
        ensure_queue_dir(&rpc_outbox())?;
        clean_queue_dir(&rpc_inbox())?;
        clean_queue_dir(&rpc_outbox())?;
        let dylib = fs::canonicalize(&self.dylib_path)
            .map_err(|e| RsFacetimeError::PrivateApi(format!("dylib: {e}")))?;
        Command::new(FACETIME_BIN)
            .env("DYLD_INSERT_LIBRARIES", dylib)
            .spawn()
            .map_err(|e| RsFacetimeError::PrivateApi(format!("launch FaceTime: {e}")))?;
        wait_for_ready(Duration::from_secs(20))
    }
}

fn ensure_queue_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|e| RsFacetimeError::PrivateApi(format!("mkdir {}: {e}", path.display())))?;
    Ok(())
}

fn clean_queue_dir(path: &Path) -> Result<()> {
    if !path.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let p = entry?.path();
        if p.is_file() {
            let _ = fs::remove_file(p);
        }
    }
    Ok(())
}

fn wait_for_ready(timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if bridge_ready_lock().is_file() {
            thread::sleep(Duration::from_millis(500));
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(RsFacetimeError::PrivateApi(
        "timeout waiting for FaceTime bridge ready lock".into(),
    ))
}

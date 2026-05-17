use std::process::Command;

use crate::error::{Result, RsFacetimeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SipStatus {
    Enabled,
    Disabled,
    Unknown,
}

pub fn current_sip_status() -> SipStatus {
    let Ok(output) = Command::new("/usr/bin/csrutil").arg("status").output() else {
        return SipStatus::Unknown;
    };
    let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
    if text.contains("disabled") {
        SipStatus::Disabled
    } else if text.contains("enabled") {
        SipStatus::Enabled
    } else {
        SipStatus::Unknown
    }
}

pub fn require_sip_disabled() -> Result<()> {
    match current_sip_status() {
        SipStatus::Disabled => Ok(()),
        SipStatus::Enabled => Err(RsFacetimeError::PrivateApi(
            "SIP must be disabled for FaceTime dylib injection".into(),
        )),
        SipStatus::Unknown => Err(RsFacetimeError::PrivateApi(
            "could not determine SIP status".into(),
        )),
    }
}

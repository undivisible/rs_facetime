//! FaceTime Audio bridge for macOS — **not** bundled with `rs_imsg`.
//!
//! Tier 2: inject a helper dylib into FaceTime.app (SIP off), using a v2
//! file-queue RPC protocol parallel to [openclaw/imsg](https://github.com/openclaw/imsg).

pub mod error;
pub mod paths;

#[cfg(all(target_os = "macos", feature = "private-api"))]
pub mod private_api;

pub use error::{Result, RsFacetimeError};

pub fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "unsupported"
    }
}

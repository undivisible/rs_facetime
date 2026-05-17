pub const PROTOCOL_VERSION: i32 = 2;
pub const RPC_DIR: &str = ".rs-facetime-rpc";
pub const INBOX: &str = "in";
pub const OUTBOX: &str = "out";
pub const READY_LOCK: &str = ".rs-facetime-bridge-ready";
pub const DEFAULT_DYLIB_NAME: &str = "rs-facetime-bridge-helper.dylib";
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeAction {
    Ping,
    Status,
    StartCall,
    EndCall,
}

impl BridgeAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Status => "status",
            Self::StartCall => "start-call",
            Self::EndCall => "end-call",
        }
    }
}

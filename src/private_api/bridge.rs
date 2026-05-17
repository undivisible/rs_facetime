use serde_json::{json, Value};

use crate::error::Result;
use crate::private_api::ipc::{invoke_default, BridgeResponse};
use crate::private_api::launcher::Launcher;
use crate::private_api::protocol::BridgeAction;

pub struct BridgeClient {
    _launcher: Launcher,
}

impl BridgeClient {
    pub fn connect() -> Result<Self> {
        let launcher = Launcher::discover()?;
        launcher.ensure_launched()?;
        Ok(Self {
            _launcher: launcher,
        })
    }

    pub fn is_ready() -> bool {
        Launcher::discover()
            .map(|l| l.is_ready())
            .unwrap_or(false)
    }

    pub fn ping(&self) -> Result<BridgeResponse> {
        self.invoke(BridgeAction::Ping, json!({}))
    }

    pub fn status(&self) -> Result<BridgeResponse> {
        self.invoke(BridgeAction::Status, json!({}))
    }

    pub fn start_call(&self, handle: &str) -> Result<BridgeResponse> {
        self.invoke(
            BridgeAction::StartCall,
            json!({ "handle": handle }),
        )
    }

    pub fn end_call(&self) -> Result<BridgeResponse> {
        self.invoke(BridgeAction::EndCall, json!({}))
    }

    pub fn answer_call(&self, call_uuid: &str) -> Result<BridgeResponse> {
        self.invoke(
            BridgeAction::AnswerCall,
            json!({ "callUUID": call_uuid }),
        )
    }

    pub fn leave_call(&self, call_uuid: &str) -> Result<BridgeResponse> {
        self.invoke(
            BridgeAction::LeaveCall,
            json!({ "callUUID": call_uuid }),
        )
    }

    pub fn invoke(&self, action: BridgeAction, params: Value) -> Result<BridgeResponse> {
        let _ = self;
        invoke_default(action, params)
    }
}

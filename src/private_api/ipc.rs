use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{Result, RsFacetimeError};
use crate::private_api::paths::{rpc_inbox, rpc_outbox};
use crate::private_api::protocol::{BridgeAction, DEFAULT_TIMEOUT_MS, PROTOCOL_VERSION};

#[derive(Debug, Clone)]
pub struct BridgeResponse {
    pub id: String,
    pub success: bool,
    pub data: Value,
    pub error: Option<String>,
}

impl BridgeResponse {
    pub fn parse(raw: &Value) -> Result<Self> {
        let id = raw.get("id").map(stringify_id).unwrap_or_default();
        let success = raw.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
        let error = raw.get("error").and_then(|v| v.as_str()).map(str::to_owned);
        let data = if let Some(d) = raw.get("data") {
            d.clone()
        } else {
            let mut map = raw.as_object().cloned().unwrap_or_default();
            for key in ["v", "id", "success", "error", "timestamp"] {
                map.remove(key);
            }
            Value::Object(map)
        };
        Ok(Self {
            id,
            success,
            data,
            error,
        })
    }
}

fn stringify_id(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub fn invoke_blocking(
    action: BridgeAction,
    params: Value,
    timeout: Duration,
) -> Result<BridgeResponse> {
    let id = Uuid::new_v4().to_string();
    let envelope = json!({
        "v": PROTOCOL_VERSION,
        "id": id,
        "action": action.as_str(),
        "params": params,
    });

    let inbox = rpc_inbox();
    let outbox = rpc_outbox();
    fs::create_dir_all(&inbox).map_err(|e| {
        RsFacetimeError::PrivateApi(format!("mkdir inbox: {e}"))
    })?;
    fs::create_dir_all(&outbox).map_err(|e| {
        RsFacetimeError::PrivateApi(format!("mkdir outbox: {e}"))
    })?;

    let tmp = inbox.join(format!("{id}.tmp"));
    let request_path = inbox.join(format!("{id}.json"));
    let response_path = outbox.join(format!("{id}.json"));

    fs::write(&tmp, serde_json::to_vec(&envelope)?)?;
    fs::rename(&tmp, &request_path)?;

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(data) = fs::read(&response_path) {
            if data.len() > 1 {
                let _ = fs::remove_file(&response_path);
                let raw: Value = serde_json::from_slice(&data)?;
                let response = BridgeResponse::parse(&raw)?;
                if response.success {
                    return Ok(response);
                }
                return Err(RsFacetimeError::PrivateApi(
                    response.error.unwrap_or_else(|| "bridge error".into()),
                ));
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = fs::remove_file(&request_path);
    Err(RsFacetimeError::PrivateApi(format!(
        "timeout waiting for '{}'",
        action.as_str()
    )))
}

pub fn invoke_default(action: BridgeAction, params: Value) -> Result<BridgeResponse> {
    invoke_blocking(
        action,
        params,
        Duration::from_millis(DEFAULT_TIMEOUT_MS),
    )
}

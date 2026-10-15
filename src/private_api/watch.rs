//! Call-state watching built on the existing `status` RPC.
//!
//! This adds no new bridge action and no media/streaming: it polls
//! [`BridgeClient::status`](crate::private_api::BridgeClient::status) and reports
//! when the reported call state changes, so CLI and library users can react to a
//! call starting, ending, or otherwise changing without writing their own polling
//! loop. The change detection ([`StatusWatcher`]) is pure and device-free.

use serde_json::Value;

use crate::private_api::ipc::BridgeResponse;

/// How the call state changed between two successive `status` responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The active-call count increased.
    Started,
    /// The active-call count decreased.
    Ended,
    /// The status data changed but the active-call count did not move (or could
    /// not be determined).
    Changed,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Ended => "ended",
            Self::Changed => "changed",
        }
    }
}

/// A detected transition in the FaceTime call state.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusChange {
    pub kind: ChangeKind,
    pub previous: Option<Value>,
    pub current: Value,
}

/// Detects call-state transitions across successive `status` responses.
///
/// Feed each [`BridgeResponse`] to [`observe`](StatusWatcher::observe); the first
/// observation establishes a baseline and never reports a change. Pure and
/// device-free, so it is unit-testable without a live call or an injected helper.
#[derive(Debug, Default)]
pub struct StatusWatcher {
    last: Option<Value>,
}

impl StatusWatcher {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Observe one `status` response, returning a [`StatusChange`] if the status
    /// data differs from the previously observed response.
    pub fn observe(&mut self, response: &BridgeResponse) -> Option<StatusChange> {
        let current = response.data.clone();
        let change = match &self.last {
            Some(prev) if *prev != current => Some(StatusChange {
                kind: classify(prev, &current),
                previous: Some(prev.clone()),
                current: current.clone(),
            }),
            _ => None,
        };
        self.last = Some(current);
        change
    }
}

/// Best-effort active-call count from a status payload, tolerant of field naming.
fn active_count(data: &Value) -> Option<i64> {
    for key in [
        "activeCalls",
        "activeCallCount",
        "active_call_count",
        "calls",
    ] {
        if let Some(value) = data.get(key) {
            if let Some(n) = value.as_i64() {
                return Some(n);
            }
            if let Some(arr) = value.as_array() {
                return Some(arr.len() as i64);
            }
        }
    }
    None
}

fn classify(previous: &Value, current: &Value) -> ChangeKind {
    match (active_count(previous), active_count(current)) {
        (Some(p), Some(c)) if c > p => ChangeKind::Started,
        (Some(p), Some(c)) if c < p => ChangeKind::Ended,
        _ => ChangeKind::Changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resp(data: Value) -> BridgeResponse {
        BridgeResponse {
            id: "test".into(),
            success: true,
            data,
            error: None,
        }
    }

    #[test]
    fn first_observation_is_baseline_not_a_change() {
        let mut w = StatusWatcher::new();
        assert!(w.observe(&resp(json!({ "activeCalls": 0 }))).is_none());
    }

    #[test]
    fn identical_status_is_not_a_change() {
        let mut w = StatusWatcher::new();
        w.observe(&resp(json!({ "activeCalls": 1 })));
        assert!(w.observe(&resp(json!({ "activeCalls": 1 }))).is_none());
    }

    #[test]
    fn count_increase_is_started() {
        let mut w = StatusWatcher::new();
        w.observe(&resp(json!({ "activeCalls": 0 })));
        let c = w.observe(&resp(json!({ "activeCalls": 1 }))).unwrap();
        assert_eq!(c.kind, ChangeKind::Started);
    }

    #[test]
    fn count_decrease_is_ended() {
        let mut w = StatusWatcher::new();
        w.observe(&resp(json!({ "activeCalls": 2 })));
        let c = w.observe(&resp(json!({ "activeCalls": 0 }))).unwrap();
        assert_eq!(c.kind, ChangeKind::Ended);
        assert_eq!(c.previous, Some(json!({ "activeCalls": 2 })));
        assert_eq!(c.current, json!({ "activeCalls": 0 }));
    }

    #[test]
    fn array_valued_calls_field_is_counted() {
        let mut w = StatusWatcher::new();
        w.observe(&resp(json!({ "calls": [] })));
        let c = w.observe(&resp(json!({ "calls": ["uuid-1"] }))).unwrap();
        assert_eq!(c.kind, ChangeKind::Started);
    }

    #[test]
    fn unknown_shape_change_reports_changed() {
        let mut w = StatusWatcher::new();
        w.observe(&resp(json!({ "bundleId": "com.apple.FaceTime" })));
        let c = w
            .observe(&resp(json!({ "bundleId": "com.apple.other" })))
            .unwrap();
        assert_eq!(c.kind, ChangeKind::Changed);
    }
}

//! Task records retain their existing wire/persistence representation.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::{to_raw_value, RawValue};

pub(super) fn error_result(message: &str) -> Box<RawValue> {
    #[expect(clippy::expect_used, reason = "serializing a string-only JSON error cannot fail")]
    to_raw_value(&super::error(message)).expect("string-only JSON error is serializable")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TaskState {
    Queued,
    Sent,
    Completed,
    AcceptedPendingApply,
    Unknown,
    Fault,
    Expired,
}

impl TaskState {
    pub(super) fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Sent)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Task {
    pub(super) id: u64,
    pub(super) device: String,
    pub(super) method: String,
    pub(super) args: Value,
    pub(super) state: TaskState,
    pub(super) created: u64,
    pub(super) sent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) result: Option<Box<RawValue>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_states_round_trip() {
        for (state, expected_str) in [
            (TaskState::Queued, "queued"),
            (TaskState::Sent, "sent"),
            (TaskState::Completed, "completed"),
            (TaskState::AcceptedPendingApply, "accepted_pending_apply"),
            (TaskState::Unknown, "unknown"),
            (TaskState::Fault, "fault"),
            (TaskState::Expired, "expired"),
        ] {
            let serialized = serde_json::to_string(&state).unwrap();
            assert_eq!(serialized, format!("\"{expected_str}\""));
            let deserialized: TaskState = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, state);
            assert_eq!(state.is_active(), matches!(state, TaskState::Queued | TaskState::Sent));
        }
    }
}

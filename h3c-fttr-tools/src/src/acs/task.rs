//! Task records retain their existing wire/persistence representation.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub(super) enum TaskState {
    Queued,
    Sent,
    Completed,
    AcceptedPendingApply,
    Unknown,
    Fault,
    Expired,
    /// Existing files accepted arbitrary state strings. Preserve those values
    /// rather than rejecting the entire task history during deserialization.
    Other(String),
}

impl TaskState {
    pub(super) fn as_str(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Completed => "completed",
            Self::AcceptedPendingApply => "accepted_pending_apply",
            Self::Unknown => "unknown",
            Self::Fault => "fault",
            Self::Expired => "expired",
            Self::Other(value) => value,
        }
    }

    pub(super) fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Sent)
    }
}

impl From<String> for TaskState {
    fn from(value: String) -> Self {
        match value.as_str() {
            "queued" => Self::Queued,
            "sent" => Self::Sent,
            "completed" => Self::Completed,
            "accepted_pending_apply" => Self::AcceptedPendingApply,
            "unknown" => Self::Unknown,
            "fault" => Self::Fault,
            "expired" => Self::Expired,
            _ => Self::Other(value),
        }
    }
}

impl From<TaskState> for String {
    fn from(state: TaskState) -> Self {
        match state {
            TaskState::Other(value) => value,
            state => state.as_str().to_owned(),
        }
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
    pub(super) result: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_task_records_round_trip_including_unrecognized_states() {
        for state in [
            "queued",
            "sent",
            "completed",
            "accepted_pending_apply",
            "unknown",
            "fault",
            "expired",
            "legacy-state",
            "",
        ] {
            let record = serde_json::json!({
                "id": 7, "device": "child", "method": "GetParameterValues",
                "args": {"names": ["InternetGatewayDevice.DeviceInfo.ModelName"]},
                "state": state, "created": 1, "sent": null, "result": null,
            });
            let task: Task = serde_json::from_value(record.clone()).unwrap();
            assert_eq!(task.state.is_active(), matches!(state, "queued" | "sent"));
            assert_eq!(serde_json::to_value(task).unwrap(), record);
        }
    }
}

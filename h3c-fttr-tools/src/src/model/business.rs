use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusinessPhase {
    #[default]
    NotStarted,
    CollectingIdentity,
    RetryingIdentity,
    Unsupported,
    Configuring,
    Verifying,
    WanBootstrapPending,
    Verified,
    Retrying,
    Offline,
}

impl std::fmt::Display for BusinessPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotStarted => "Not started",
            Self::CollectingIdentity => "Collecting identity",
            Self::RetryingIdentity => "Retrying identity",
            Self::Unsupported => "Adapter not compiled or unsupported device",
            Self::Configuring => "Configuring",
            Self::Verifying => "Verifying",
            Self::WanBootstrapPending => "WAN bootstrap pending",
            Self::Verified => "Verified",
            Self::Retrying => "Retrying",
            Self::Offline => "Offline",
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessStatus {
    pub phase: BusinessPhase,
    pub generation: u64,
    pub error: Option<String>,
}

impl std::fmt::Display for BusinessStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} (generation {})", self.phase, self.generation)?;
        if let Some(error) = &self.error {
            write!(formatter, ": {error}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::BusinessStatus;

    #[test]
    fn deserialization_rejects_unrecognized_or_legacy_worker_state() {
        for value in [
            serde_json::json!("Verified (generation 1)"),
            serde_json::json!({"phase": "Verified", "generation": 1, "error": null}),
            serde_json::json!({"phase": "invented", "generation": 1, "error": null}),
        ] {
            let result = serde_json::from_value::<BusinessStatus>(value);
            assert!(result.is_err());
        }
    }
}

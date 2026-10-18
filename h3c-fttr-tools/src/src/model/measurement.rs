use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Measurements = BTreeMap<String, MeasurementEvidence>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementEvidence {
    pub source: String,
    pub unit: String,
    pub reason: Option<String>,
    pub raw: Option<String>,
    pub age_seconds: Option<u64>,
}

impl MeasurementEvidence {
    pub fn unavailable(source: impl Into<String>, unit: &str, reason: impl Into<String>) -> Self {
        Self {
            source: source.into(), unit: unit.into(), reason: Some(reason.into()),
            raw: None, age_seconds: None,
        }
    }

    pub fn observed(source: impl Into<String>, unit: &str, raw: impl Into<String>) -> Self {
        Self {
            source: source.into(), unit: unit.into(), reason: None,
            raw: Some(raw.into()), age_seconds: Some(0),
        }
    }
}

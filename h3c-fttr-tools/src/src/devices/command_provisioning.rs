//! Backend-neutral asynchronous command job. Device contracts live in their adapters.
use crate::provisioning::model::Device;
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Job(Arc<Mutex<Option<Result<Value, String>>>>);

impl Job {
    pub fn take(&self) -> Option<Result<Value, String>> {
        self.0.lock().unwrap().take()
    }

    pub fn start(
        device: Device,
        apply: bool,
        full: bool,
        revision: String,
        operation_id: u64,
    ) -> Self {
        let job = Self::default();
        let result = job.clone();
        crate::runtime::spawn(async move {
            #[cfg(feature = "mqtt-hl202")]
            let outcome = if device.adapter == "hl202" {
                super::hl202::provisioning::run(device, apply, full, revision, operation_id).await
            } else {
                Err("command provisioning unsupported by this adapter".into())
            };
            #[cfg(not(feature = "mqtt-hl202"))]
            let outcome = {
                let _ = (device, apply, full, revision, operation_id);
                Err("command provisioning not compiled".into())
            };
            *result.0.lock().unwrap() = Some(outcome);
            crate::provisioning::controller::wake();
        });
        job
    }
}

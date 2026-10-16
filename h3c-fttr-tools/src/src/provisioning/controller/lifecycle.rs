use super::*;
use serde::ser::{Serialize, SerializeSeq, Serializer};
use serde::Deserialize;

#[derive(Deserialize)]
struct Recovered {
    operations: Vec<Value>,
    #[serde(default)]
    owned: BTreeMap<String, BTreeMap<String, wan::Owned>>,
}

struct Operations<'a>(&'a Controller);

impl Serialize for Operations<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for (id, recovered) in &self.0.recovered {
            if !self.0.operations.contains_key(id) {
                sequence.serialize_element(recovered)?;
            }
        }
        for operation in self.0.operations.values() {
            sequence.serialize_element(&operation.view())?;
        }
        sequence.end()
    }
}

impl Controller {
    pub(super) fn resolve_pending_identities(&mut self, devices: &Value) {
        let mut resolved = false;
        for d in &mut self.config.devices {
            if d.cwmp_id.starts_with("pending:") && !self.resets.blocks(&d.optical_sn) {
                if let Some(id) = discovered_identity(devices, &d.optical_sn, &d.adapter) {
                    d.cwmp_id = id;
                    resolved = true;
                }
            }
        }
        if resolved {
            self.retain_configured_state();
        }
    }

    pub(super) fn history(&self) -> String {
        #[derive(serde::Serialize)]
        struct History<'a> {
            operations: Operations<'a>,
            owned: &'a BTreeMap<String, BTreeMap<String, wan::Owned>>,
        }
        #[expect(
            clippy::expect_used,
            reason = "JSON values and string-keyed ownership maps serialize infallibly"
        )]
        serde_json::to_string(&History {
            operations: Operations(self),
            owned: &self.owned,
        })
        .expect("controller history is serializable")
    }

    pub(super) fn recover(&mut self, data: &[u8]) {
        if let Ok(recovered) = serde_json::from_slice::<Recovered>(data) {
            self.owned = recovered.owned;
            for mut operation in recovered.operations {
                self.next_id = self
                    .next_id
                    .max(operation["operation_id"].as_u64().unwrap_or(0));
                if operation["optical_sn"]
                    .as_str()
                    .is_some_and(|serial| self.resets.blocks(serial))
                {
                    continue;
                }
                if operation["state"] != "completed" {
                    operation["state"] = json!("interrupted");
                }
                if let Some(id) = operation["device"].as_str().map(str::to_owned) {
                    self.recovered.insert(id, operation);
                }
            }
            self.owned.retain(|id, _| {
                !self.resets.owns(id)
                    && !self
                        .config
                        .devices
                        .iter()
                        .any(|d| &d.cwmp_id == id && self.resets.blocks(&d.optical_sn))
            });
        }
    }

    pub(super) fn replace_config(&mut self, mut config: Config) -> Result<(), String> {
        if self
            .config
            .devices
            .iter()
            .any(|d| self.resets.blocks(&d.optical_sn))
        {
            return Err("device reset pending; retry reset before reload".into());
        }
        if self
            .operations
            .values()
            .any(|op| op.stage != OperationPhase::Completed)
        {
            return Err("active synchronization: finish operation before reload".into());
        }
        for device in &mut config.devices {
            if device.cwmp_id.starts_with("pending:") {
                if let Some(previous) = self.config.devices.iter().find(|previous| {
                    previous.id == device.id
                        && previous.optical_sn == device.optical_sn
                        && previous.adapter == device.adapter
                }) {
                    device.cwmp_id.clone_from(&previous.cwmp_id);
                }
            }
        }
        for previous in &self.config.devices {
            if !config.devices.iter().any(|d| {
                d.id == previous.id
                    && d.optical_sn == previous.optical_sn
                    && d.cwmp_id == previous.cwmp_id
            }) {
                self.management.remove(&previous.id);
                self.last_boot.remove(&previous.id);
                self.last_online.remove(&previous.id);
            }
        }
        self.config = config;
        self.load_reset_intents();
        self.retain_configured_state();
        Ok(())
    }

    pub(super) fn retain_configured_state(&mut self) {
        let devices = &self.config.devices;
        self.operations.retain(|id, op| {
            devices.iter().any(|d| {
                &d.id == id
                    && d.optical_sn == op.device.optical_sn
                    && (d.cwmp_id.starts_with("pending:")
                        || op.device.cwmp_id.starts_with("pending:")
                        || d.cwmp_id == op.device.cwmp_id)
            })
        });
        self.recovered.retain(|id, op| {
            !self.operations.contains_key(id)
                && devices.iter().any(|d| {
                    &d.id == id
                        && op["optical_sn"] == d.optical_sn
                        && (d.cwmp_id.starts_with("pending:") || op["cwmp_id"] == d.cwmp_id)
                })
        });
        self.management
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        self.last_boot
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        self.last_online
            .retain(|id| devices.iter().any(|d| &d.id == id));
        self.omci_management
            .retain(|serial, _| devices.iter().any(|d| &d.optical_sn == serial));
        self.optical_epochs
            .retain(|serial, _| devices.iter().any(|d| &d.optical_sn == serial));
        let retained_binding = |id: &String| {
            devices.iter().any(|d| &d.cwmp_id == id)
                || self
                    .recovered
                    .values()
                    .any(|op| op["cwmp_id"].as_str() == Some(id.as_str()))
                || self.operations.values().any(|op| &op.device.cwmp_id == id)
        };
        self.owned.retain(|id, _| retained_binding(id));
        self.wifi_owned.retain(|id, _| retained_binding(id));
    }
}

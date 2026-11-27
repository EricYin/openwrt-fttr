//! Read-only recovery observations are never the authoritative business result.
use super::*;

impl Controller {
    pub(super) fn reboot_for_recovery(&mut self, target: &str) -> Result<u64, String> {
        if !cfg!(all(
            feature = "diagnostics",
            feature = "device-hl202",
            feature = "mqtt"
        )) {
            return Err("recovery restart requires the diagnostics build".into());
        }
        if !self.config.enabled {
            return Err("provisioning disabled by UCI".into());
        }
        let d = self
            .config
            .devices
            .iter()
            .find(|d| d.id == target || d.optical_sn == target)
            .ok_or("select one configured HL202 device")?;
        let old_id = self
            .recovery_owner(d)
            .and_then(|op| op["operation_id"].as_u64())
            .ok_or("no unambiguous unresolved operation")?;
        let view = self
            .observations
            .get(&d.id)
            .filter(|view| {
                view["observes_operation_id"] == old_id
                    && view["observation"]["inventory_complete"] == true
                    && view["revision"] == self.config.device_revision(&d.id)
                    && view["observation"]["native_pending_operation_id"].is_u64()
                    && view["observation"]["restart_available"] == true
                    && view["completed_at"]
                        .as_u64()
                        .is_some_and(|at| now().checked_sub(at).is_some_and(|age| age <= 180))
            })
            .ok_or("run a fresh complete recovery observation first")?;
        let boot_id = view["observation"]["boot_id"]
            .as_str()
            .ok_or("missing ONU boot ID")?
            .to_owned();
        let id = self.observe_selected(target)?;
        self.operations
            .values_mut()
            .find(|op| op.id == id)
            .unwrap()
            .command_mut()
            .unwrap()
            .reboot_from = Some(boot_id);
        Ok(id)
    }

    pub(super) fn business_operation(&self, id: &str) -> Option<&Operation> {
        self.operations.get(id).filter(|op| op.observes().is_none())
    }

    pub(super) fn recovery_owner(&self, d: &Device) -> Option<&Value> {
        let mut owners = self.recovered.values().filter(|op| {
            op["optical_sn"] == d.optical_sn && facts::Facts::recovered(op).unresolved
        });
        let owner = owners.next()?;
        owners.next().is_none().then_some(owner)
    }

    pub(super) fn observe_selected(&mut self, target: &str) -> Result<u64, String> {
        if !cfg!(all(feature = "device-hl202", feature = "mqtt")) {
            return Err("HL202 recovery observation not compiled".into());
        }
        if self.pending_config.is_some() || self.snapshots.as_ref().is_some_and(|s| s.blocked()) {
            return Err("configuration or recovery storage blocks observation".into());
        }
        let mut devices = self
            .config
            .devices
            .iter()
            .filter(|d| d.id == target || d.optical_sn == target);
        let d = devices.next().ok_or("select one configured HL202 device")?;
        if devices.next().is_some() || d.provisioning_adapter != "hl202-mqtt" {
            return Err("select one configured HL202 device".into());
        }
        if self.operations.contains_key(&d.id)
            || self.resets.blocks(&d.optical_sn)
            || !crate::omci_auth::allowed(&d.optical_sn)
        {
            return Err("active operation, reset, or authorization blocks observation".into());
        }
        if !self.business_ready(d) {
            return Err("waiting for a fresh authorized MQTT binding".into());
        }
        let owner = self
            .recovery_owner(d)
            .and_then(|op| op["operation_id"].as_u64())
            .ok_or("no unambiguous unresolved operation for this optical identity")?;
        self.next_id += 1;
        let mut op = Operation::new(
            self.next_id,
            d.clone(),
            self.config.device_revision(&d.id).into(),
            false,
            false,
        );
        op.command_mut()
            .expect("HL202 owns a command backend")
            .observes = Some(owner);
        self.operations.insert(d.id.clone(), op);
        Ok(self.next_id)
    }

    pub(super) fn observation_view(&self, d: &Device) -> Option<Value> {
        let old_id = self.recovery_owner(d)?["operation_id"].as_u64()?;
        let mut view = if let Some(op) = self
            .operations
            .get(&d.id)
            .filter(|op| op.observes() == Some(old_id))
        {
            op.view()
        } else if let Some(view) = self
            .observations
            .get(&d.id)
            .filter(|v| v["observes_operation_id"] == old_id)
        {
            view.clone()
        } else {
            json!({"state":"not_started","observes_operation_id":old_id,
                "old_execution":"unconfirmed","recovery_state":"manual_required",
                "manual_action":"observe current state; old execution and original intent need independent evidence"})
        };
        view["current_revision"] = json!(view["revision"] == self.config.device_revision(&d.id));
        Some(view)
    }

    pub(super) fn schedule_observation(&mut self, d: &Device, reachable: bool, time: u64) {
        if !reachable
            || !self.config.enabled
            || !self.config.automatic
            || self.operations.contains_key(&d.id)
            || d.provisioning_adapter != "hl202-mqtt"
            || self.recovery_owner(d).is_none()
        {
            return;
        }
        let episode = self.read_recovery.get(&d.id).cloned().unwrap_or_default();
        let recent = self.observations.get(&d.id);
        if recent.is_some_and(|v| {
            v["observation"]["inventory_complete"] == true
                && v["revision"] == self.config.device_revision(&d.id)
        }) {
            return;
        }
        let retry_at = recent
            .and_then(|v| v["completed_at"].as_u64())
            .map_or(0, |at| {
                at.saturating_add(15 << episode.attempts.saturating_sub(1).min(2))
            });
        if episode.attempts < 3 && time >= retry_at && self.observe_selected(&d.id).is_ok() {
            self.read_recovery.entry(d.id.clone()).or_default().attempts += 1;
        }
    }
}

#[cfg(all(test, feature = "device-hl202", feature = "mqtt"))]
mod tests {
    use super::*;

    fn fixture() -> (Controller, Device, Value) {
        let mut d = super::super::tests::operation(OperationPhase::Completed, false).device;
        d.optical_sn = "TEST00000035".into();
        d.provisioning_adapter = "hl202-mqtt".into();
        crate::omci_auth::test_authorize(&d.optical_sn);
        let old = json!({"operation_id":15,"device":d.id,"optical_sn":d.optical_sn,
            "cwmp_id":d.cwmp_id,"revision":"old","mode":"apply","state":"completed",
            "items":[{"id":"wifi","kind":"ssid","state":"unknown"}]});
        let mut c = Controller::default();
        c.config.devices.push(d.clone());
        c.config.enabled = true;
        c.config.automatic = true;
        c.config.interval_seconds = 0;
        c.mqtt.insert(d.id.clone(), json!({"state":"online"}));
        c.next_id = 15;
        c.recovered.insert(d.id.clone(), old.clone());
        (c, d, old)
    }

    #[test]
    fn observation_preserves_durable_unknown_and_does_not_enable_apply() {
        let (mut c, d, old) = fixture();
        c.load_snapshots();
        assert!(c.persist_snapshots());
        let path = crate::provisioning::runtime_store::Record::Recovery.path("");
        let before = crate::provisioning::storage::read(&path).unwrap();
        let id = c.observe_selected(&d.id).unwrap();
        c.retain_configured_state();
        assert_eq!(c.operation_view(15), Some(old.clone()));
        assert_eq!(c.operation_view(id).unwrap()["observes_operation_id"], 15);
        c.operations.get_mut(&d.id).unwrap().finish_command(
            Ok(json!({"inventory_complete":true,"comparison":"match"})),
            100,
        );
        c.compact_completed();
        assert!(c.persist_snapshots());
        assert_eq!(crate::provisioning::storage::read(path).unwrap(), before);
        assert_eq!(c.operation_view(15), Some(old));
        let view = c.observation_view(&d).unwrap();
        assert_eq!(view["observation"]["comparison"], "match");
        assert_eq!(view["old_execution"], "unconfirmed");
        assert_eq!(view["write_performed"], false);
        assert!(c.start_selected(&d.id, true, false, None).is_err());
        assert!(!crate::provisioning::runtime_store::approved(&d.optical_sn));
    }

    #[test]
    fn automatic_observation_has_a_finite_episode_even_without_periodic_sync() {
        let (mut c, d, old) = fixture();
        for time in [100, 115, 145] {
            c.schedule_observation(&d, true, time);
            let op = c.operations.get_mut(&d.id).unwrap();
            assert_eq!(op.observes(), Some(15));
            assert!(!op.apply);
            op.finish_command(Err("inventory timed out".into()), time);
            c.compact_completed();
            c.schedule_observation(&d, true, time + 14);
            assert!(c.operations.is_empty());
        }
        c.schedule_observation(&d, true, 1000);
        assert!(c.operations.is_empty());
        assert_eq!(c.operation_view(15), Some(old));
    }

    #[test]
    #[cfg(feature = "diagnostics")]
    fn recovery_reboot_requires_recent_inventory_for_this_pending_operation() {
        let (mut c, d, old) = fixture();
        assert!(c.reboot_for_recovery(&d.id).is_err());
        let boot = "73d5ff2a-4f34-409a-9577-166c32c0a83b";
        let mut view = json!({"observes_operation_id":14,"completed_at":now(),
            "revision":c.config.device_revision(&d.id),"observation":{"inventory_complete":true,
            "native_pending_operation_id":9,"boot_id":boot,"restart_available":true}});
        c.observations.insert(d.id.clone(), view.clone());
        assert!(c.reboot_for_recovery(&d.id).is_err());
        view["observes_operation_id"] = json!(15);
        view["completed_at"] = json!(now().saturating_sub(181));
        c.observations.insert(d.id.clone(), view.clone());
        assert!(c.reboot_for_recovery(&d.id).is_err());
        view["completed_at"] = json!(now());
        c.observations.insert(d.id.clone(), view);
        let id = c.reboot_for_recovery(&d.id).unwrap();
        let op = c.operations.get(&d.id).unwrap();
        assert_eq!(op.command().unwrap().reboot_from.as_deref(), Some(boot));
        assert_eq!(c.operation_view(id).unwrap()["mode"], "recover_reboot");
        assert!(c.reboot_for_recovery(&d.id).is_err());
        assert_eq!(c.operation_view(15), Some(old));
    }
}

use super::command::{self, CommandResult};
use super::command_transport::{self, Transport};
use crate::mqtt::{MqttClient, MqttEvent};
use crate::mqtt_identity::{Registry, Selection, Snapshot};
use serde::Serialize;
use std::io;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) struct Guarded<'a, T, F> {
    transport: &'a mut T,
    registry: &'a Mutex<Registry>,
    selection: &'a Selection,
    target: &'a str,
    clock: F,
}

impl<T: Transport, F: Fn() -> u64> Transport for Guarded<'_, T, F> {
    fn subscribe(&mut self, topic: &str, timeout: Duration) -> io::Result<()> {
        self.transport.subscribe(topic, timeout)
    }
    fn publish(&mut self, topic: &str, payload: &[u8]) -> io::Result<()> {
        let state = self
            .registry
            .lock()
            .map_err(|_| io::Error::other("MQTT identity state unavailable"))?;
        let snapshot = state
            .resolve(self.selection, (self.clock)())
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        if snapshot.mqtt_mac != self.target || topic != format!("/fttr/subdev/exec/{}", self.target)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MQTT target changed before publication",
            ));
        }
        self.transport.publish(topic, payload)
    }
    fn receive(&mut self, timeout: Duration) -> io::Result<Option<MqttEvent>> {
        self.transport.receive(timeout)
    }
    fn ping(&mut self) -> io::Result<()> {
        self.transport.ping()
    }
}

#[derive(Serialize)]
pub struct SelectedResult {
    pub target: Snapshot,
    pub result: CommandResult,
    pub binding_current: bool,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn execute(
    selection: &Selection,
    script: &str,
    timeout: Duration,
) -> io::Result<SelectedResult> {
    let registry = crate::mqtt_identity::shared();
    let target = registry
        .lock()
        .map_err(|_| io::Error::other("MQTT identity state unavailable"))?
        .resolve(selection, now())
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let _target_lock = command::lock_target(&target.mqtt_mac)?;
    let id = command::request_id();
    let mut mqtt = MqttClient::connect("127.0.0.1:1883", &format!("fttr-exec-{id}"))?;
    let mut guarded = Guarded {
        transport: &mut mqtt,
        registry,
        selection,
        target: &target.mqtt_mac,
        clock: now,
    };
    let result = command_transport::run(
        &mut guarded,
        command_transport::Request {
            target: target.mqtt_mac.clone(),
            script,
            id,
            timeout,
        },
    )?;
    let binding_current = registry
        .lock()
        .map_err(|_| io::Error::other("MQTT identity state unavailable"))?
        .resolve(selection, now())
        .is_ok();
    Ok(SelectedResult {
        target,
        result,
        binding_current,
    })
}

#[cfg(test)]
mod tests {
    use super::super::command_transport::Transport;
    use super::*;
    use crate::mqtt::MqttEvent;
    use crate::mqtt_identity::{Registry, Selection};
    use std::collections::BTreeMap;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Broker {
        registry: Arc<Mutex<Registry>>,
        publications: usize,
    }

    impl Transport for Broker {
        fn subscribe(&mut self, _: &str, _: Duration) -> io::Result<()> {
            self.registry.lock().unwrap().update_topology(
                BTreeMap::from([(2, "H3CT87654321".into())]),
                Ok(BTreeMap::new()),
                100,
            );
            Ok(())
        }
        fn publish(&mut self, _: &str, _: &[u8]) -> io::Result<()> {
            self.publications += 1;
            Ok(())
        }
        fn receive(&mut self, _: Duration) -> io::Result<Option<MqttEvent>> {
            Ok(None)
        }
        fn ping(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn mqtt_identity_changed_after_subscribe_prevents_any_publication() {
        let registry = Arc::new(Mutex::new(Registry::new("daemon-a".into())));
        {
            let mut state = registry.lock().unwrap();
            state.observe("02AABBCCDDEE", 100);
            state.update_topology(
                BTreeMap::from([(2, "H3CT12345678".into())]),
                Ok(BTreeMap::from([(
                    "02AABBCCDDEE".into(),
                    "H3CT12345678".into(),
                )])),
                100,
            );
        }
        let selection = Selection {
            optical_sn: "H3CT12345678".into(),
            onu_id: 2,
            binding_generation: registry.lock().unwrap().snapshots(100)[0]
                .binding_generation
                .clone(),
        };
        let mut broker = Broker {
            registry: registry.clone(),
            publications: 0,
        };
        let mut guarded = Guarded {
            transport: &mut broker,
            registry: &registry,
            selection: &selection,
            target: "02AABBCCDDEE",
            clock: || 100,
        };
        let result = super::super::command_transport::run(
            &mut guarded,
            super::super::command_transport::Request {
                target: "02AABBCCDDEE".into(),
                script: "printf verified",
                id: "request".into(),
                timeout: Duration::from_secs(1),
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(broker.publications, 0);
    }
}

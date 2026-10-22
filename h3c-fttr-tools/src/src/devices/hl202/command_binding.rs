use super::{command::{self, CommandResult}, command_transport};
use crate::mqtt::session::CommandSession;
use crate::mqtt_identity::{Registry, Selection, Snapshot};
use serde::Serialize;
use std::{io, sync::Mutex, time::{Duration, SystemTime, UNIX_EPOCH}};

fn enqueue_guarded(
    session: &mut CommandSession, registry: &Mutex<Registry>, selection: &Selection,
    target: &str, topic: &str, payload: &[u8], now: u64,
) -> io::Result<()> {
    let state = registry.lock().map_err(|_| io::Error::other("MQTT identity state unavailable"))?;
    let snapshot = state.resolve(selection, now).map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    if snapshot.mqtt_mac != target || topic != format!("/fttr/subdev/exec/{target}") {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "MQTT target changed before publication"));
    }
    // Resolve and nonblocking enqueue are atomic against rebinding. No await
    // occurs with this lock held; a full queue is a terminal local error.
    command_transport::enqueue(session, topic, payload)
}

#[derive(Serialize)]
pub struct SelectedResult {
    pub target: Snapshot,
    pub result: CommandResult,
    pub binding_current: bool,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub fn execute(selection: &Selection, script: &str, timeout: Duration) -> io::Result<SelectedResult> {
    if script.trim().is_empty() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "command must not be empty")); }
    execute_at("127.0.0.1:1883".parse().unwrap(), crate::mqtt_identity::shared(), selection, script, timeout, now)
}

fn execute_at(address: std::net::SocketAddr, registry: &Mutex<Registry>, selection: &Selection, script: &str, timeout: Duration, clock: impl Fn() -> u64) -> io::Result<SelectedResult> {
    let target = registry.lock().map_err(|_| io::Error::other("MQTT identity state unavailable"))?
        .resolve(selection, clock()).map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let _target_lock = command::lock_target(&target.mqtt_mac)?;
    let id = command::request_id();
    let result = command_transport::blocking(command_transport::run(address, command_transport::Request {
        target: target.mqtt_mac.clone(), script, id, timeout,
    }, |session, topic, payload| enqueue_guarded(session, registry, selection, &target.mqtt_mac, topic, payload, clock())))?;
    let binding_current = registry.lock().map_err(|_| io::Error::other("MQTT identity state unavailable"))?.resolve(selection, clock()).is_ok();
    Ok(SelectedResult { target, result, binding_current })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqtt::test_peer::{self, read, write};
    use rumqttc::{Packet, QoS, SubAck, SubscribeReasonCode};
    use std::{collections::BTreeMap, io::Read, sync::Arc};

    #[test]
    fn command_binding_reply_reports_rebound_without_retargeting() {
        let registry = Arc::new(Mutex::new(Registry::new("daemon-b".into())));
        {
            let mut state = registry.lock().unwrap();
            state.observe("02AABBCCDDEF", 100);
            state.update_topology(BTreeMap::from([(3, "H3CT12345679".into())]), Ok(BTreeMap::from([("02AABBCCDDEF".into(), "H3CT12345679".into())])), 100);
        }
        let selection = Selection { optical_sn: "H3CT12345679".into(), onu_id: 3,
            binding_generation: registry.lock().unwrap().snapshots(100)[0].binding_generation.clone() };
        let peer_registry = registry.clone();
        let (address, worker) = test_peer::spawn(move |mut stream| {
            let Packet::Subscribe(subscription) = read(&mut stream).unwrap() else { panic!("expected subscription") };
            write(&mut stream, Packet::SubAck(SubAck::new(subscription.pkid, vec![SubscribeReasonCode::Success(QoS::AtMostOnce)])), false);
            let Packet::Publish(published) = read(&mut stream).unwrap() else { panic!("expected publication") };
            assert_eq!(published.topic, "/fttr/subdev/exec/02AABBCCDDEF");
            let script = std::str::from_utf8(&published.payload).unwrap();
            let id = script.split("__FTTR_BEGIN_").nth(1).unwrap().split("__").next().unwrap();
            peer_registry.lock().unwrap().update_topology(BTreeMap::new(), Ok(BTreeMap::new()), 100);
            write(&mut stream, Packet::Publish(rumqttc::Publish::new(format!("{}/resp", published.topic), QoS::AtMostOnce,
                format!("__FTTR_BEGIN_{id}__\nfixture\n__FTTR_END_{id}__=7\n"))), false);
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let result = execute_at(address, &registry, &selection, "fixture", Duration::from_secs(1), || 100).unwrap();
        assert_eq!(result.target.mqtt_mac, "02AABBCCDDEF");
        assert_eq!(result.result.exit_code, Some(7));
        assert!(!result.binding_current);
        worker.join().unwrap();
    }

    #[test]
    fn mqtt_identity_changed_after_subscribe_prevents_any_publication() {
        let registry = Arc::new(Mutex::new(Registry::new("daemon-a".into())));
        {
            let mut state = registry.lock().unwrap();
            state.observe("02AABBCCDDEE", 100);
            state.update_topology(BTreeMap::from([(2, "H3CT12345678".into())]), Ok(BTreeMap::from([("02AABBCCDDEE".into(), "H3CT12345678".into())])), 100);
        }
        let selection = Selection { optical_sn: "H3CT12345678".into(), onu_id: 2,
            binding_generation: registry.lock().unwrap().snapshots(100)[0].binding_generation.clone() };
        let peer_registry = registry.clone();
        let (address, worker) = test_peer::spawn(move |mut stream| {
            let Packet::Subscribe(subscription) = read(&mut stream).unwrap() else { panic!("expected subscription") };
            peer_registry.lock().unwrap().update_topology(BTreeMap::from([(2, "H3CT87654321".into())]), Ok(BTreeMap::new()), 100);
            write(&mut stream, Packet::SubAck(SubAck::new(subscription.pkid, vec![SubscribeReasonCode::Success(QoS::AtMostOnce)])), true);
            assert_eq!(stream.read(&mut [0]).unwrap(), 0); // No command publication.
        });
        let result = execute_at(address, &registry, &selection, "fixture", Duration::from_secs(1), || 100);
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidInput);
        worker.join().unwrap();
    }
}

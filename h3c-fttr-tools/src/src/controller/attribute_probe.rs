//! Bounded, identity-bound field experiments, serialized with the health worker.
use crate::pon_engine::omci::{schema::MeRegistry, FmcsTransport, OmciSession};
use std::{io, sync::{Arc, atomic::AtomicBool}, time::{Duration, Instant}};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Command {
    pub onu_id: u8,
    pub serial: String,
    pub generation: u64,
    pub class: u16,
    pub instance: u16,
    pub mask: u16,
    #[serde(default)]
    pub expected: Option<Vec<u8>>,
    #[serde(default)]
    pub replacement: Option<Vec<u8>>,
}
pub(super) struct Request {
    pub command: Command,
    pub deadline: Instant,
    pub reply: std::sync::mpsc::Sender<io::Result<serde_json::Value>>,
}
impl Command {
    fn width(&self) -> io::Result<usize> {
        let attr = MeRegistry::get(self.class)
            .filter(|def| def.layout_supported)
            .and_then(|def| def.attributes.iter().find(|attr| attr.mask == self.mask))
            .filter(|attr| attr.read && attr.size > 0 && attr.size <= 25)
            .ok_or_else(|| io::Error::other("requires one readable, fixed-width attribute"))?;
        let width = usize::from(attr.size);
        match (&self.expected, &self.replacement) {
            (None, None) => {},
            (Some(before), Some(after)) if attr.write && before.len() == width && after.len() == width => {},
            _ => return Err(io::Error::other("SET requires writable attribute and exact-width expected/replacement bytes")),
        }
        Ok(width)
    }
    fn current(&self, states: &super::service::States) -> bool {
        super::service::operation_confirmed(self.onu_id)
            && super::service::onu_serial(self.onu_id).as_deref() == Some(self.serial.as_str())
            && states.lock().unwrap().get(&self.onu_id).is_some_and(|s| s.generation == self.generation)
    }
}
pub(super) fn control(command: Command, states: &super::service::States) -> io::Result<serde_json::Value> {
    command.width()?;
    if !command.current(states) { return Err(io::Error::other("ONU identity/registration generation mismatch")); }
    let (sender, wake) = states.lock().unwrap().get(&command.onu_id)
        .and_then(|s| Some((s.mib.clone()?, s.wake.clone()?)))
        .ok_or_else(|| io::Error::other("ONU has no active monitor"))?;
    let (reply, receive) = std::sync::mpsc::channel();
    sender.try_send(super::inventory::WorkerRequest::Attribute(Request {
        command, deadline: Instant::now() + Duration::from_secs(600), reply,
    })).map_err(|_| io::Error::other("ONU diagnostic queue busy or stopped"))?;
    wake.notify();
    receive.recv_timeout(Duration::from_secs(605)).map_err(io::Error::other)?
}
pub(super) fn run(request: &Request, cancellation: Option<Arc<AtomicBool>>) -> io::Result<serde_json::Value> {
    let command = &request.command;
    let width = command.width()?;
    // Called only by this generation's worker under its ONU/OMCC/channel leases.
    if Instant::now() >= request.deadline || super::service::onu_serial(command.onu_id).as_deref() != Some(command.serial.as_str()) {
        return Err(io::Error::other("diagnostic expired or ONU identity changed"));
    }
    let mut session = OmciSession::new(FmcsTransport::open(command.onu_id)?);
    let id = command.onu_id;
    let serial = command.serial.clone();
    session.guard_registration(move || super::service::operation_confirmed(id)
        && super::service::onu_serial(id).as_deref() == Some(serial.as_str()));
    if let Some(token) = cancellation { session.cancel_when(token); }
    session.finish_before(request.deadline.min(Instant::now() + Duration::from_secs(30)));
    let before = session.get(command.class, command.instance, command.mask)?.payload[3..3+width].to_vec();
    if let Some(after) = &command.replacement {
        if command.expected.as_ref() != Some(&before) { return Err(io::Error::other("compare-before-SET mismatch; no write performed")); }
        session.set(command.class, command.instance, command.mask, after)?;
        let readback = session.get(command.class, command.instance, command.mask);
        if !readback.as_ref().is_ok_and(|frame| &frame.payload[3..3+width] == after.as_slice()) {
            let restore = session.set(command.class, command.instance, command.mask, &before)
                .and_then(|()| session.get(command.class, command.instance, command.mask))
                .and_then(|frame| if frame.payload[3..3+width] == before { Ok(()) } else { Err(io::Error::other("restore readback mismatch")) });
            return Err(io::Error::other(format!("SET readback failed; restore={restore:?}")));
        }
    }
    Ok(serde_json::json!({"before":before,"after":command.replacement,"readback_ok":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command() -> Command {
        serde_json::from_value(serde_json::json!({"onu_id":1,"serial":"<ONU_A_SN>",
            "generation":2,"class":268,"instance":257,"mask":4096})).unwrap()
    }
    #[test]
    fn probe_requires_one_known_attribute_and_exact_compare_before_set() {
        let mut c = command();
        assert_eq!(c.width().unwrap(), 2);
        c.replacement = Some(vec![0x80, 0]);
        assert!(c.width().is_err());
        c.expected = Some(vec![0x80, 1]);
        assert_eq!(c.width().unwrap(), 2);
        c.replacement = Some(vec![0]);
        assert!(c.width().is_err());
        c.replacement = None;
        c.expected = None;
        c.mask = 0x3000;
        assert!(c.width().is_err());
    }
    #[test]
    fn probe_rejects_writes_to_read_only_attributes() {
        let mut c = command();
        c.class = 277;
        c.mask = 0x4000; // MaximumQueueSize: read only.
        assert_eq!(c.width().unwrap(), 2);
        c.expected = Some(vec![0, 0]);
        c.replacement = Some(vec![0, 1]);
        assert!(c.width().is_err());
    }
}

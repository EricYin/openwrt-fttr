//! Read-only identity and complete baseline MIB upload; no MIB reset/provisioning.
use crate::pon_engine::omci_transport::{OmciSession, StockOmccLease};
use std::io;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

/// Fixed-width G.988 strings are ASCII, padded by NUL/space (sometimes 0xff).
fn text(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|byte| *byte == 0 || *byte == 0xff).unwrap_or(bytes.len());
    let value = std::str::from_utf8(&bytes[..end]).ok()?.trim();
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_graphic() || byte == b' '))
        .then(|| value.to_string())
}

pub(super) fn collect(id: u8, cancellation: Arc<AtomicBool>) -> io::Result<crate::model::OnuIdentity> {
    let _lock = crate::pon_engine::omci_transport::lock_onu(id)?;
    if cancellation.load(Ordering::Acquire) { return Err(io::Error::new(io::ErrorKind::Interrupted, "identity collection cancelled")); }
    let mut session = OmciSession::open(id)?;
    session.require_operation();
    session.cancel_when(cancellation.clone());
    let _lease = StockOmccLease::open(id)?;
    let _channel = ChannelLease { id, previous: crate::bosa::exchange_omci_channel(id, 0)? };
    let mut identity = crate::model::OnuIdentity::default();
    for (class, instance, mask, label) in [(257,0,0x8000,"equipment"), (256,0,0x4000,"hardware"),
        (257,0,0x4000,"OMCC"), (7,0,0x8000,"software image 0"), (7,1,0x8000,"software image 1")] {
        if cancellation.load(Ordering::Acquire) { return Err(io::Error::new(io::ErrorKind::Interrupted, "identity collection cancelled")); }
        match session.get(class, instance, mask) {
            Ok(frame) => match label {
                "equipment" => identity.equipment_id = text(&frame.payload[3..23]),
                "hardware" => identity.hardware_version = text(&frame.payload[3..17]),
                "OMCC" => identity.omcc_version = Some(frame.payload[3]),
                _ => if let Some(version) = text(&frame.payload[3..17]) { identity.software_images.push(format!("image {instance}: {version}")); },
            },
            Err(error) if error.kind() == io::ErrorKind::NotConnected => return Err(error),
            Err(error) => identity.errors.push(format!("{label}: {error}")),
        }
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_width_identity_strings_do_not_turn_padding_or_binary_into_a_model() {
        assert_eq!(text(b"K155-11\0garbage"), Some("K155-11".into()));
        assert_eq!(text(b"  V5R023C10S500  "), Some("V5R023C10S500".into()));
        assert_eq!(text(&[0xff;20]), None);
        assert_eq!(text(&[1,2,3]), None);
        assert_eq!(text(b"\0\0"), None);
    }
}
struct ChannelLease { id: u8, previous: u16 }
impl Drop for ChannelLease {
    fn drop(&mut self) { let _ = crate::bosa::exchange_omci_channel(self.id, self.previous); }
}
pub(super) struct MibRequest {
    pub summary: bool,
    pub deadline: std::time::Instant,
    pub reply: std::sync::mpsc::Sender<io::Result<String>>,
}

pub fn read(id: u8, summary: bool) -> io::Result<()> {
    if !(1..=15).contains(&id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "ONU_ID must be 1..15"));
    }
    let response = crate::ipc::send_ipc_command(&format!("MIB {id} {summary}"))?;
    if response.trim().is_empty() {
        return Err(io::Error::other("MIB IPC is unsupported by this daemon; update fttrd together with fttrctl"));
    }
    let response: serde_json::Value = serde_json::from_str(&response).map_err(io::Error::other)?;
    if let Some(error) = response["error"].as_str() {
        return Err(io::Error::other(error.to_string()));
    }
    let output = response["output"].as_str().ok_or_else(|| io::Error::other("invalid MIB IPC response; update fttrd together with fttrctl"))?;
    print!("{output}");
    Ok(())
}

/// Run under the caller's ONU lock and existing OMCC/channel leases.
pub(super) fn read_owned(id: u8, summary: bool, cancellation: Option<Arc<AtomicBool>>, deadline: std::time::Instant) -> io::Result<String> {
    use std::fmt::Write;
    if !crate::pon_engine::omci_transport::operation_confirmed(id) {
        return Err(io::Error::new(io::ErrorKind::NotConnected, "read-only MIB requires a currently registered ONU"));
    }
    let mut session = OmciSession::open(id)?;
    session.require_operation();
    session.finish_before(deadline);
    if let Some(token) = cancellation { session.cancel_when(token); }
    let mut output = String::new();
    for (class, mask, label) in [(256,0x8000,"vendor"),(256,0x2000,"serial"),(257,0x8000,"equipment"),(257,0x4000,"OMCC version")] {
        match session.get(class,0,mask) {
            Ok(frame) => writeln!(output, "IDENTITY {label} data={:02X?}",&frame.payload[3..]).unwrap(),
            Err(e) => writeln!(output, "IDENTITY {label} unavailable: {e}").unwrap(),
        }
    }
    let records = session.upload_mib()?;
    if summary {
        let mut classes = std::collections::BTreeMap::new();
        for record in &records { *classes.entry(record.class).or_insert(0usize) += 1; }
        for (class, count) in classes { writeln!(output, "MIB_CLASS class={class} records={count}").unwrap(); }
        for record in &records {
            if [7,11,45,47,84,130,131,171,256,257,262,266,268,272,277,329,65530].contains(&record.class) {
                writeln!(output, "MIB_ENTITY class={} instance={:04x} mask={:04x} data={:02X?}",record.class,record.instance,record.mask,record.data).unwrap();
            }
        }
    } else { writeln!(output, "MIB_JSON {}",serde_json::to_string(&records).map_err(io::Error::other)?).unwrap(); }
    writeln!(output, "RESULT MIB complete=true records={}; no OMCI ME configuration was written",records.len()).unwrap();
    Ok(output)
}

pub(super) fn control(id: u8, summary: bool, states: &super::service::States) -> io::Result<String> {
    if !(1..=15).contains(&id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "ONU_ID must be 1..15"));
    }
    if !crate::pon_engine::omci_transport::operation_confirmed(id) {
        return Err(io::Error::new(io::ErrorKind::NotConnected, "read-only MIB requires a currently registered ONU"));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    match crate::pon_engine::omci_transport::lock_onu(id) {
        Ok(_lock) => {
            if !crate::pon_engine::omci_transport::operation_confirmed(id) {
                return Err(io::Error::new(io::ErrorKind::NotConnected, "read-only MIB requires a currently registered ONU"));
            }
            let _lease = StockOmccLease::open(id)?;
            let _channel = ChannelLease { id, previous: crate::bosa::exchange_omci_channel(id, 0)? };
            read_owned(id, summary, None, deadline)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            let sender = states.lock().unwrap().get(&id).and_then(|state| state.mib.clone())
                .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "ONU OMCI busy; no active monitor available"))?;
            let (reply, receive) = std::sync::mpsc::channel();
            sender.try_send(MibRequest { summary, deadline, reply })
                .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "ONU MIB queue busy or worker stopped; retry shortly"))?;
            receive.recv_timeout(std::time::Duration::from_secs(125))
                .map_err(|error| io::Error::other(format!("MIB worker unavailable: {error}")))?
        }
        Err(error) => Err(error),
    }
}

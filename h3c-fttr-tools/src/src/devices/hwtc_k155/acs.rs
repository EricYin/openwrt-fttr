//! Opt-in standard G.988 TR-069 URL association for K155 VEIP 0601.
use crate::{controller::business::{create_verified, verify}, pon_engine::omci_transport::{OmciSession, is_unknown_instance}};
use std::io;
fn management_tag(value: &str) -> io::Result<[u8; 2]> {
    match value.parse::<u16>() {
        Ok(0) => Ok([0xff, 0xff]),
        Ok(vlan @ 1..=4094) => Ok(vlan.to_be_bytes()),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "K155 ACS VLAN must be 0 (untagged) or 1..4094")),
    }
}
pub fn configure(session: &mut OmciSession, url: &str) -> io::Result<()> {
    let tag = management_tag(&std::env::var("FTTR_ACS_K155_VLAN").unwrap_or_else(|_| "0".into()))?;
    if std::env::var("FTTR_ACS_USERNAME").is_ok_and(|name| !name.is_empty()) {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "K155 ACS bootstrap does not provision authentication ME 148; use an unauthenticated endpoint or configure CPE credentials separately"));
    }
    if !url.starts_with("http://") || url.len() > 374 || !url.is_ascii() || url.bytes().any(|c| c.is_ascii_control()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "ACS URL must be ASCII HTTP, <=374 bytes"));
    }
    let before = session.get(340, 0x601, 0xe000)?;
    let original = before.payload[3..8].to_vec();
    if original != [1,0xff,0xff,0xff,0xff] && !(original[0] <= 1 && original[1..3] == [0x80,1]) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "existing K155 ACS association is not owned by fttr; preserved"));
    }
    // These IDs are local to this adapter; never replace a foreign instance.
    match session.get(157, 0x200, 0x8000) {
        Ok(frame) => {
            let count = frame.payload[3] as usize;
            let mut old = Vec::new();
            if count > 15 { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid existing large string")); }
            for n in 0..count { let part = session.get(157,0x200,0x4000 >> n)?; old.extend_from_slice(&part.payload[3..28]); }
            while old.last()==Some(&0) { old.pop(); }
            if old != url.as_bytes() { return Err(io::Error::new(io::ErrorKind::AlreadyExists, "existing ACS large string differs; preserved")); }
        }
        Err(e) if is_unknown_instance(&e) => {
            session.create(157,0x200,&[])?;
            for (n,part) in url.as_bytes().chunks(25).enumerate() {
                let mut data = [0;25]; data[..part.len()].copy_from_slice(part);
                session.set(157,0x200,0x4000 >> n,&data)?;
                verify(session,157,0x200,0x4000 >> n,&data)?;
            }
            let count = url.len().div_ceil(25) as u8;
            session.set(157,0x200,0x8000,&[count])?; verify(session,157,0x200,0x8000,&[count])?;
        }
        Err(e) => return Err(e),
    }
    create_verified(session,137,0x8001,&[0,0,2,0],0xc000,&[0,0,2,0])?;
    let association = [0x80, 1, tag[0], tag[1]];
    let result = (|| {
        session.set(340,0x601,0x6000,&association)?;
        verify(session,340,0x601,0x6000,&association)?;
        session.set(340,0x601,0x8000,&[0])?;
        verify(session,340,0x601,0xe000,&[0,0x80,1,tag[0],tag[1]])
    })();
    if result.is_err() { let _ = session.set(340,0x601,0xe000,&original); }
    result?;
    println!("K155_ACS_ASSOCIATION_READBACK_OK tag=0x{:02x}{:02x}; a real Inform is still required", tag[0], tag[1]);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vlan_tci_is_priority_zero_and_rejects_reserved_vid() {
        assert_eq!(management_tag("0").unwrap(), [0xff,0xff]);
        assert_eq!(management_tag("46").unwrap(), [0,46]);
        assert_eq!(management_tag("4094").unwrap(), [0x0f,0xfe]);
        for bad in ["4095", "65535", "-1", "", "46x"] { assert!(management_tag(bad).is_err()); }
    }
}

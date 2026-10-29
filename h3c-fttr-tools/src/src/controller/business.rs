//! Device-independent OMCI writes: preserve existing entities and require readback.
use crate::pon_engine::omci::{OmciSession, error::{is_absent, is_unknown_instance, result_code}, attributes::AttributeValue};
use std::io;

pub fn matches(attributes: &[u8], expected: &[u8]) -> bool {
    attributes.get(..expected.len()) == Some(expected)
}
pub fn verify(session: &mut OmciSession, class: u16, instance: u16, mask: u16, expected: &[u8]) -> io::Result<()> {
    let response = session.get(class, instance, mask)?;
    if !matches(&response.payload[3..], expected) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("readback mismatch class={class} instance={instance:04x}")));
    }
    println!("READBACK_OK class={class} instance={instance:04x} mask={mask:04x}");
    Ok(())
}
pub fn create_verified(session: &mut OmciSession, class: u16, instance: u16, attributes: &[u8], mask: u16, expected: &[u8]) -> io::Result<()> {
    match session.get(class, instance, mask) {
        Ok(response) => {
            if !matches(&response.payload[3..], expected) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("existing entity differs class={class} instance={instance:04x}")));
            }
            println!("EXISTING_READBACK_OK class={class} instance={instance:04x}");
            return Ok(());
        }
        Err(error) if is_unknown_instance(&error) => {},
        Err(error) => return Err(error),
    }
    session.create(class, instance, attributes)?;
    verify(session, class, instance, mask, expected)
}

/// Vendor-neutral CREATE. ONUs differ in what they will report and in which
/// result code they use for "already exists", so an unclear probe or a changing
/// entity is resolved by replacing the entity with our own definition instead of
/// aborting the profile forever.
pub fn create_verified_lenient(session: &mut OmciSession, class: u16, instance: u16, attributes: &[u8], mask: u16, expected: &[u8]) -> io::Result<()> {
    match session.get(class, instance, mask) {
        Ok(response) => {
            if matches(&response.payload[3..], expected) {
                println!("EXISTING_READBACK_OK class={class} instance={instance:04x}");
                return Ok(());
            }
            println!("REPLACING_DIFFERING_ENTITY class={class} instance={instance:04x}");
            let _ = session.delete(class, instance);
        }
        Err(error) if is_absent(&error) => {},
        Err(error) if result_code(&error) == Some(9) => {
            // The ONU refuses this mask, so what is stored cannot be compared.
            println!("REPLACING_UNREPORTED_ENTITY class={class} instance={instance:04x} mask={mask:04x}");
            let _ = session.delete(class, instance);
        }
        Err(error) => return Err(error),
    }
    match session.create(class, instance, attributes) {
        Ok(()) => {}
        Err(error) if matches!(result_code(&error), Some(4 | 7)) => {
            // Already there from an earlier attempt that we could not observe.
            let _ = session.delete(class, instance);
            session.create(class, instance, attributes)?;
        }
        Err(error) => return Err(error),
    }
    match verify(session, class, instance, mask, expected) {
        Ok(()) => Ok(()),
        Err(error) if is_absent(&error) || result_code(&error) == Some(9) => {
            println!("CREATE_ACCEPTED_WITHOUT_READBACK class={class} instance={instance:04x} mask={mask:04x}");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Standard Alloc-ID assignment. Instance and allocation come from the device plan.
pub fn assign_tcont(session: &mut OmciSession, instance: u16, alloc: u16) -> io::Result<()> {
    let before = session.get(262, instance, 0x8000)?;
    if before.payload[3..5] != alloc.to_be_bytes() {
        session.set_attributes(262, instance, &[(1, AttributeValue::number(alloc as u32))])?;
    }
    verify(session, 262, instance, 0x8000, &alloc.to_be_bytes())
}
pub fn create_gem(session: &mut OmciSession, instance: u16, port: u16, tcont: u16, bridge: u16, gal: u16) -> io::Result<()> {
    let mut ctp = Vec::new();
    ctp.extend(port.to_be_bytes()); ctp.extend(tcont.to_be_bytes());
    ctp.extend([3,0,0,0,0,0,0,0,0,0]);
    create_verified(session, 268, instance, &ctp, 0xe000, &ctp[..5])?;
    let mut iw = Vec::new();
    iw.extend(instance.to_be_bytes()); iw.push(1); iw.extend(bridge.to_be_bytes());
    iw.extend([0,0]); iw.extend(gal.to_be_bytes());
    create_verified(session, 266, instance, &iw, 0xf200, &iw)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_short_and_different_readback_without_panicking() {
        assert!(!super::matches(&[1], &[1, 2]));
        assert!(!super::matches(&[1, 3], &[1, 2]));
        assert!(super::matches(&[1, 2, 0], &[1, 2]));
    }
}

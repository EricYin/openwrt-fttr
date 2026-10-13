//! Identity and cache scope are independent of the OMCI adapter enum.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    pub optical_vendor: String,
    pub optical_serial: String,
    pub cwmp_serial: String,
    pub product_class: String,
    pub software_version: String,
    pub hardware_version: String,
}

pub fn associated(identity: &Identity) -> bool {
    let sn = &identity.optical_serial;
    if sn.len() != 12 || !sn.is_ascii() {
        return false;
    }
    let hex = format!(
        "{}{}",
        sn[..4]
            .bytes()
            .map(|b| format!("{b:02X}"))
            .collect::<String>(),
        &sn[4..]
    );
    if identity.optical_vendor == "H3CT" {
        identity
            .cwmp_serial
            .to_ascii_uppercase()
            .contains(&sn.to_ascii_uppercase())
    } else {
        identity.cwmp_serial.eq_ignore_ascii_case(&hex)
            || identity.cwmp_serial.eq_ignore_ascii_case(sn)
    }
}
pub fn specialization(identity: &Identity) -> Option<&'static super::contract::Contract> {
    if identity.optical_serial.is_empty() || identity.cwmp_serial.is_empty() {
        return None;
    }
    super::discovery::specialization(&identity.optical_vendor, &identity.product_class)
}

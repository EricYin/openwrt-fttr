//! ZTE vendor dispatch. Model-specific behavior belongs in its model module.
pub mod g1612;

pub fn matches(vendor: &str, equipment: Option<&str>, serial: Option<&str>) -> bool {
    let is_zte_vendor = vendor.eq_ignore_ascii_case("9866")
        || vendor.eq_ignore_ascii_case("ZTEG")
        || vendor.eq_ignore_ascii_case("ZTE");
    let is_zte_serial = serial.is_some_and(|sn| {
        sn.starts_with("9866") || sn.starts_with("ZTEG") || sn.starts_with("zte")
    });
    let is_zte_equipment = equipment
        .is_some_and(|eq| eq.contains("ZXHN") || eq.contains("ZTE") || eq.contains("G1612"));

    if !is_zte_vendor && !is_zte_serial && !is_zte_equipment {
        return false;
    }

    g1612::matches(vendor, equipment, serial)
}

pub fn matches_cwmp(optical_sn: &str, product_class: &str, serial: &str) -> bool {
    let is_zte_sn = optical_sn.starts_with("9866") || optical_sn.starts_with("ZTEG");
    let is_zte_product = product_class.contains("G1612") || product_class.contains("ZXHN");
    if !is_zte_sn && !is_zte_product {
        return false;
    }

    g1612::matches_cwmp(optical_sn, product_class, serial)
}

pub fn configure(
    id: u8,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<()> {
    g1612::configure(id, cancellation)
}

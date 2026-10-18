use crate::model::{DataPathStatus, MeasurementEvidence, Measurements, OnuOmciInventory};
use std::{collections::BTreeMap, fs, path::Path, sync::{Mutex, OnceLock}, time::{Duration, Instant}};

struct Counters {
    at: Instant,
    tx: Option<u64>,
    rx: Option<u64>,
}

pub(in crate::controller) fn data_path(id: u8, registration: &str, inventory: Option<OnuOmciInventory>) -> DataPathStatus {
    let mut status = collect(Path::new("/sys/class/net"), id, registration);
    if let Some(inventory) = inventory {
        status.gem_ports = inventory.gem_ports.iter().map(|port| port.port_id).collect();
        for field in ["gem_ports", "onu_inventory"] {
            let mut evidence = MeasurementEvidence::observed("fresh ONU OMCI MIB upload; not OLT forwarding proof",
                "ONU configuration", format!("{} GEM CTPs; {} VLAN filters; {} VLAN operation tables",
                    inventory.gem_ports.len(), inventory.vlan_filters.len(), inventory.vlan_operations.len()));
            evidence.age_seconds = Some(inventory.age_seconds);
            if field == "gem_ports" && status.gem_ports.is_none() {
                evidence.reason = Some("gem_port_attribute_missing_or_invalid".into());
            }
            status.measurements.insert(field.into(), evidence);
        }
        status.onu_inventory = Some(inventory);
    }
    status
}

fn counter(path: &Path) -> (Option<u64>, MeasurementEvidence) {
    let source = path.display().to_string();
    match fs::read_to_string(path) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(value) => (Some(value), MeasurementEvidence::observed(source, "bytes", raw.trim())),
            Err(error) => (None, MeasurementEvidence::unavailable(source, "bytes", format!("invalid_counter: {error}"))),
        },
        Err(error) => (None, MeasurementEvidence::unavailable(source, "bytes", format!("read_failed: {error}"))),
    }
}

fn rate(current: Option<u64>, previous: Option<u64>, elapsed: Duration) -> Option<f64> {
    if elapsed < Duration::from_secs(1) { return None; }
    let delta = current?.checked_sub(previous?)?;
    Some(delta.to_string().parse::<f64>().ok()? * 8.0 / elapsed.as_secs_f64() / 1000.0)
}

fn collect(root: &Path, id: u8, registration: &str) -> DataPathStatus {
    static SAMPLES: OnceLock<Mutex<BTreeMap<String, Counters>>> = OnceLock::new();
    let interface = format!("fttr{id}");
    let base = root.join(&interface).join("statistics");
    let mut measurements = Measurements::new();
    let (tx_bytes, tx) = counter(&base.join("tx_bytes"));
    let (rx_bytes, rx) = counter(&base.join("rx_bytes"));
    measurements.insert("tx_bytes".into(), tx);
    measurements.insert("rx_bytes".into(), rx);
    let mut current_tx_kbps = None;
    let mut current_rx_kbps = None;
    if let Ok(mut samples) = SAMPLES.get_or_init(|| Mutex::new(BTreeMap::new())).lock() {
        let key = format!("{}:{interface}:{registration}", root.display());
        let previous = samples.get(&key);
        if let Some(previous) = previous {
            current_tx_kbps = rate(tx_bytes, previous.tx, previous.at.elapsed());
            current_rx_kbps = rate(rx_bytes, previous.rx, previous.at.elapsed());
        }
        if previous.is_none_or(|sample| sample.at.elapsed() >= Duration::from_secs(1)) {
            samples.insert(key, Counters { at: Instant::now(), tx: tx_bytes, rx: rx_bytes });
        }
    }
    for (name, value) in [("current_tx_kbps", current_tx_kbps), ("current_rx_kbps", current_rx_kbps)] {
        let evidence = match value {
            Some(value) => MeasurementEvidence::observed(format!("{}/counter_delta", base.display()), "kbit/s", value.to_string()),
            None => MeasurementEvidence::unavailable(format!("{}/counter_delta", base.display()), "kbit/s", "requires_two_valid_monotonic_samples_at_least_one_second_apart"),
        };
        measurements.insert(name.into(), evidence);
    }
    for field in ["gem_ports", "onu_inventory"] {
        measurements.insert(field.into(), MeasurementEvidence::unavailable("verified data-path inventory", "identifier", "verified_inventory_unavailable"));
    }
    measurements.insert("vlan_id".into(), MeasurementEvidence::unavailable("ONU VLAN rule tables", "VID",
        "vlan_rules_are_not_a_single_business_vlan; see onu_inventory"));
    DataPathStatus { interface, gem_ports: None, vlan_id: None, onu_inventory: None, tx_bytes, rx_bytes,
        current_tx_kbps, current_rx_kbps, measurements }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_traffic_when_counters_unchanged() {
        let previous = Some(42);
        let actual = rate(Some(42), previous, Duration::from_secs(2));
        assert_eq!(actual, Some(0.0));
    }

    #[test]
    fn unavailable_when_counter_reset() {
        let previous = Some(42);
        let actual = rate(Some(2), previous, Duration::from_secs(2));
        assert_eq!(actual, None);
    }

    #[test]
    fn converts_bytes_to_decimal_kilobits_when_sampled() {
        let elapsed = Duration::from_secs(2);
        let actual = rate(Some(1500), Some(500), elapsed);
        assert_eq!(actual, Some(4.0));
    }
}

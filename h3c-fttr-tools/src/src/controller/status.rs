use crate::{bosa, fpga, ipc::SubDevice, model::{FttrSystemStatus, MeasurementEvidence,
    SubGatewayStatus, SwitchPortStatus, TelemetryGatewayInfo, WifiMeshStatus},
    pon_engine::{PonEngine, ploam::OnuState}};
use std::{fs, time::{SystemTime, UNIX_EPOCH}};

pub(super) fn snapshot(engine: &PonEngine, services: &super::service::States,
    sources: (&[SubDevice], serde_json::Value)) -> FttrSystemStatus {
    let (devs, provisioning) = sources;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let switch_ports = (1..=16u8).map(|port| {
        let interface = format!("fttr{port}");
        let onu = engine.active_onus.iter().find(|onu| onu.onu_id == port);
        let carrier = onu.is_some_and(|onu| onu.state == OnuState::O5Operation)
            && fs::read_to_string(format!("/sys/class/net/{interface}/carrier"))
                .is_ok_and(|value| value.trim() == "1");
        let (onu_id, onu_sn, link_state) = match onu {
            Some(onu) => {
                let state = match onu.state {
                    OnuState::O1Initial => "Offline (identity retained)",
                    OnuState::O2Standby => "O2Standby",
                    OnuState::O3SerialNumber => "O3SerialNumber",
                    OnuState::O4Ranging if onu.eq_delay.is_some() => "EqD sent; O5 unconfirmed",
                    OnuState::O4Ranging => "Awaiting ranging (O4)",
                    OnuState::O5Operation => "O5 (protocol confirmed)",
                    OnuState::O6IntermittentLODS => "O6IntermittentLODS",
                    OnuState::O7EmergencyStop => "O7EmergencyStop",
                };
                (Some(port), Some(onu.full_sn_str()), state.to_string())
            }
            None => (None, None, "Offline".into()),
        };
        SwitchPortStatus { port, interface, carrier, status: if carrier { "UP" } else { "DOWN" }.into(),
            onu_id, onu_sn, link_state }
    }).collect();
    let sub_gateways = engine.active_onus.iter().map(|onu| {
        let service = services.lock().unwrap().get(&onu.onu_id).cloned();
        let identity = service.as_ref().and_then(|state| state.identity.clone());
        let sn_str = onu.full_sn_str();
        let desired = provisioning["devices"].as_array().and_then(|a| a.iter().find(|d| d["optical_sn"] == sn_str)).cloned().unwrap_or_default();
        let mut telemetry = super::telemetry::snapshot(onu.onu_id, onu.state == OnuState::O5Operation);
        for (field, unit, reason) in [
            ("fiber_distance_m", "m", "calibrated_propagation_measurement_unavailable"),
            ("ranging_delay_rtd_ns", "ns", "equalization_delay_is_not_round_trip_propagation_delay"),
            ("connected_clients", "stations", "station_inventory_unavailable"),
        ] {
            telemetry.measurements.insert(field.into(), MeasurementEvidence::unavailable("FTTR", unit, reason));
        }
        let equalization_delay_bits = onu.eq_delay;
        telemetry.measurements.insert("equalization_delay_bits".into(), match equalization_delay_bits {
            Some(bits) => MeasurementEvidence::observed(format!("FMCS directed ranging register 0x{:08x}",
                0x01010020u32 + 4 * u32::from(onu.onu_id)), "bits", bits.to_string()),
            None => MeasurementEvidence::unavailable("FMCS directed ranging", "bits", "ranging_measurement_unavailable"),
        });
        SubGatewayStatus {
            onu_id: onu.onu_id,
            vendor: onu.vendor_str(),
            model: identity.as_ref().and_then(|value| value.equipment_id.clone()).unwrap_or_else(|| onu.model.clone()),
            serial_number: sn_str.clone(),
            state: onu.state,
            management: desired["management"].clone(),
            provisioning: desired["provisioning"].clone(),
            services: desired["services"].as_array().cloned().unwrap_or_default(),
            ssids: desired["ssids"].as_array().cloned().unwrap_or_default(),
            wifi_sync: desired["wifi_sync"].as_str().unwrap_or("not_configured").into(),
            business_status: services.lock().unwrap().get(&onu.onu_id).map(|state| {
                let verified = fs::read_to_string(format!("/sys/class/net/fttr{}/carrier", onu.onu_id)).is_ok_and(|value| value.trim() == "1");
                state.business_status(verified, onu.state == OnuState::O5Operation && desired["management"]["state"] == "online")
            }).unwrap_or_default(),
            identity: identity.clone(),
            identity_age_seconds: service.as_ref().and_then(|state| state.identity_at).map(|at| at.elapsed().as_secs()),
            identity_current: onu.state == OnuState::O5Operation && identity.is_some(),
            fiber_distance_m: None,
            ranging_delay_rtd_ns: None,
            equalization_delay_bits,
            optical_rx_power_dbm: telemetry.rx,
            optical_tx_power_dbm: telemetry.tx,
            firmware_version: identity.as_ref().filter(|value| !value.software_images.is_empty())
                .map(|value| value.software_images.join("; ")).unwrap_or_else(|| onu.firmware_version.clone()),
            hardware_version: identity.as_ref().and_then(|value| value.hardware_version.clone()).unwrap_or_else(|| onu.hardware_version.clone()),
            uptime_seconds: telemetry.uptime,
            data_path: super::telemetry::data_path(onu.onu_id, &sn_str,
                telemetry.inventory.as_ref().and_then(|sample| sample.current())),
            wifi_mesh: WifiMeshStatus {
                channel_2g: None, channel_5g: None, bandwidth_5g: None, tx_power_pct: None,
                measurements: [("channel_2g", "channel"), ("channel_5g", "channel"),
                    ("bandwidth_5g", "MHz"), ("tx_power_pct", "percent")].into_iter().map(|(field, unit)|
                    (field.into(), MeasurementEvidence::unavailable("ONU radio telemetry", unit,
                        "verified_radio_report_unavailable"))).collect(),
            },
            connected_clients: None,
            measurements: telemetry.measurements,
        }
    }).collect();
    let telemetry_gateways = devs.iter().map(|dev| TelemetryGatewayInfo {
        id: dev.id.clone(), last_seen_sec_ago: now.saturating_sub(dev.last_seen), last_payload_bytes: dev.payload_bytes,
    }).collect();
    FttrSystemStatus { provisioning, fpga_status: fpga::get_fpga_status().to_string(),
        optical_transceiver: bosa::get_optical_status(), switch_ports, sub_gateways, telemetry_gateways }
}

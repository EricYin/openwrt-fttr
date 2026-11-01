use crate::model::{NpuStatus, PpeFlowEntry};
use std::{fs, path::Path};

/// Parse single line from `/sys/kernel/debug/ppe/bind` or `/sys/kernel/debug/ppe/entries`.
pub fn parse_ppe_entry(line: &str) -> Option<PpeFlowEntry> {
    let mut parts = line.split_whitespace();
    let index = parts.next()?.to_string();
    let state = parts.next()?.to_string();
    if index.len() > 8
        || !index.bytes().all(|b| b.is_ascii_hexdigit())
        || !matches!(state.as_str(), "BND" | "UNB" | "FIN" | "INV")
    {
        return None;
    }
    let mut type_parts = Vec::new();
    let mut next_token = parts.next();
    while let Some(tok) = next_token {
        if tok.contains('=') {
            break;
        }
        type_parts.push(tok);
        next_token = parts.next();
    }
    let pkt_type = type_parts.join(" ");
    if pkt_type.is_empty() {
        return None;
    }
    let mut orig_tuple = String::new();
    let mut new_tuple = String::new();
    let mut eth = String::new();
    let mut vlan = String::new();
    let mut packets = None;
    let mut bytes = None;

    let mut current_token = next_token;
    while let Some(tok) = current_token {
        if let Some((k, v)) = tok.split_once('=') {
            match k {
                "orig" => orig_tuple = v.to_string(),
                "new" => new_tuple = v.to_string(),
                "eth" => eth = v.to_string(),
                "vlan" => vlan = v.to_string(),
                "packets" => packets = v.parse::<u64>().ok(),
                "bytes" => bytes = v.parse::<u64>().ok(),
                _ => {}
            }
        }
        current_token = parts.next();
    }

    let counters_valid = packets.is_some() && bytes.is_some() && pkt_type != "L2B";
    Some(PpeFlowEntry {
        index,
        state,
        pkt_type,
        orig_tuple,
        new_tuple,
        eth,
        vlan,
        packets: packets.unwrap_or(0),
        bytes: bytes.unwrap_or(0),
        // AN7581 deliberately returns zero stats for native L2B entries.
        // Those zeros are placeholders, not measured traffic.
        counters_valid,
        packets_exact: packets.map(|v| v.to_string()).unwrap_or_default(),
        bytes_exact: bytes.map(|v| v.to_string()).unwrap_or_default(),
    })
}

pub(crate) fn parse_uci_option(config: &str, section: &str, option_name: &str) -> Option<String> {
    let prefix1 = format!("fttr.{section}.{option_name}=");
    let prefix2 = format!("{section}.{option_name}=");
    for line in config.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix(&prefix1)
            .or_else(|| trimmed.strip_prefix(&prefix2))
        {
            return Some(rest.trim_matches('\'').trim_matches('"').to_string());
        }
    }

    let mut in_section = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("config") {
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            if tokens.len() >= 2 {
                let sec_type = tokens[1].trim_matches('\'').trim_matches('"');
                let sec_name = tokens
                    .get(2)
                    .map(|s| s.trim_matches('\'').trim_matches('"'))
                    .unwrap_or(sec_type);
                in_section = sec_name == section;
            } else {
                in_section = false;
            }
        } else if in_section && trimmed.starts_with("option") {
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            if tokens.len() >= 3 && tokens[1] == option_name {
                let val = tokens[2..].join(" ");
                return Some(val.trim_matches('\'').trim_matches('"').to_string());
            }
        }
    }
    None
}

/// Collect hardware NPU and offload status from sysfs, debugfs and UCI configuration.
pub fn status_from(
    config_path: &Path,
    ppe_debug_path: &Path,
    module_params_path: &Path,
) -> NpuStatus {
    let mut status = NpuStatus {
        supported: true,
        sampled_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        bridge: "br-lan".into(),
        bridge_offload_enabled: true,
        ..Default::default()
    };

    if let Ok(config) = fs::read_to_string(config_path) {
        status.config_valid = true;
        if let Some(val) = parse_uci_option(&config, "offload", "enabled") {
            status.bridge_offload_enabled = val == "1";
            if val != "0" && val != "1" {
                status.config_valid = false;
                status
                    .errors
                    .push("offload.enabled: expected 0 or 1".into());
            }
        }
        if let Some(b) = parse_uci_option(&config, "offload", "bridge") {
            if b.is_empty()
                || b.len() > 15
                || !b
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || b"_.-".contains(&v))
            {
                status.config_valid = false;
                status
                    .errors
                    .push("offload.bridge: invalid interface name".into());
            }
            status.bridge = b;
        }
    } else {
        status.errors.push("FTTR configuration: read failed".into());
    }

    if let Ok(ppe_cfg) = fs::read_to_string(ppe_debug_path.join("config")) {
        status.engine_attached = ppe_cfg.lines().any(|l| l.trim() == "npu_attached: 1");
        status.engine_valid = ppe_cfg
            .lines()
            .any(|l| matches!(l.trim(), "npu_attached: 0" | "npu_attached: 1"));
    }
    if !status.engine_valid {
        status
            .errors
            .push("PPE config: missing or invalid npu_attached".into());
    }

    if let Ok(val) = fs::read_to_string(module_params_path.join("bridge_offload_mask")) {
        if let Ok(mask) = val.trim().parse::<u16>() {
            status.bridge_offload_mask = mask;
            status.mask_valid = true;
            for i in 0..16 {
                if (mask & (1 << i)) != 0 {
                    status.active_ports.push(format!("fttr{}", i + 1));
                }
            }
        }
    }
    if !status.mask_valid {
        status
            .errors
            .push("bridge_offload_mask: read or parse failed".into());
    }

    if let Ok(val) = fs::read_to_string(module_params_path.join("routed_offload_mask")) {
        if let Ok(mask) = val.trim().parse::<u16>() {
            status.routed_offload_mask = mask;
            status.routed_mask_valid = true;
        }
    }
    if !status.routed_mask_valid {
        status
            .errors
            .push("routed_offload_mask: read or parse failed".into());
    }

    if let Ok(val) = fs::read_to_string(module_params_path.join("bridge_offload_debug")) {
        status.bridge_offload_debug = val.trim().eq_ignore_ascii_case("y") || val.trim() == "1";
        status.debug_valid = matches!(val.trim(), "Y" | "y" | "N" | "n" | "0" | "1");
    }
    if !status.debug_valid {
        status
            .errors
            .push("bridge_offload_debug: read or parse failed".into());
    }

    if let Ok(bind_content) = fs::read_to_string(ppe_debug_path.join("bind")) {
        status.flows_valid = true;
        status.counters_valid = true;
        for line in bind_content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(entry) = parse_ppe_entry(line) {
                if entry.state != "BND" {
                    status.flows_valid = false;
                    continue;
                }
                status.counters_valid &= entry.counters_valid;
                match (
                    status.total_packets.checked_add(entry.packets),
                    status.total_bytes.checked_add(entry.bytes),
                ) {
                    (Some(packets), Some(bytes)) => {
                        status.total_packets = packets;
                        status.total_bytes = bytes;
                    }
                    _ => status.counters_valid = false,
                }
                status.bound_flows.push(entry);
            } else {
                status.flows_valid = false;
            }
        }
    }
    status.counters_valid &= status.flows_valid && status.engine_valid && status.engine_attached;
    if !status.flows_valid {
        status.errors.push("PPE bind: read or parse failed".into());
    }
    if !status.counters_valid {
        status
            .errors
            .push("PPE counters: unavailable, invalid or overflowing".into());
    }
    status.total_packets_exact = status.total_packets.to_string();
    status.total_bytes_exact = status.total_bytes.to_string();
    status.bound_entries = status.bound_flows.len() as u32;

    if let Ok(entries_content) = fs::read_to_string(ppe_debug_path.join("entries")) {
        status.entries_valid = true;
        for line in entries_content.lines().filter(|l| !l.trim().is_empty()) {
            if parse_ppe_entry(line).is_some() {
                status.total_entries += 1;
            } else {
                status.entries_valid = false;
            }
        }
    }
    if !status.entries_valid {
        status
            .errors
            .push("PPE entries: read or parse failed".into());
    }

    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ppe_entry_handles_5t_and_l2b() {
        let line_5t = "003b0 BND IPv4 5T orig=192.0.2.1:5001->192.0.2.2:5001 new=192.0.2.1:5001->192.0.2.2:5001 eth=02:00:00:00:00:01->02:00:00:00:00:02 etype=0800 data=00000000 vlan=259,0 ib1=100001ab ib2=00000000 packets=18239 bytes=27358500";
        let entry = parse_ppe_entry(line_5t).unwrap();
        assert_eq!(entry.index, "003b0");
        assert_eq!(entry.state, "BND");
        assert_eq!(entry.pkt_type, "IPv4 5T");
        assert_eq!(entry.orig_tuple, "192.0.2.1:5001->192.0.2.2:5001");
        assert_eq!(entry.eth, "02:00:00:00:00:01->02:00:00:00:00:02");
        assert_eq!(entry.vlan, "259,0");
        assert_eq!(entry.packets, 18239);
        assert_eq!(entry.bytes, 27358500);

        let line_l2b = "008ec UNB     L2B eth=02:00:00:00:00:01->ff:ff:ff:ff:ff:ff etype=0000 data=00000000 vlan=0,0 ib1=540001ac ib2=00000000 packets=0 bytes=0";
        let entry_l2b = parse_ppe_entry(line_l2b).unwrap();
        assert_eq!(entry_l2b.index, "008ec");
        assert_eq!(entry_l2b.state, "UNB");
        assert_eq!(entry_l2b.pkt_type, "L2B");
        assert_eq!(entry_l2b.eth, "02:00:00:00:00:01->ff:ff:ff:ff:ff:ff");
        assert_eq!(entry_l2b.packets, 0);
    }

    #[test]
    fn unavailable_and_placeholder_counters_are_not_measured_zeroes() {
        assert!(parse_ppe_entry("index state packets bytes").is_none());
        let missing = parse_ppe_entry("00001 BND IPv4 5T orig=a->b").unwrap();
        assert!(!missing.counters_valid);
        let invalid = parse_ppe_entry("00001 BND IPv4 5T packets=oops bytes=1").unwrap();
        assert!(!invalid.counters_valid);
        let l2 = parse_ppe_entry("00001 BND L2B eth=a->b packets=0 bytes=0").unwrap();
        assert!(!l2.counters_valid);
        let large = parse_ppe_entry(
            "00001 BND IPv4 5T packets=9007199254740993 bytes=18446744073709551615",
        )
        .unwrap();
        assert!(large.counters_valid);
        assert_eq!(large.packets_exact, "9007199254740993");
        assert_eq!(large.bytes_exact, "18446744073709551615");
        let absent = Path::new("/nonexistent-fttr-npu-status-test");
        let status = status_from(absent, absent, absent);
        assert!(!status.config_valid && !status.engine_valid && !status.mask_valid);
        assert!(!status.flows_valid && !status.entries_valid && !status.counters_valid);
        assert!(!status.hooks_valid && !status.fw4_hooks_ready);
        assert!(!status.errors.is_empty());
    }

    #[test]
    fn status_from_aggregates_ppe_and_config() {
        let dir = std::env::temp_dir().join(format!("fttr-npu-status-{}", std::process::id()));
        let cfg_path = dir.join("fttr");
        let ppe_dir = dir.join("ppe");
        let mod_dir = dir.join("params");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&ppe_dir).unwrap();
        fs::create_dir_all(&mod_dir).unwrap();

        fs::write(
            &cfg_path,
            "fttr.offload.enabled='1'\nfttr.offload.bridge='br-lan'\n",
        )
        .unwrap();
        fs::write(ppe_dir.join("config"), "npu_attached: 1\n").unwrap();
        fs::write(
            ppe_dir.join("entries"),
            "00001 UNB L2B eth=a->b\n00002 BND IPv4 5T orig=a->b\n00003 FIN IPv4 5T orig=b->a\n",
        )
        .unwrap();
        fs::write(
            ppe_dir.join("bind"),
            "003b0 BND IPv4 5T orig=192.168.1.1:1->192.168.1.2:2 packets=10 bytes=1000\n003b1 BND IPv4 5T orig=192.168.1.2:2->192.168.1.1:1 packets=20 bytes=2000\n",
        )
        .unwrap();
        fs::write(mod_dir.join("bridge_offload_mask"), "14\n").unwrap();
        fs::write(mod_dir.join("routed_offload_mask"), "0\n").unwrap();
        fs::write(mod_dir.join("bridge_offload_debug"), "Y\n").unwrap();

        let s = status_from(&cfg_path, &ppe_dir, &mod_dir);
        assert!(s.supported);
        assert!(s.engine_attached);
        assert!(s.bridge_offload_enabled);
        assert_eq!(s.bridge_offload_mask, 14);
        assert_eq!(s.active_ports, vec!["fttr2", "fttr3", "fttr4"]);
        assert!(s.bridge_offload_debug);
        assert_eq!(s.bridge, "br-lan");
        assert_eq!(s.total_entries, 3);
        assert_eq!(s.bound_entries, 2);
        assert_eq!(s.total_packets, 30);
        assert_eq!(s.total_bytes, 3000);
        assert_eq!(s.bound_flows.len(), 2);
        assert!(
            s.config_valid
                && s.engine_valid
                && s.mask_valid
                && s.flows_valid
                && s.entries_valid
                && s.counters_valid
        );
        assert!(!s.fw4_hooks_ready);

        fs::write(
            ppe_dir.join("bind"),
            "00001 UNB IPv4 5T packets=99 bytes=99\nmalformed\n",
        )
        .unwrap();
        let corrupt = status_from(&cfg_path, &ppe_dir, &mod_dir);
        assert!(!corrupt.flows_valid && !corrupt.counters_valid);
        assert!(corrupt.bound_flows.is_empty());

        fs::write(
            &cfg_path,
            "config offload 'offload'\n\toption enabled '0'\n\toption bridge 'br-fttr'\n",
        )
        .unwrap();
        let s2 = status_from(&cfg_path, &ppe_dir, &mod_dir);
        assert!(!s2.bridge_offload_enabled);
        assert_eq!(s2.bridge, "br-fttr");

        let _ = fs::remove_dir_all(&dir);
    }
}

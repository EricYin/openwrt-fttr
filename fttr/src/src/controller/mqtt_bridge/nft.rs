use std::io::Write;
use std::net::Ipv4Addr;
use std::process::{Command, Stdio};

pub(super) const BRIDGE_TABLE: &str = "fttr_mqtt_redirect";
pub(super) const IP_TABLE: &str = "fttr_mqtt_redirect";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RedirectRule {
    pub interface: String,
    pub client_mac: [u8; 6],
    pub client_ip: Ipv4Addr,
    pub target_gw: Ipv4Addr,
    pub broker_ip: Ipv4Addr,
    pub bridge_mac: [u8; 6],
}

pub(super) fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn valid_interface(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

pub(super) fn render_apply_script(rules: &[RedirectRule]) -> Result<String, String> {
    if rules.is_empty() {
        return Ok(render_delete_script());
    }
    let mut bridge_rules = String::new();
    let mut ip_rules = String::new();
    for rule in rules {
        if !valid_interface(&rule.interface) {
            return Err(format!("invalid ONU interface name: {}", rule.interface));
        }
        let client_mac = format_mac(rule.client_mac);
        let bridge_mac = format_mac(rule.bridge_mac);
        bridge_rules.push_str(&format!(
            "        iifname \"{}\" ether saddr {} ip saddr {} ip daddr {} tcp dport 1883 meta pkttype set host ether daddr set {} counter\n",
            rule.interface, client_mac, rule.client_ip, rule.target_gw, bridge_mac
        ));
        ip_rules.push_str(&format!(
            "        ip saddr {} ip daddr {} tcp dport 1883 counter dnat to {}:1883\n",
            rule.client_ip, rule.target_gw, rule.broker_ip
        ));
    }
    Ok(format!(
        "table bridge {BRIDGE_TABLE}\n\
delete table bridge {BRIDGE_TABLE}\n\
table bridge {BRIDGE_TABLE} {{\n\
    chain prerouting {{\n\
        type filter hook prerouting priority -300; policy accept;\n\
{bridge_rules}\
    }}\n\
}}\n\
table ip {IP_TABLE}\n\
delete table ip {IP_TABLE}\n\
table ip {IP_TABLE} {{\n\
    chain prerouting {{\n\
        type nat hook prerouting priority -100; policy accept;\n\
{ip_rules}\
    }}\n\
}}\n"
    ))
}

pub(super) fn render_delete_script() -> String {
    format!(
        "table bridge {BRIDGE_TABLE}\n\
delete table bridge {BRIDGE_TABLE}\n\
table ip {IP_TABLE}\n\
delete table ip {IP_TABLE}\n"
    )
}

fn run_nft_batch(script: &str) -> Result<(), String> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start nft: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .map_err(|e| format!("write nft script: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait nft: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("nft exited with {}", output.status)
        } else {
            format!("nft failed: {stderr}")
        });
    }
    Ok(())
}

pub(super) fn apply(rules: &[RedirectRule]) -> Result<(), String> {
    let script = render_apply_script(rules)?;
    run_nft_batch(&script)
}

pub(super) fn remove() -> Result<(), String> {
    run_nft_batch(&render_delete_script())
}

pub(super) fn tables_present() -> Result<(bool, bool), String> {
    let output = Command::new("nft")
        .args(["-j", "list", "tables"])
        .output()
        .map_err(|e| format!("cannot list nft tables: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!("nft list tables failed: {stderr}"));
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("parse nft tables: {e}"))?;
    Ok(parse_tables_present(&parsed))
}

fn parse_tables_present(value: &serde_json::Value) -> (bool, bool) {
    let Some(entries) = value["nftables"].as_array() else {
        return (false, false);
    };
    let mut bridge = false;
    let mut ip = false;
    for entry in entries {
        let table = &entry["table"];
        if table["family"] == "bridge" && table["name"] == BRIDGE_TABLE {
            bridge = true;
        }
        if table["family"] == "ip" && table["name"] == IP_TABLE {
            ip = true;
        }
    }
    (bridge, ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_exact_per_device_bridge_and_ip_rules() {
        let rule = RedirectRule {
            interface: "fttr127".into(),
            client_mac: [0x1c, 0x94, 0x68, 0x5d, 0xf9, 0x98],
            client_ip: Ipv4Addr::new(192, 0, 2, 244),
            target_gw: Ipv4Addr::new(192, 0, 2, 1),
            broker_ip: Ipv4Addr::new(192, 0, 2, 44),
            bridge_mac: [0xd4, 0xa0, 0x29, 0x03, 0x20, 0x94],
        };
        let script = render_apply_script(&[rule]).unwrap();
        assert!(script
            .contains("table bridge fttr_mqtt_redirect\ndelete table bridge fttr_mqtt_redirect"));
        assert!(script.contains("iifname \"fttr127\" ether saddr 1c:94:68:5d:f9:98 ip saddr 192.0.2.244 ip daddr 192.0.2.1 tcp dport 1883 meta pkttype set host ether daddr set d4:a0:29:03:20:94 counter"));
        assert!(script.contains("ip saddr 192.0.2.244 ip daddr 192.0.2.1 tcp dport 1883 counter dnat to 192.0.2.44:1883"));
    }

    #[test]
    fn rejects_invalid_interface_names_and_detects_tables() {
        let bad = RedirectRule {
            interface: "fttr1\"; drop".into(),
            client_mac: [2, 0, 0, 0, 0, 1],
            client_ip: Ipv4Addr::new(192, 0, 2, 2),
            target_gw: Ipv4Addr::new(192, 0, 2, 1),
            broker_ip: Ipv4Addr::new(192, 0, 2, 3),
            bridge_mac: [2, 0, 0, 0, 0, 2],
        };
        assert!(render_apply_script(&[bad]).is_err());

        let parsed = serde_json::json!({
            "nftables": [
                {"metainfo": {"version": "1.1.7"}},
                {"table": {"family": "inet", "name": "fw4"}},
                {"table": {"family": "bridge", "name": "fttr_mqtt_redirect"}},
                {"table": {"family": "ip", "name": "fttr_mqtt_redirect"}}
            ]
        });
        assert_eq!(parse_tables_present(&parsed), (true, true));
    }
}

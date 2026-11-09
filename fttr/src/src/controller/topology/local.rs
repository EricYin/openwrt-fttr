//! Read only external ubus services, outside all optical/device locks.
use super::*;
use serde_json::Value;
use std::time::Instant;

#[derive(Default)]
pub(super) struct Local {
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub uptime: Option<f64>,
    pub interfaces: Vec<TopologyInterface>,
    pub uplinks: Vec<TopologyUplink>,
    pub hints: BTreeMap<String, (Option<String>, Vec<String>)>,
    pub associated: Vec<TopologyClient>,
    pub association_complete: bool,
    pub sources: Vec<TopologySource>,
}

struct Reader {
    bus: Option<openwrt_ubus::Ubus>,
    started: Instant,
    sources: Vec<TopologySource>,
}

impl Reader {
    fn call(&mut self, object: &str, method: &str) -> Option<Value> {
        let now = unix_time();
        // Bound aggregate latency on the native dispatch thread; never call fttr itself.
        let remaining = 2000u128.saturating_sub(self.started.elapsed().as_millis());
        let result = if remaining == 0 {
            Err("collection_budget_exhausted")
        } else if let Some(bus) = &mut self.bus {
            bus.call(object, method, None, remaining.min(400) as i32)
                .map_err(|_| "ubus_read_failed")
                .and_then(|value| {
                    let valid = value.is_object()
                        && match (object, method) {
                            ("network.interface", "dump") => value["interface"].is_array(),
                            (_, "get_clients") => value["clients"].is_object(),
                            ("network.wireless", "status") => {
                                value.as_object().is_some_and(|radios| {
                                    radios.values().all(|radio| radio["interfaces"].is_array())
                                })
                            }
                            _ => true,
                        };
                    if valid {
                        Ok(value)
                    } else {
                        Err("invalid_response")
                    }
                })
        } else {
            Err("ubus_unavailable")
        };
        self.sources.push(TopologySource {
            id: format!("{object}.{method}"),
            state: if result.is_ok() {
                TopologySourceState::Ok
            } else {
                TopologySourceState::Unavailable
            },
            sampled_at: now,
            reason: result.as_ref().err().map(|e| (*e).into()),
        });
        result.ok()
    }
}

pub(super) fn collect() -> Local {
    let mut reader = Reader {
        bus: openwrt_ubus::Ubus::connect(None).ok(),
        started: Instant::now(),
        sources: Vec::new(),
    };
    let mut local = collect_with(|object, method| reader.call(object, method));
    local.sources = reader.sources;
    local
}

// Shared by the live collector and private, read-only source-sample validation.
pub(super) fn collect_with(mut read: impl FnMut(&str, &str) -> Option<Value>) -> Local {
    let mut local = Local::default();
    if let Some(board) = read("system", "board") {
        local.model = text(&board, "model");
        local.firmware = board.get("release").and_then(|r| text(r, "description"));
    }
    if let Some(info) = read("system", "info") {
        local.uptime = info
            .get("uptime")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0);
    }
    if let Some(devices) = read("luci-rpc", "getNetworkDevices") {
        local.interfaces = interfaces(&devices);
    }
    if let Some(dump) = read("network.interface", "dump") {
        local.uplinks = uplinks(&dump);
    }
    if let Some(hints) = read("luci-rpc", "getHostHints") {
        for (mac, hint) in hints.as_object().unwrap() {
            if let Some(mac) = normalize_mac(mac) {
                let mut addresses = Vec::new();
                for key in ["ipaddrs", "ip6addrs"] {
                    if let Some(values) = hint.get(key).and_then(Value::as_array) {
                        addresses
                            .extend(values.iter().filter_map(Value::as_str).filter_map(valid_ip));
                    }
                }
                addresses.sort();
                addresses.dedup();
                local.hints.insert(mac, (text(hint, "name"), addresses));
            }
        }
    }
    if let Some(wireless) = read("network.wireless", "status") {
        let mut names = BTreeSet::new();
        for radio in wireless.as_object().unwrap().values() {
            if let Some(interfaces) = radio.get("interfaces").and_then(Value::as_array) {
                for interface in interfaces {
                    if let Some(name) = text(interface, "ifname").filter(|s| valid_interface(s)) {
                        names.insert(name);
                    }
                }
            }
        }
        // Only APs support hostapd. Unsupported/non-AP objects remain explicit failures.
        local.association_complete = true;
        for name in names {
            let object = format!("hostapd.{name}");
            let clients = read(&object, "get_clients");
            if let Some(entries) = clients
                .as_ref()
                .and_then(|v| v.get("clients"))
                .and_then(Value::as_object)
            {
                for (mac, state) in entries {
                    // Do not treat unauthenticated/probing entries as an association.
                    if state.get("assoc").and_then(Value::as_bool) != Some(true) {
                        continue;
                    }
                    if let Some(mac) = normalize_mac(mac) {
                        local.associated.push(TopologyClient {
                            id: format!("client:{mac}"),
                            mac,
                            hostname: None,
                            addresses: Vec::new(),
                            relation: TopologyRelation::AssociatedWith,
                            source: format!("{object}.get_clients"),
                            interface: Some(name.clone()),
                        });
                    }
                }
            } else {
                local.association_complete = false;
            }
        }
    }
    local
}

pub(super) fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "Unknown" && !s.contains('\0'))
        .map(str::to_owned)
}

fn valid_ip(value: &str) -> Option<String> {
    value
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

pub(super) fn valid_interface(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}

pub(super) fn interfaces(devices: &Value) -> Vec<TopologyInterface> {
    devices
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| {
            if !valid_interface(name) {
                return None;
            }
            let link = &value["link"];
            let speed = link["speed"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0);
            let physical = value["devtype"].as_str() == Some("ethernet")
                && value["wireless"].as_bool() != Some(true)
                && !name.starts_with("fttr")
                && value.get("parent").is_none();
            Some(TopologyInterface {
                name: name.clone(),
                mac: value["mac"].as_str().and_then(normalize_mac),
                carrier: link["carrier"]
                    .as_bool()
                    .or_else(|| value["carrier"].as_bool()),
                speed_mbps: if physical { speed } else { None },
                duplex: if physical {
                    text(link, "duplex").filter(|s| s == "full" || s == "half")
                } else {
                    None
                },
                bridge_members: value["ports"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                addresses: addresses(value, &["ipaddrs", "ip6addrs"]),
            })
        })
        .collect()
}

fn addresses(value: &Value, keys: &[&str]) -> Vec<String> {
    let mut result = Vec::new();
    for key in keys {
        if let Some(entries) = value.get(*key).and_then(Value::as_array) {
            result.extend(
                entries
                    .iter()
                    .filter_map(|entry| entry["address"].as_str())
                    .filter_map(valid_ip),
            );
        }
    }
    result.sort();
    result.dedup();
    result
}

pub(super) fn uplinks(dump: &Value) -> Vec<TopologyUplink> {
    dump["interface"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|interface| {
            let has_default = interface["route"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|route| {
                    matches!(route["target"].as_str(), Some("0.0.0.0" | "::"))
                        && route["mask"].as_u64() == Some(0)
                });
            if !has_default {
                return None;
            }
            Some(TopologyUplink {
                interface: text(interface, "interface")?,
                device: text(interface, "l3_device"),
                protocol: text(interface, "proto"),
                up: interface["up"].as_bool()?,
                addresses: addresses(interface, &["ipv4-address", "ipv6-address"]),
            })
        })
        .collect()
}

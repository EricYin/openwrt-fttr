//! Public group/template model. Expanded sections are private compiler output.
use super::config::{self, Config, Section};
use super::model::ItemKind;
use std::collections::{BTreeMap, BTreeSet};

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn emit(out: &mut String, kind: &str, id: &str, s: &Section) {
    out.push_str(&format!("config {} {}\n", quote(kind), quote(id)));
    for (key, values) in &s.options {
        for value in values {
            out.push_str(&format!(
                " {} {} {}\n",
                if values.len() > 1 { "list" } else { "option" },
                quote(key),
                quote(value)
            ));
        }
    }
}

fn hl202_binding_supported(interface: &str) -> bool {
    matches!(
        interface,
        "InternetGatewayDevice.LANDevice.1.LANEthernetInterfaceConfig.2"
            | "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1"
            | "InternetGatewayDevice.LANDevice.1.WLANConfiguration.2"
            | "InternetGatewayDevice.LANDevice.1.WLANConfiguration.6"
    )
}
fn section(kind: &str, values: &[(&str, &str)]) -> Section {
    Section {
        kind: kind.into(),
        options: values
            .iter()
            .map(|(k, v)| (k.to_string(), vec![v.to_string()]))
            .collect(),
    }
}
fn list<'a>(s: &'a Section, key: &str) -> impl Iterator<Item = &'a str> {
    s.options.get(key).into_iter().flatten().map(String::as_str)
}
fn get<'a>(all: &'a [(String, Section)], id: &str, kind: &str) -> Result<&'a Section, String> {
    all.iter()
        .find(|(n, s)| n == id && s.kind == kind)
        .map(|(_, s)| s)
        .ok_or_else(|| format!("unknown {kind}: {id}"))
}
pub fn compile(text: &str) -> Result<Config, String> {
    let mut all = Vec::<(String, Section)>::new();
    for line in text.lines() {
        let w = config::words(line)?;
        if w.is_empty() || w[0].starts_with('#') {
            continue;
        }
        match w[0].as_str() {
            "package" => (),
            "config" if w.len() == 3 => {
                if all.iter().any(|(id, _)| id == &w[2]) {
                    return Err("duplicate section".into());
                }
                if !matches!(
                    w[1].as_str(),
                    "acs"
                        | "provisioning"
                        | "management"
                        | "device"
                        | "group"
                        | "template"
                        | "network"
                        | "wifi"
                ) {
                    return Err(format!("unsupported section: {}", w[1]));
                }
                all.push((
                    w[2].clone(),
                    Section {
                        kind: w[1].clone(),
                        ..Default::default()
                    },
                ));
            }
            "option" | "list" if w.len() == 3 => {
                let s = &mut all.last_mut().ok_or("option without section")?.1;
                if w[0] == "option" {
                    s.options.insert(w[1].clone(), vec![w[2].clone()]);
                } else {
                    s.options
                        .entry(w[1].clone())
                        .or_default()
                        .push(w[2].clone());
                }
            }
            _ => return Err("invalid configuration".into()),
        }
    }
    if all.iter().filter(|(_, s)| s.kind == "management").count() > 1 {
        return Err("only one OMCI management network allowed".into());
    }
    for (_, s) in all.iter().filter(|(_, s)| s.kind == "management") {
        s.number("vlan", 4094)?;
        if !s.get("priority").is_empty() {
            s.number("priority", 7)?;
        }
        if !s.get("acs_url").starts_with("http://") {
            return Err("OMCI ACS URL must use HTTP".into());
        }
    }
    // Validate every template, including ones not assigned to a group yet.
    for (id, s) in &all {
        let allowed: &[&str] = match s.kind.as_str() {
            "device" => &["name", "optical_sn", "adapter", "group", "wifi_cleanup"],
            "group" => &["name", "template"],
            "template" => &["name"],
            "network" => &[
                "name", "template", "type", "vlan", "priority", "ssid", "port",
            ],
            "wifi" => &["template", "ssid", "radio", "security", "key"],
            "management" => &["vlan", "priority", "acs_url"],
            "provisioning" => &["enabled", "mode", "trigger", "interval", "freshness"],
            "acs" => &[
                "enabled",
                "listen",
                "port",
                "path",
                "state_file",
                "username",
                "password",
            ],
            _ => &[],
        };
        if let Some(key) = s.options.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!("{id}: unsupported option {key}"));
        }
        if s.kind == "group" {
            get(&all, s.get("template"), "template")?;
        }
        if matches!(s.kind.as_str(), "network" | "wifi") {
            get(&all, s.get("template"), "template")?;
        }
        if s.kind == "network" {
            if !matches!(s.get("type"), "internet" | "iptv") {
                return Err(format!("{id}: business type must be internet or iptv"));
            }
            s.number("vlan", 4094)?;
            if !s.get("priority").is_empty() {
                s.number("priority", 7)?;
            }
            for wifi in list(s, "ssid") {
                if get(&all, wifi, "wifi")?.get("template") != s.get("template") {
                    return Err(format!("{id}: SSID belongs to another template"));
                }
            }
            for port in list(s, "port") {
                if !matches!(port, "lan1" | "lan2" | "lan3" | "lan4") {
                    return Err(format!("{id}: invalid LAN port"));
                }
            }
        }
        if s.kind == "wifi" {
            if s.get("ssid").is_empty() || s.get("ssid").len() > 32 {
                return Err(format!("{id}: SSID must be 1..32 bytes"));
            }
            if !matches!(s.get("radio"), "2g" | "5g") {
                return Err(format!("{id}: invalid radio"));
            }
            if !matches!(s.get("security"), "" | "wpa2-psk")
                || (s.get("security") == "wpa2-psk" && !(8..=63).contains(&s.get("key").len()))
            {
                return Err(format!("{id}: invalid Wi-Fi security"));
            }
        }
        if s.kind == "device"
            && !matches!(
                s.get("wifi_cleanup"),
                "" | "preserve" | "disable" | "delete"
            )
        {
            return Err(format!(
                "{id}: wifi_cleanup must be preserve, disable or delete"
            ));
        }
    }
    for (template, _) in all.iter().filter(|(_, s)| s.kind == "template") {
        let mut bound = BTreeSet::new();
        for (_, s) in all
            .iter()
            .filter(|(_, s)| s.kind == "network" && s.get("template") == template)
        {
            for endpoint in list(s, "ssid")
                .map(|s| format!("ssid:{s}"))
                .chain(list(s, "port").map(|s| format!("port:{s}")))
            {
                if !bound.insert(endpoint) {
                    return Err(format!("{template}: endpoint bound to multiple networks"));
                }
            }
        }
    }
    let mut expanded = String::new();
    let mut order = BTreeMap::<String, Vec<String>>::new();
    for (id, s) in all
        .iter()
        .filter(|(_, s)| matches!(s.kind.as_str(), "acs" | "provisioning"))
    {
        emit(&mut expanded, &s.kind, id, s);
    }
    for (device, d) in all.iter().filter(|(_, s)| s.kind == "device") {
        let group = get(&all, d.get("group"), "group")?;
        let template_id = group.get("template");
        get(&all, template_id, "template")?;
        let mut dev = d.clone();
        if dev.get("wifi_cleanup").is_empty() {
            dev.options
                .insert("wifi_cleanup".into(), vec!["disable".into()]);
        }
        dev.options.insert(
            "cwmp_id".into(),
            vec![format!(
                "pending:{}",
                d.get("optical_sn").to_ascii_uppercase()
            )],
        );
        emit(&mut expanded, "device", device, &dev);
        let mut wifi_instances = BTreeMap::new();
        let mut slots = [0u32; 2];
        let mut ids = Vec::new();
        for (id, s) in all
            .iter()
            .filter(|(_, s)| s.kind == "wifi" && s.get("template") == template_id)
        {
            let band = if s.get("radio") == "2g" { 0 } else { 1 };
            slots[band] += 1;
            if slots[band] > 4 {
                return Err(format!("{template_id}: too many SSIDs for radio"));
            }
            wifi_instances.insert(id.clone(), slots[band] + if band == 1 { 4 } else { 0 });
            let profile = format!("{device}__profile__{id}");
            let mut p = s.clone();
            p.options
                .entry("enabled".into())
                .or_insert(vec!["1".into()]);
            p.options
                .entry("broadcast".into())
                .or_insert(vec!["1".into()]);
            emit(&mut expanded, "wifi_profile", &profile, &p);
            let name = format!("wifi__{id}");
            ids.push(name.clone());
            emit(
                &mut expanded,
                "ssid",
                &format!("{device}__{name}"),
                &section(
                    "ssid",
                    &[
                        ("device", device),
                        ("profile", &profile),
                        ("radio", s.get("radio")),
                        ("slot", &slots[band].to_string()),
                    ],
                ),
            );
        }
        let mut network_ids = Vec::new();
        let mut bindings = Vec::new();
        for (id, s) in all
            .iter()
            .filter(|(_, s)| s.kind == "network" && s.get("template") == template_id)
        {
            let service = format!("{device}__service__{id}");
            emit(&mut expanded, "service", &service, s);
            let name = format!("{device}__network__{id}");
            network_ids.push(name.clone());
            // These service labels have been read back on the two physical models.
            let label = if s.get("type") == "internet" {
                "INTERNET"
            } else if d.get("adapter") == "hl202" {
                "OTHER"
            } else {
                return Err(format!("{device}: HWTC IPTV service contract not verified"));
            };
            emit(
                &mut expanded,
                "device_service",
                &name,
                &section(
                    "device_service",
                    &[
                        ("device", device),
                        ("service", &service),
                        ("service_list", label),
                        (
                            "enabled",
                            if s.get("enabled").is_empty() {
                                "1"
                            } else {
                                s.get("enabled")
                            },
                        ),
                    ],
                ),
            );
            let interfaces: Vec<String> = list(s, "ssid")
                .map(|id| {
                    format!(
                        "InternetGatewayDevice.LANDevice.1.WLANConfiguration.{}",
                        wifi_instances[id]
                    )
                })
                .chain(list(s, "port").map(|p| {
                    format!(
                        "InternetGatewayDevice.LANDevice.1.LANEthernetInterfaceConfig.{}",
                        &p[3..]
                    )
                }))
                .collect();
            if !interfaces.is_empty() {
                let binding = format!("{device}__binding__{id}");
                bindings.push(binding.clone());
                let mut b = section(
                    "port_binding",
                    &[("device", device), ("device_service", &name)],
                );
                b.options.insert("interface".into(), interfaces);
                emit(&mut expanded, "port_binding", &binding, &b);
            }
        }
        network_ids.extend(ids.into_iter().map(|id| format!("{device}__{id}")));
        network_ids.extend(bindings);
        order.insert(device.clone(), network_ids);
    }
    let mut cfg = config::parse_expanded(&expanded)?;
    if let Some((_, s)) = all.iter().find(|(_, s)| s.kind == "management") {
        cfg.management = Some(config::Management {
            vlan: s.number("vlan", 4094)?,
            priority: if s.get("priority").is_empty() {
                0
            } else {
                s.number("priority", 7)?
            },
            acs_url: s.get("acs_url").into(),
        });
    }
    cfg.groups = all
        .iter()
        .filter(|(_, s)| s.kind == "group")
        .map(|(id, s)| (id.clone(), s.get("template").into()))
        .collect();
    cfg.labels = all
        .iter()
        .map(|(id, s)| {
            (
                id.clone(),
                if !s.get("name").is_empty() {
                    s.get("name").into()
                } else if s.kind == "wifi" {
                    s.get("ssid").into()
                } else if s.kind == "device" {
                    s.get("optical_sn").into()
                } else {
                    id.clone()
                },
            )
        })
        .collect();
    cfg.assignments = all
        .iter()
        .filter(|(_, s)| s.kind == "device")
        .map(|(id, s)| {
            (
                id.clone(),
                (
                    s.get("group").into(),
                    get(&all, s.get("group"), "group")
                        .unwrap()
                        .get("template")
                        .into(),
                ),
            )
        })
        .collect();
    cfg.devices
        .sort_by_key(|d| all.iter().position(|(id, _)| id == &d.id).unwrap());
    for d in &mut cfg.devices {
        d.items
            .sort_by_key(|i| order[&d.id].iter().position(|id| id == &i.id).unwrap());
        // Keep only interfaces verified on HL202; WLAN2/6 were written and
        // read back together (the firmware's extra 10/14 aliases are ignored
        // by readback equivalence, not exposed as configurable endpoints).
        for item in &mut d.items {
            if item.kind == ItemKind::PortBinding
                && d.adapter == "hl202"
                && item
                    .parameters
                    .iter()
                    .any(|p| p.value.split(',').any(|v| !hl202_binding_supported(v)))
            {
                item.blocked =
                    Some("LAN/SSID binding mapping not verified for selected endpoint".into());
            }
        }
    }
    // Do not include listener credentials in the business revision.
    let mut canonical = String::new();
    for (id, s) in &all {
        if s.kind != "acs" {
            emit(&mut canonical, &s.kind, id, s);
        }
    }
    cfg.revision = format!(
        "{:016x}",
        canonical
            .bytes()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64)
                .wrapping_mul(0x100000001b3))
    );
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hl202_internet_bridge_dhcp_and_second_radio_slots_compile_for_binding() {
        let text = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT685DF998'\n option adapter 'hl202'\n option group 'g'\nconfig wifi 'a2'\n option template 't'\n option ssid 'base2'\n option radio '2g'\nconfig wifi 'a5'\n option template 't'\n option ssid 'base5'\n option radio '5g'\nconfig wifi 'b2'\n option template 't'\n option ssid 'TTTT'\n option radio '2g'\nconfig wifi 'b5'\n option template 't'\n option ssid 'YYYY'\n option radio '5g'\nconfig network 'iot'\n option template 't'\n option type 'internet'\n option vlan '66'\n option priority '6'\n list ssid 'b2'\n list ssid 'b5'";
        let cfg = compile(text).unwrap();
        let items = &cfg.devices[0].items;
        let service = items
            .iter()
            .find(|i| i.kind == ItemKind::DeviceService)
            .unwrap();
        for (suffix, value) in [
            (".ConnectionType", "IP_Bridged"),
            (".NATEnabled", "0"),
            (".X_CU_CloudCPEEnable", "1"),
        ] {
            assert!(service
                .parameters
                .iter()
                .any(|p| p.name.ends_with(suffix) && p.value == value));
        }
        let binding = items
            .iter()
            .find(|i| i.kind == ItemKind::PortBinding)
            .unwrap();
        assert!(binding.blocked.is_none());
        assert_eq!(binding.parameters[0].value, "InternetGatewayDevice.LANDevice.1.WLANConfiguration.2,InternetGatewayDevice.LANDevice.1.WLANConfiguration.6");
    }
    #[test]
    fn hl202_binding_contract_accepts_verified_wlan_slots_only() {
        for interface in [
            "InternetGatewayDevice.LANDevice.1.LANEthernetInterfaceConfig.2",
            "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1",
            "InternetGatewayDevice.LANDevice.1.WLANConfiguration.2",
            "InternetGatewayDevice.LANDevice.1.WLANConfiguration.6",
        ] {
            assert!(hl202_binding_supported(interface), "{interface}");
        }
        for interface in [
            "InternetGatewayDevice.LANDevice.1.LANEthernetInterfaceConfig.1",
            "InternetGatewayDevice.LANDevice.1.WLANConfiguration.5",
            "InternetGatewayDevice.LANDevice.1.WLANConfiguration.10",
        ] {
            assert!(!hl202_binding_supported(interface), "{interface}");
        }
    }
    #[test]
    fn wpa2_uses_device_beacon_contract() {
        let text="config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'h'\n option optical_sn 'H3CT685DF998'\n option adapter 'hl202'\n option group 'g'\nconfig device 'k'\n option optical_sn 'HWTC3C6DE5B3'\n option adapter 'hwtc'\n option group 'g'\nconfig wifi 'w'\n option template 't'\n option ssid 'test'\n option radio '2g'\n option security 'wpa2-psk'\n option key 'testpassword'";
        let cfg = compile(text).unwrap();
        for d in &cfg.devices {
            let beacon = d.items[0]
                .parameters
                .iter()
                .find(|p| p.name.ends_with(".BeaconType"))
                .unwrap();
            assert_eq!(
                beacon.value,
                if d.adapter == "hl202" { "WPA2" } else { "11i" }
            );
        }
    }
    #[test]
    fn rejects_legacy_and_acs_management() {
        assert!(compile("config device_service 'old'").is_err());
        assert!(compile("config template 't'\nconfig network 'n'\n option template 't'\n option type 'tr069'\n option vlan '46'").is_err());
    }
    #[test]
    fn enabled_ssid_requires_radio_but_disabled_ssid_does_not_disable_shared_radio() {
        let text="config device 'd'\n option optical_sn 'HWTC3C6DE5B3'\n option cwmp_id 'test:hwtc'\n option adapter 'hwtc'\nconfig wifi_profile 'a'\n option ssid 'enabled'\n option enabled '1'\nconfig wifi_profile 'b'\n option ssid 'disabled'\n option enabled '0'\nconfig ssid 'a'\n option device 'd'\n option profile 'a'\n option radio '2g'\n option slot '1'\nconfig ssid 'b'\n option device 'd'\n option profile 'b'\n option radio '2g'\n option slot '2'";
        let text = text
            .replace("config ssid 'a'", "config ssid 'ssid_a'")
            .replace("config ssid 'b'", "config ssid 'ssid_b'");
        let cfg = config::parse_expanded(&text).unwrap();
        let items = &cfg.devices[0].items;
        assert!(items[0]
            .parameters
            .iter()
            .any(|p| p.name.ends_with(".RadioEnabled") && p.value == "1"));
        assert!(!items[1]
            .parameters
            .iter()
            .any(|p| p.name.ends_with(".RadioEnabled")));
        assert!(items[1]
            .parameters
            .iter()
            .any(|p| p.name.ends_with(".Enable") && p.value == "0"));
    }
    #[test]
    fn rejects_cross_template_binding() {
        assert!(compile("config template 'a'\nconfig template 'b'\nconfig wifi 'w'\n option template 'b'\n option ssid 'x'\n option radio '2g'\nconfig network 'n'\n option template 'a'\n option type 'internet'\n option vlan '1'\n list ssid 'w'").is_err());
    }
    #[test]
    fn creation_order_changes_revision() {
        assert_ne!(
            compile("config template 'z'\nconfig template 'a'")
                .unwrap()
                .revision,
            compile("config template 'a'\nconfig template 'z'")
                .unwrap()
                .revision
        );
    }
    #[test]
    fn wlan_cleanup_defaults_to_disable_and_explicit_policy_is_preserved() {
        let base="config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT685DF998'\n option adapter 'hl202'\n option group 'g'";
        assert_eq!(compile(base).unwrap().devices[0].wifi_cleanup, "disable");
        for policy in ["preserve", "disable", "delete"] {
            let text = format!("{base}\n option wifi_cleanup '{policy}'");
            assert_eq!(compile(&text).unwrap().devices[0].wifi_cleanup, policy);
        }
        assert!(compile(&format!("{base}\n option wifi_cleanup 'remove-everything'")).is_err());
    }
    #[test]
    fn group_expansion_keeps_creation_order_and_no_public_indices() {
        let text="config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT685DF998'\n option adapter 'hl202'\n option group 'g'\nconfig network 'z'\n option template 't'\n option type 'internet'\n option vlan '100'\nconfig network 'a'\n option template 't'\n option type 'iptv'\n option vlan '43'\nconfig wifi 'w5'\n option template 't'\n option ssid 'five'\n option radio '5g'\nconfig wifi 'w2'\n option template 't'\n option ssid 'two'\n option radio '2g'";
        let cfg = compile(text).unwrap();
        let d = &cfg.devices[0];
        assert_eq!(
            d.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            [
                "d__network__z",
                "d__network__a",
                "d__wifi__w5",
                "d__wifi__w2"
            ]
        );
        assert!(d.items[..2].iter().all(|i| i.auto_instance.is_some()));
        assert!(d.items[2].parameters[0]
            .name
            .contains("WLANConfiguration.5."));
        assert_eq!(cfg.assignments["d"], ("g".into(), "t".into()));
        assert!(compile(&format!("{text}\n option instance '1'")).is_err());
    }
    #[test]
    fn one_endpoint_cannot_belong_to_two_networks() {
        assert!(compile("config template 't'\nconfig network 'a'\n option template 't'\n option type 'internet'\n option vlan '1'\n list port 'lan1'\nconfig network 'b'\n option template 't'\n option type 'iptv'\n option vlan '2'\n list port 'lan1'").is_err());
    }
}

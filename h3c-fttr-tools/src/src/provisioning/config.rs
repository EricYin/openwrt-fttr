use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

#[derive(Clone, Default)]
pub struct Section { pub kind: String, pub options: BTreeMap<String, Vec<String>> }
impl Section {
    pub fn get(&self, key: &str) -> &str { self.options.get(key).and_then(|v| v.first()).map(String::as_str).unwrap_or("") }
    pub fn number(&self, key: &str, max: u32) -> Result<u32, String> {
        self.get(key).parse::<u32>().ok().filter(|n| *n <= max).ok_or_else(|| format!("invalid {key}"))
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Parameter { pub name: String, pub value: String, pub kind: String, pub secret: bool }
#[derive(Clone)]
pub struct Item { pub id: String, pub kind: String, pub service_type: String, pub parameters: Vec<Parameter>, pub guard: Option<String>, pub blocked: Option<String>, pub wan_ref: Option<String>, pub auto_instance: Option<u32> }
#[derive(Clone)]
pub struct Device { pub id: String, pub optical_sn: String, pub cwmp_id: String, pub adapter: String, pub items: Vec<Item>, pub wifi_cleanup: String, pub wifi_preserve: BTreeSet<u32> }
#[derive(Clone, Default)]
pub struct Config { pub revision: String, pub enabled: bool, pub automatic: bool, pub triggers: BTreeSet<String>, pub interval: u64, pub freshness: u64, pub devices: Vec<Device>, pub assignments: BTreeMap<String,(String,String)>, pub groups: BTreeMap<String,String>, pub management: Option<Management>, pub labels: BTreeMap<String,String> }
#[derive(Clone, Serialize)]
pub struct Management { pub vlan: u32, pub priority: u32, pub acs_url: String }

// UCI export uses shell quoting, including the '\'' sequence inside strings.
pub(super) fn words(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new(); let mut word = String::new(); let mut quote = None; let mut started = false; let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => word.push(c),
            (_, '\\') => word.push(chars.next().ok_or("dangling escape")?),
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => { quote = Some(c); started = true; },
            (None, c) if c.is_whitespace() => { if started { out.push(std::mem::take(&mut word)); started = false; } },
            (None, c) => { word.push(c); started = true; },
        }
    }
    if quote.is_some() { return Err("unterminated UCI quote".into()); }
    if started { out.push(word); } Ok(out)
}
pub fn load() -> Result<Config, String> {
    let output = Command::new("uci").args(["-q", "-n", "export", "fttr"]).output().map_err(|e| format!("uci: {e}"))?;
    if !output.status.success() { return Err("cannot export /etc/config/fttr".into()); }
    parse(std::str::from_utf8(&output.stdout).map_err(|_| "UCI is not UTF-8")?)
}
pub fn parse(text: &str) -> Result<Config, String> {
    super::templates::compile(text)
}
pub(super) fn parse_expanded(text: &str) -> Result<Config, String> {
    let mut sections = BTreeMap::<String, Section>::new(); let mut current = String::new();
    for line in text.lines() {
        let w = words(line)?; if w.is_empty() || w[0].starts_with('#') { continue; }
        match w[0].as_str() {
            "config" if w.len() >= 2 => {
                current = w.get(2).cloned().unwrap_or_else(|| format!("@{}", sections.len()));
                if sections.insert(current.clone(), Section { kind: w[1].clone(), ..Section::default() }).is_some() { return Err("duplicate section".into()); }
            },
            "option" | "list" if w.len() == 3 => {
                let section = sections.get_mut(&current).ok_or("option without section")?;
                if w[0] == "option" { section.options.insert(w[1].clone(), vec![w[2].clone()]); }
                else { section.options.entry(w[1].clone()).or_default().push(w[2].clone()); }
            },
            "package" => (),
            _ => return Err("invalid UCI export".into()),
        }
    }
    let globals: Vec<_> = sections.values().filter(|s| s.kind == "provisioning").collect();
    if globals.len() > 1 { return Err("only one provisioning section allowed".into()); }
    let mut cfg = Config { freshness: 180, ..Config::default() };
    if let Some(s) = globals.first() {
        cfg.enabled = boolean(s.get("enabled"), false)?;
        cfg.automatic = match s.get("mode") { "" | "manual" => false, "auto" => true, _ => return Err("mode must be manual or auto".into()) };
        if !matches!(s.get("method"), "" | "diff") { return Err("only diff synchronization is supported".into()); }
        cfg.interval = if s.get("interval").is_empty() { 0 } else { s.number("interval", 86400)? as u64 };
        cfg.freshness = if s.get("freshness").is_empty() { 180 } else { s.number("freshness", 86400)? as u64 };
        if cfg.freshness == 0 || (cfg.interval != 0 && cfg.interval < 30) { return Err("freshness must be positive; interval must be 0 or >=30".into()); }
        cfg.triggers = s.options.get("trigger").cloned().unwrap_or_else(|| vec!["config_change".into(), "device_online".into(), "cwmp_boot".into()]).into_iter().collect();
        if cfg.triggers.iter().any(|t| !matches!(t.as_str(), "config_change" | "device_online" | "cwmp_boot" | "periodic")) { return Err("unknown synchronization trigger".into()); }
    }
    let mut sns = BTreeSet::new(); let mut ids = BTreeSet::new();
    for (id, s) in sections.iter().filter(|(_,s)| s.kind == "device") {
        if s.get("optical_sn").len() != 12 || !s.get("optical_sn").is_ascii() || !s.get("optical_sn")[4..].bytes().all(|b| b.is_ascii_hexdigit()) { return Err(format!("{id}: invalid optical_sn")); }
        if s.get("cwmp_id").is_empty() || !sns.insert(s.get("optical_sn").to_ascii_uppercase()) || !ids.insert(s.get("cwmp_id")) { return Err(format!("{id}: missing/duplicate device identity")); }
        let compiled = match s.get("adapter") { "hl202" => cfg!(feature="device-hl202"), "hwtc" => cfg!(feature="device-hwtc"), _ => false };
        if !compiled { return Err(format!("{id}: adapter unknown or not compiled")); }
        let wifi_cleanup=match s.get("wifi_cleanup") { "" | "preserve" => "preserve", "delete" => "delete", "disable" => "disable", _ => return Err(format!("{id}: wifi_cleanup must be preserve, delete or disable")) };
        let wifi_preserve=s.options.get("wifi_preserve").into_iter().flatten().map(|v| v.parse::<u32>().ok().filter(|n| (1..=32).contains(n)).ok_or("invalid wifi_preserve instance")).collect::<Result<BTreeSet<_>,_>>()?;
        cfg.devices.push(Device { id: id.clone(), optical_sn: s.get("optical_sn").to_ascii_uppercase(), cwmp_id: s.get("cwmp_id").into(), adapter: s.get("adapter").into(), items: Vec::new(), wifi_cleanup:wifi_cleanup.into(), wifi_preserve });
    }
    for (id, s) in &sections {
        if !matches!(s.kind.as_str(), "device_service" | "ssid" | "port_binding") { continue; }
        let d = cfg.devices.iter_mut().find(|d| d.id == s.get("device")).ok_or_else(|| format!("{id}: unknown device"))?;
        let mut item = Item { id: id.clone(), kind: s.kind.clone(), service_type: String::new(), parameters: Vec::new(), guard: None, blocked: None, wan_ref: None, auto_instance: None };
        if s.kind == "ssid" {
            let profile = reference(&sections, s.get("profile"), "wifi_profile")?;
            if profile.get("ssid").is_empty() || profile.get("ssid").len() > 32 { return Err(format!("{id}: SSID must be 1..32 bytes")); }
            let slot = s.number("slot", 4)?; if slot == 0 { return Err("slot must be 1..4".into()); }
            let base = match s.get("radio") { "2g" => 0, "5g" => 4, _ => return Err("radio must be 2g or 5g".into()) };
            let instance = if s.get("instance").is_empty() { base + slot } else { s.number("instance", 32)? };
            if instance == 0 { return Err("invalid WLAN instance".into()); }
            let path = format!("InternetGatewayDevice.LANDevice.1.WLANConfiguration.{instance}.");
            add(&mut item, &path, "SSID", profile.get("ssid"), "string", false);
            for field in ["Enable", "BeaconAdvertisementEnabled"] {
                let key = if field == "Enable" { "enabled" } else { "broadcast" };
                if !profile.get(key).is_empty() { add(&mut item, &path, field, if boolean(profile.get(key), true)? {"1"} else {"0"}, "boolean", false); }
            }
            // An enabled SSID also needs its shared radio enabled. Never turn
            // that radio off for a disabled SSID: other SSIDs may still use it.
            if boolean(profile.get("enabled"), true)? {
                add(&mut item, &path, "RadioEnabled", "1", "boolean", false);
            }
            if !profile.get("security").is_empty() {
                if profile.get("security") != "wpa2-psk" { return Err("security must be wpa2-psk".into()); }
                let key = profile.get("key"); if !(8..=63).contains(&key.len()) { return Err("WPA2 key must be 8..63 bytes".into()); }
                // HL202 reports WPA2 and rejects the standard 11i token with
                // CWMP fault 9007; K155 accepts 11i.
                add(&mut item, &path, "BeaconType", if d.adapter == "hl202" { "WPA2" } else { "11i" }, "string", false);
                add(&mut item, &path, "IEEE11iEncryptionModes", "AESEncryption", "string", false);
                add(&mut item, &path, "IEEE11iAuthenticationMode", "PSKAuthentication", "string", false);
                add(&mut item, &path, "PreSharedKey.1.KeyPassphrase", key, "string", true);
            }
            if !s.get("device_service").is_empty() {
                let ds = reference(&sections, s.get("device_service"), "device_service")?;
                if ds.get("device") != d.id { return Err("cross-device SSID service reference".into()); }
                if !sections.values().any(|b| b.kind == "port_binding" && b.get("device_service") == s.get("device_service") && b.options.get("interface").is_some_and(|v| v.contains(&path.trim_end_matches('.').to_string()))) {
                    return Err(format!("{id}: device_service requires explicit port_binding interface for this WLAN object"));
                }
            }
        } else {
            let ds = if s.kind == "device_service" { s } else { reference(&sections, s.get("device_service"), "device_service")? };
            if ds.get("device") != d.id { return Err(format!("{id}: cross-device binding")); }
            let auto=matches!(ds.get("wan_instance"),"" | "auto");
            let instance = if auto { 1000 + sections.keys().position(|key| key == if s.kind == "device_service" {id} else {s.get("device_service")}).unwrap() as u32 } else { ds.number("wan_instance", 64)? }; if instance == 0 { return Err("wan_instance must be positive".into()); }
            item.wan_ref=Some(if s.kind == "device_service" {id.clone()} else {s.get("device_service").into()});
            if auto { item.auto_instance=Some(instance); }
            let root = format!("InternetGatewayDevice.WANDevice.1.WANConnectionDevice.{instance}.");
            let path = format!("{root}WANIPConnection.1.");
            item.guard = Some(format!("{path}{}", if d.adapter == "hl202" { "X_CU_ServiceList" } else { "X_HW_SERVICELIST" }));
            let service = reference(&sections, ds.get("service"), "service")?;
            item.service_type = match service.get("type") { "internet" => "internet", "iptv" => "iptv", _ => return Err("business type must be internet or iptv".into()) }.into();
            if s.kind == "port_binding" {
                if item.service_type == "tr069" { return Err("TR069 management WAN cannot have business port bindings".into()); }
                if d.adapter != "hl202" { item.blocked = Some("HWTC LAN/SSID binding contract not verified".into()); }
                let interfaces = s.options.get("interface").ok_or("port_binding requires interface object paths")?;
                if interfaces.iter().any(|v| !v.starts_with("InternetGatewayDevice.LANDevice.1.") || v.ends_with('.') || v.contains(',')) { return Err("invalid binding interface path".into()); }
                add(&mut item, &path, "X_CU_LanInterface", &interfaces.join(","), "string", false);
            } else {
                if auto && s.get("service_list").is_empty() { return Err(format!("{id}: automatic WAN requires explicit vendor service_list")); }
                if !s.get("service_list").is_empty() && (s.get("service_list").contains("TR069") != (item.service_type == "tr069")) { return Err(format!("{id}: service_list conflicts with management/business role")); }
                let vlan = service.number("vlan", 4094)?; let priority = if service.get("priority").is_empty() { 0 } else { service.number("priority", 7)? };
                let connection_type = "IP_Bridged";
                add(&mut item, &path, "ConnectionType", connection_type, "string", false);
                add(&mut item, &path, "NATEnabled", "0", "boolean", false);
                if !s.get("service_list").is_empty() {
                    add(&mut item, &path, if d.adapter == "hl202" { "X_CU_ServiceList" } else { "X_HW_SERVICELIST" }, s.get("service_list"), "string", false);
                }
                if d.adapter == "hl202" {
                    add(&mut item, &root, "X_CU_VLANEnabled", if vlan == 0 {"0"} else {"1"}, "boolean", false);
                    add(&mut item, &root, "X_CU_VLAN", &vlan.to_string(), "unsignedInt", false);
                    add(&mut item, &root, "X_CU_802_1p", &priority.to_string(), "unsignedInt", false);
                } else {
                    add(&mut item, &path, "X_HW_VLAN", &vlan.to_string(), "unsignedInt", false);
                    add(&mut item, &path, "X_HW_PRI", &priority.to_string(), "unsignedInt", false);
                }
                add(&mut item, &path, "Enable", if boolean(ds.get("enabled"), true)? { "1" } else { "0" }, "boolean", false);
            }
        }
        d.items.push(item);
    }
    for d in &mut cfg.devices {
        d.items.sort_by_key(|i| (if i.service_type == "tr069" { 3 } else { match i.kind.as_str() { "device_service" => 0, "ssid" => 1, _ => 2 } }, i.id.clone()));
        let mut paths = BTreeSet::new();
        for i in &d.items { for p in &i.parameters { if !paths.insert(&p.name) { return Err(format!("{}: duplicate parameter ownership", d.id)); } } }
    }
    // Stable revision over provisioning sections only; ACS credentials never enter it.
    let mut hash = 0xcbf29ce484222325u64;
    for (id,s) in sections.iter().filter(|(_,s)| matches!(s.kind.as_str(), "provisioning" | "device" | "service" | "device_service" | "wifi_profile" | "ssid" | "port_binding")) {
        for byte in format!("{id}\0{}\0{:?}", s.kind, s.options).bytes() { hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3); }
    }
    cfg.revision = format!("{hash:016x}"); Ok(cfg)
}
fn reference<'a>(sections: &'a BTreeMap<String, Section>, id: &str, kind: &str) -> Result<&'a Section, String> {
    sections.get(id).filter(|s| s.kind == kind).ok_or_else(|| format!("missing {kind} reference {id}"))
}
fn boolean(value: &str, default: bool) -> Result<bool, String> {
    match value { "" => Ok(default), "1" | "true" => Ok(true), "0" | "false" => Ok(false), _ => Err("invalid boolean".into()) }
}
fn add(item: &mut Item, path: &str, suffix: &str, value: &str, kind: &str, secret: bool) {
    item.parameters.push(Parameter { name: format!("{path}{suffix}"), value: value.into(), kind: kind.into(), secret });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn quoted_export() { assert_eq!(words("option ssid 'a'\\''b c'").unwrap(), ["option", "ssid", "a'b c"]); }
    #[test] fn rejects_invalid_policy() { assert!(parse("config provisioning 'global'\n option mode 'invalid'").is_err()); }
    #[test] fn revision_excludes_acs_credentials() { assert_eq!(parse("config acs 'acs'\n option password 'one'").unwrap().revision, parse("config acs 'acs'\n option password 'two'").unwrap().revision); }
    #[test] fn hwtc_is_the_only_supported_vendor_adapter_name() {
        let config = |adapter: &str| format!("config device 'child'\n option optical_sn 'HWTC3C6DE5B3'\n option cwmp_id 'child-id'\n option adapter '{adapter}'");
        assert_eq!(parse_expanded(&config("hwtc")).is_ok(), cfg!(feature="device-hwtc"));
        assert!(parse_expanded(&config("k155")).is_err());
    }
}

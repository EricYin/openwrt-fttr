//! Optional fttrd control-plane service. No ONU/device model dependencies:
//! topology comes from sysfs, policy remains owned by fw4, admission by the driver.
use crate::model::{NpuStatus, PpeFlowEntry};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const MASK: &str = "/sys/module/h3c_fmcs/parameters/bridge_offload_mask";
const NET: &str = "/sys/class/net";
const POLL: Duration = Duration::from_secs(2);

/// The daemon owns this guard; stopping and publishing a mask share one lock.
/// A poll suspended in a command cannot re-arm admission after stop().
pub(crate) struct Service(Arc<Mutex<Admission>>);

impl Service {
    pub(crate) fn start() -> Self {
        let admission = Arc::new(Mutex::new(Admission {
            path: PathBuf::from(MASK),
            stopped: false,
        }));
        let service = Self(admission.clone());
        // Revoke stale admission before discovery, including after daemon restart.
        if let Err(error) = admission.lock().unwrap().set(0) {
            eprintln!("[-] NPU initial withdrawal failed: {error}");
        }
        if text("/tmp/sysinfo/board_name").ok().as_deref() != Some("h3c,hm2004-du") {
            return service;
        }
        crate::runtime::spawn_service("NPU admission", async move {
            // Also withdraw if the task is aborted during bounded service shutdown.
            let _guard = Service(admission.clone());
            let mut reconciler = Reconciler::default();
            let mut last_error = None;
            loop {
                tokio::select! {
                    biased;
                    _ = crate::runtime::cancelled() => break,
                    _ = tokio::time::sleep(POLL) => (),
                }
                let result = tokio::select! {
                    biased;
                    _ = crate::runtime::cancelled() => break,
                    result = reconcile(&admission, &mut reconciler) => result,
                };
                match result {
                    Ok(()) => last_error = None,
                    Err(error) => {
                        reconciler = Reconciler::default();
                        if let Err(error) = admission.lock().unwrap().set(0) {
                            eprintln!("[-] NPU withdrawal failed: {error}");
                        }
                        let message = error.to_string();
                        if last_error.as_ref() != Some(&message) {
                            eprintln!("[-] NPU admission paused: {message}");
                            last_error = Some(message);
                        }
                    }
                }
            }
        });
        service
    }

    pub(crate) fn stop(&self) {
        let mut admission = self.0.lock().unwrap();
        admission.stopped = true;
        if let Err(error) = admission.set(0) {
            eprintln!("[-] NPU shutdown withdrawal failed: {error}");
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Admission {
    path: PathBuf,
    stopped: bool,
}

impl Admission {
    fn current(&self) -> io::Result<u16> {
        match text(&self.path) {
            Ok(value) => value.parse().map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    fn set(&mut self, mask: u16) -> io::Result<()> {
        let mask = if self.stopped { 0 } else { mask };
        if self.current()? != mask {
            fs::write(&self.path, format!("{mask}\n"))?;
            eprintln!("[*] NPU bridge admission mask={mask}");
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Topology {
    bridge: String,
    mask: u16,
    // Includes ifindex, MTU, MAC and learning: even same-mask changes must drain.
    ports: Vec<(String, Vec<String>)>,
}

fn text(path: impl AsRef<Path>) -> io::Result<String> {
    fs::read_to_string(path).map(|value| value.trim().to_owned())
}

fn option<'a>(config: &'a str, key: &str) -> Option<&'a str> {
    config.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then(|| value.trim_matches('\''))
    })
}

fn topology(config: &str, firewall: &str, net: &Path, mask: &Path) -> io::Result<Topology> {
    let bridge = option(config, "fttr.offload.bridge").unwrap_or("br-lan");
    let mut topology = Topology {
        bridge: bridge.into(),
        mask: 0,
        ports: Vec::new(),
    };
    let valid_bridge = !bridge.is_empty()
        && bridge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte));
    if !valid_bridge || option(config, "fttr.offload.enabled").unwrap_or("1") != "1" {
        return Ok(topology);
    }
    // uci show uses the actual section name for both named and anonymous defaults.
    let defaults = firewall
        .lines()
        .find_map(|line| line.strip_suffix("=defaults"));
    let policy = defaults.is_some_and(|section| {
        option(firewall, &format!("{section}.flow_offloading")) == Some("1")
            && option(firewall, &format!("{section}.flow_offloading_hw")) == Some("1")
    });
    if !policy
        || !mask.exists()
        || text(net.join("fttr0/carrier")).ok().as_deref() != Some("1")
        || text(net.join(bridge).join("bridge/vlan_filtering"))
            .ok()
            .as_deref()
            != Some("0")
    {
        return Ok(topology);
    }
    for port in 1..=16 {
        let name = format!("fttr{port}");
        let path = net.join(&name);
        if text(path.join("carrier")).ok().as_deref() != Some("1")
            || text(path.join("brport/state")).ok().as_deref() != Some("3")
            || fs::read_link(path.join("master")).ok().as_deref()
                != Some(Path::new(&format!("../{bridge}")))
        {
            continue;
        }
        let identity = ["ifindex", "mtu", "address", "brport/learning"]
            .iter()
            .map(|field| text(path.join(field)))
            .collect::<io::Result<Vec<_>>>()?;
        topology.mask |= 1 << (port - 1);
        topology.ports.push((name, identity));
    }
    if topology.ports.len() < 2 {
        topology.mask = 0;
    }
    Ok(topology)
}

#[derive(Default)]
struct Reconciler {
    last: Option<Topology>,
    stable: u8,
    armed: bool,
    reload_requested: bool,
}

impl Reconciler {
    /// Two unchanged observations after withdrawal, also on kernel invalidation.
    fn observe(&mut self, topology: Topology, current: u16) -> bool {
        if self.last.as_ref() != Some(&topology) || (self.armed && current == 0) {
            self.last = Some(topology);
            self.stable = 0;
            self.armed = false;
            self.reload_requested = false;
            return false;
        }
        self.stable = self.stable.saturating_add(1);
        self.last.as_ref().unwrap().mask != 0 && self.stable >= 2
    }
}

fn text_offload_flag(table: &str) -> bool {
    let lines: Vec<_> = table
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines.first() == Some(&"table bridge fw4 {")
        && lines.get(1) == Some(&"flowtable fb {")
        && lines
            .iter()
            .any(|line| line.trim_end_matches(';') == "flags offload")
}

fn hooks_ready(table: &serde_json::Value, ports: &[(String, Vec<String>)], text: &str) -> bool {
    table["nftables"].as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            let flow = &entry["flowtable"];
            flow["family"] == "bridge"
                && flow["table"] == "fw4"
                && flow["name"] == "fb"
                && flow["hook"] == "ingress"
                // nft 1.1.7 omits flowtable flags from JSON listings. Only use
                // the named flowtable's text listing when that key is absent.
                && match flow.get("flags") {
                    Some(flags) => flags.as_array().is_some_and(|flags| flags.iter().any(|flag| flag == "offload")),
                    None => text_offload_flag(text),
                }
                && flow["dev"].as_array().is_some_and(|devices| {
                    ports.iter().all(|(port, _)| {
                        devices
                            .iter()
                            .any(|device| device.as_str() == Some(port.as_str()))
                    })
                })
        })
    })
}

/// Native commands only at OpenWrt policy boundaries; no shell or per-port processes.
/// Killing a dropped child bounds shutdown even during a stuck firewall reload.
async fn command(program: &str, args: &[&str]) -> io::Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("{program} timed out")))??;
    if !output.status.success() {
        return Err(io::Error::other(format!("{program}: {}", output.status)));
    }
    String::from_utf8(output.stdout).map_err(io::Error::other)
}

async fn reconcile(admission: &Mutex<Admission>, state: &mut Reconciler) -> io::Result<()> {
    let config = command("uci", &["-q", "show", "fttr"]).await?;
    let firewall = command("uci", &["-q", "show", "firewall"]).await?;
    let topology = topology(&config, &firewall, Path::new(NET), Path::new(MASK))?;
    let current = admission.lock().unwrap().current()?;
    if !state.observe(topology, current) {
        return admission.lock().unwrap().set(0);
    }
    let topology = state.last.as_ref().unwrap();
    let table = command("nft", &["-j", "list", "flowtable", "bridge", "fw4", "fb"]).await;
    let text = command("nft", &["list", "flowtable", "bridge", "fw4", "fb"])
        .await
        .unwrap_or_default();
    let ready = table
        .ok()
        .and_then(|table| serde_json::from_str(&table).ok())
        .is_some_and(|table| hooks_ready(&table, &topology.ports, &text));
    if !ready {
        admission.lock().unwrap().set(0)?;
        state.armed = false;
        if !state.reload_requested {
            state.reload_requested = true;
            eprintln!("[*] NPU refreshing fw4 hooks for live FTTR ports");
            // Keep the one-reload-per-topology budget even when reload fails.
            if let Err(error) = command("/etc/init.d/firewall", &["reload"]).await {
                eprintln!("[-] NPU fw4 refresh failed: {error}");
            }
        }
        return Ok(());
    }
    state.reload_requested = false;
    admission.lock().unwrap().set(topology.mask)?;
    state.armed = true;
    Ok(())
}

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

pub fn status() -> NpuStatus {
    let mut status = status_from(
        Path::new("/etc/config/fttr"),
        Path::new("/sys/kernel/debug/ppe"),
        Path::new("/sys/module/h3c_fmcs/parameters"),
    );
    // Inspect the real named flowtable; admission is not proof that hooks still exist.
    let hooks = (|| -> io::Result<bool> {
        let json = nft_listing(&["-j", "list", "flowtable", "bridge", "fw4", "fb"])?;
        let table = serde_json::from_str(&json).map_err(io::Error::other)?;
        let text = nft_listing(&["list", "flowtable", "bridge", "fw4", "fb"])?;
        if !status.mask_valid {
            return Err(io::Error::other("admitted port mask unavailable"));
        }
        let ports = status
            .active_ports
            .iter()
            .map(|p| (p.clone(), Vec::new()))
            .collect::<Vec<_>>();
        Ok(hooks_ready(&table, &ports, &text))
    })();
    match hooks {
        Ok(ready) => {
            status.hooks_valid = true;
            status.fw4_hooks_ready = ready;
        }
        Err(error) => status.errors.push(format!("fw4 hooks: {error}")),
    }
    status
}

/// Bound synchronous status collection as well as the async reconciler. Drain
/// output while the command runs so even an oversized listing cannot deadlock.
fn nft_listing(args: &[&str]) -> io::Result<String> {
    use std::{
        io::Read,
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::{Command, Stdio},
        time::Instant,
    };
    let (mut reader, writer) = UnixStream::pair()?;
    reader.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut child = Command::new("nft")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let result = (|| {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let mut eof = false;
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "nft listing timed out",
                ));
            }
            if !eof {
                match reader.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        bytes.extend_from_slice(&buffer[..n]);
                        if bytes.len() > 65536 {
                            return Err(io::Error::other("nft listing exceeds output limit"));
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                        ) =>
                    {
                        ()
                    }
                    Err(e) => return Err(e),
                }
            }
            if let Some(exit) = child.try_wait()? {
                if !exit.success() {
                    return Err(io::Error::other("nft flowtable listing failed"));
                }
                if eof {
                    return String::from_utf8(bytes).map_err(io::Error::other);
                }
            } else if eof {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    child.wait()?;
    result
}

fn parse_uci_option(config: &str, section: &str, option_name: &str) -> Option<String> {
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
                // Match the same named section used by the reconciler, not any
                // unrelated section that happens to have type "offload".
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

    fn pair(identity: &str) -> Topology {
        Topology {
            bridge: "br-lan".into(),
            mask: 3,
            ports: vec![
                ("fttr1".into(), vec![identity.into()]),
                ("fttr2".into(), vec![]),
            ],
        }
    }

    #[test]
    fn invalidation_and_same_mask_identity_changes_require_settling() {
        let mut state = Reconciler::default();
        assert!(!state.observe(pair("old"), 0));
        assert!(!state.observe(pair("old"), 0));
        assert!(state.observe(pair("old"), 0));
        state.armed = true;
        assert!(!state.observe(pair("old"), 0));
        assert!(!state.observe(pair("old"), 0));
        assert!(state.observe(pair("old"), 0));
        assert!(!state.observe(pair("new"), 3));
        assert!(!state.observe(pair("new"), 0));
        assert!(state.observe(pair("new"), 0));
        let mut disabled = pair("new");
        disabled.mask = 0;
        assert!(!state.observe(disabled, 3));
    }

    #[test]
    fn hook_validation_requires_exact_port_names_and_hardware_flag() {
        let mut table = serde_json::json!({"nftables": [{"flowtable": {
            "family": "bridge", "table": "fw4", "name": "fb", "hook": "ingress",
            "flags": ["offload"], "dev": ["fttr10", "fttr2"]
        }}]});
        assert!(!hooks_ready(&table, &pair("").ports, ""));
        table["nftables"][0]["flowtable"]["dev"][0] = "fttr1".into();
        assert!(hooks_ready(&table, &pair("").ports, ""));
        table["nftables"][0]["flowtable"]["hook"] = "egress".into();
        assert!(!hooks_ready(&table, &pair("").ports, ""));
        table["nftables"][0]["flowtable"]["hook"] = "ingress".into();
        table["nftables"][0]["flowtable"]["flags"] = serde_json::json!([]);
        let text = "table bridge fw4 {\n flowtable fb {\n flags offload\n counter\n }\n}";
        assert!(!hooks_ready(&table, &pair("").ports, text));
        table["nftables"][0]["flowtable"]
            .as_object_mut()
            .unwrap()
            .remove("flags");
        assert!(hooks_ready(&table, &pair("").ports, text));
        assert!(!hooks_ready(&table, &pair("").ports, ""));
        assert!(!hooks_ready(
            &table,
            &pair("").ports,
            &text.replace("fw4", "other")
        ));
        assert!(!hooks_ready(
            &table,
            &pair("").ports,
            &text.replace("flags offload", "counter")
        ));
    }

    #[test]
    fn stopped_service_cannot_be_rearmed_by_an_inflight_poll() {
        let path = std::env::temp_dir().join(format!("fttr-npu-{}", std::process::id()));
        fs::write(&path, "3\n").unwrap();
        let service = Service(Arc::new(Mutex::new(Admission {
            path: path.clone(),
            stopped: false,
        })));
        service.stop();
        service.0.lock().unwrap().set(3).unwrap();
        assert_eq!(text(&path).unwrap(), "0");
        drop(service);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn discovery_respects_policy_topology_and_defaults_without_persisted_onu_ids() {
        let root = std::env::temp_dir().join(format!("fttr-npu-topology-{}", std::process::id()));
        let mask = root.join("mask");
        for (name, value) in [
            ("fttr0/carrier", "1"),
            ("br-lan/bridge/vlan_filtering", "0"),
            ("fttr1/carrier", "1"),
            ("fttr2/carrier", "1"),
            ("fttr1/brport/state", "3"),
            ("fttr2/brport/state", "3"),
            ("fttr1/brport/learning", "1"),
            ("fttr2/brport/learning", "1"),
            ("fttr1/ifindex", "11"),
            ("fttr2/ifindex", "12"),
            ("fttr1/mtu", "1500"),
            ("fttr2/mtu", "1500"),
            ("fttr1/address", "00:00:00:00:00:01"),
            ("fttr2/address", "00:00:00:00:00:02"),
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }
        for port in ["fttr1", "fttr2"] {
            std::os::unix::fs::symlink("../br-lan", root.join(port).join("master")).unwrap();
        }
        fs::write(&mask, "0").unwrap();
        let firewall = "firewall.@defaults[0]=defaults\nfirewall.@defaults[0].flow_offloading='1'\nfirewall.@defaults[0].flow_offloading_hw='1'\n";
        let discover =
            |config: &str, policy: &str| topology(config, policy, &root, &mask).unwrap().mask;
        assert_eq!(discover("", firewall), 3); // Existing configs without offload section.
        assert_eq!(discover("fttr.offload.enabled='0'", firewall), 0);
        assert_eq!(discover("fttr.offload.bridge='../br-lan'", firewall), 0);
        assert_eq!(
            discover(
                "",
                "firewall.main=defaults\nfirewall.main.flow_offloading='1'"
            ),
            0
        );
        assert_eq!(
            discover(
                "",
                &firewall.replace("flow_offloading_hw='1'", "flow_offloading_hw='0'")
            ),
            0
        );
        fs::write(root.join("br-lan/bridge/vlan_filtering"), "1").unwrap();
        assert_eq!(discover("", firewall), 0);
        fs::write(root.join("br-lan/bridge/vlan_filtering"), "0").unwrap();
        fs::write(root.join("fttr2/brport/state"), "2").unwrap();
        assert_eq!(discover("", firewall), 0);
        fs::write(root.join("fttr2/brport/state"), "3").unwrap();
        fs::write(root.join("fttr0/carrier"), "0").unwrap();
        assert_eq!(discover("", firewall), 0);
        fs::remove_dir_all(root).unwrap();
    }

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
        assert!(!s.fw4_hooks_ready); // A nonzero mask is not evidence of nft hooks.

        fs::write(
            ppe_dir.join("bind"),
            "00001 UNB IPv4 5T packets=99 bytes=99\nmalformed\n",
        )
        .unwrap();
        let corrupt = status_from(&cfg_path, &ppe_dir, &mod_dir);
        assert!(!corrupt.flows_valid && !corrupt.counters_valid);
        assert!(corrupt.bound_flows.is_empty());

        // Also test with raw UCI file syntax and disabled offload
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

use super::command;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

pub const MASK: &str = "/sys/module/h3c_fmcs/parameters/bridge_offload_mask";
pub const NET: &str = "/sys/class/net";
pub const POLL_FAST: Duration = Duration::from_secs(2);
pub const POLL_STEADY: Duration = Duration::from_secs(10);
const FULL_CHECK: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub bridge: String,
    pub mask: u16,
    // Includes ifindex, MTU, MAC and learning: even same-mask changes must drain.
    pub ports: Vec<(String, Vec<String>)>,
}

pub(crate) fn text(path: impl AsRef<Path>) -> io::Result<String> {
    fs::read_to_string(path).map(|value| value.trim().to_owned())
}

pub(crate) fn option<'a>(config: &'a str, key: &str) -> Option<&'a str> {
    config.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then(|| value.trim_matches('\''))
    })
}

pub fn topology(config: &str, firewall: &str, net: &Path, mask: &Path) -> io::Result<Topology> {
    let bridge = option(config, "fttr.offload.bridge").unwrap_or("br-lan");
    let mut topology = Topology {
        bridge: bridge.into(),
        mask: 0,
        ports: Vec::new(),
    };
    let valid_bridge = !bridge.is_empty()
        && bridge.len() <= 15
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
pub struct Reconciler {
    pub last: Option<Topology>,
    pub stable: u8,
    pub armed: bool,
    pub reload_requested: bool,
    cache: Option<PolicyCache>,
}

struct PolicyCache {
    config: String,
    firewall: String,
    files: Vec<Option<Vec<u8>>>,
    verified: Instant,
}

// Include uncommitted UCI deltas: `uci show` observes these as well as /etc/config.
fn policy_files(config: &Path, delta: &Path) -> io::Result<Vec<Option<Vec<u8>>>> {
    [config, delta]
        .into_iter()
        .flat_map(|dir| ["fttr", "firewall", "network"].map(|name| dir.join(name)))
        .map(|path| match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        })
        .collect()
}

impl Reconciler {
    /// Two unchanged observations after withdrawal, also on kernel invalidation.
    pub fn observe(&mut self, topology: Topology, current: u16) -> bool {
        if self.last.as_ref() != Some(&topology) || (self.armed && current != topology.mask) {
            self.last = Some(topology);
            self.stable = 0;
            self.armed = false;
            self.reload_requested = false;
            return false;
        }
        self.stable = self.stable.saturating_add(1);
        self.last.as_ref().unwrap().mask != 0 && self.stable >= 2
    }

    fn can_skip(
        &self,
        topology: &Topology,
        current: u16,
        files: &[Option<Vec<u8>>],
        now: Instant,
    ) -> bool {
        self.armed
            && current != 0
            && current == topology.mask
            && self.last.as_ref() == Some(topology)
            && self.cache.as_ref().is_some_and(|cache| {
                cache.files == files && now.duration_since(cache.verified) < FULL_CHECK
            })
    }
}

pub fn text_offload_flag(table: &str) -> bool {
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

pub fn hooks_ready(table: &serde_json::Value, ports: &[(String, Vec<String>)], text: &str) -> bool {
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
                    Some(flags) => flags
                        .as_array()
                        .is_some_and(|flags| flags.iter().any(|flag| flag == "offload")),
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

pub struct Admission {
    pub path: PathBuf,
    pub stopped: bool,
}

impl Admission {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            stopped: false,
        }
    }

    pub fn current(&self) -> io::Result<u16> {
        match text(&self.path) {
            Ok(value) => value.parse().map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    pub fn set(&mut self, mask: u16) -> io::Result<()> {
        let mask = if self.stopped { 0 } else { mask };
        if self.current()? != mask {
            fs::write(&self.path, format!("{mask}\n"))?;
            eprintln!("[*] NPU bridge admission mask={mask}");
        }
        Ok(())
    }
}

/// Reconcile NPU admission state.
/// Employs a Fast-Path check against sysfs to avoid expensive `uci` and `nft` forks
/// when the physical topology and admission state remain in steady equilibrium.
pub async fn reconcile(
    admission: &Mutex<Admission>,
    state: &mut Reconciler,
) -> io::Result<Duration> {
    let net = Path::new(NET);
    let mask_path = Path::new(MASK);
    let current_mask = admission.lock().unwrap().current()?;
    let files = policy_files(Path::new("/etc/config"), Path::new("/tmp/.uci"))?;

    // Fast-path: When already armed and the driver hasn't revoked the mask,
    // verify the physical link status before spawning any external processes.
    if let Some(cache) = &state.cache {
        // Reuse the complete topology predicate, including new ports, master,
        // forwarding state and identity. Missing sysfs data is never healthy.
        let live = topology(&cache.config, &cache.firewall, net, mask_path)?;
        if state.can_skip(&live, current_mask, &files, Instant::now()) {
            return Ok(POLL_STEADY);
        }
        if state.armed
            && (state.last.as_ref() != Some(&live)
                || current_mask != live.mask
                || cache.files != files)
        {
            // Withdraw before waiting on UCI/nft when a known prerequisite changed.
            admission.lock().unwrap().set(0)?;
        }
    }

    // Slow-path: Inspect configuration and topology via native UCI
    let (config, firewall) = {
        let mut uci = openwrt_uci::Uci::new().map_err(|e| io::Error::other(e.to_string()))?;
        let bridge = match uci.get("fttr", "offload", "bridge") {
            Ok(Some(v)) => v,
            Ok(None) => "br-lan".to_string(),
            Err(e) => {
                return Err(io::Error::other(format!(
                    "fttr.offload.bridge read error: {e}"
                )))
            }
        };
        let offload_enabled = match uci.get("fttr", "offload", "enabled") {
            Ok(Some(v)) => v,
            Ok(None) => "1".to_string(),
            Err(e) => {
                return Err(io::Error::other(format!(
                    "fttr.offload.enabled read error: {e}"
                )))
            }
        };
        let flow_offloading = match uci.get("firewall", "@defaults[0]", "flow_offloading") {
            Ok(opt) => opt,
            Err(e) => {
                return Err(io::Error::other(format!(
                    "firewall.flow_offloading read error: {e}"
                )))
            }
        };
        let flow_offloading_hw = match uci.get("firewall", "@defaults[0]", "flow_offloading_hw") {
            Ok(opt) => opt,
            Err(e) => {
                return Err(io::Error::other(format!(
                    "firewall.flow_offloading_hw read error: {e}"
                )))
            }
        };
        let hw_offload =
            flow_offloading.as_deref() == Some("1") && flow_offloading_hw.as_deref() == Some("1");

        let config =
            format!("fttr.offload.bridge={bridge}\nfttr.offload.enabled={offload_enabled}\n");
        let firewall = if hw_offload {
            "firewall.@defaults[0]=defaults\nfirewall.@defaults[0].flow_offloading='1'\nfirewall.@defaults[0].flow_offloading_hw='1'\n".to_string()
        } else {
            "firewall.@defaults[0]=defaults\n".to_string()
        };
        (config, firewall)
    };
    let topology = topology(&config, &firewall, net, mask_path)?;
    // Timestamp is refreshed only after hooks pass, never by a cheap probe.
    let verified = state
        .cache
        .as_ref()
        .map(|cache| cache.verified)
        .unwrap_or_else(Instant::now);
    state.cache = Some(PolicyCache {
        config,
        firewall,
        files,
        verified,
    });
    let current = admission.lock().unwrap().current()?;

    if !state.observe(topology, current) {
        admission.lock().unwrap().set(0)?;
        return Ok(POLL_FAST);
    }

    let topology = state.last.as_ref().unwrap();

    // Query flowtable hooks with lazy text fallback (avoid duplicate nft listing)
    let json_table = command::run_async(
        "nft",
        &["-j", "list", "flowtable", "bridge", "fw4", "fb"],
        Duration::from_secs(5),
    )
    .await;

    let ready = match json_table {
        Ok(ref json_str) => match serde_json::from_str::<serde_json::Value>(json_str) {
            Ok(ref table) => {
                let has_flags = table["nftables"].as_array().is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry["flowtable"].get("flags").is_some())
                });
                let text_listing = if has_flags {
                    String::new()
                } else {
                    command::run_async(
                        "nft",
                        &["list", "flowtable", "bridge", "fw4", "fb"],
                        Duration::from_secs(5),
                    )
                    .await
                    .unwrap_or_default()
                };
                hooks_ready(table, &topology.ports, &text_listing)
            }
            Err(_) => false,
        },
        Err(_) => false,
    };

    if !ready {
        admission.lock().unwrap().set(0)?;
        state.armed = false;
        if !state.reload_requested {
            state.reload_requested = true;
            eprintln!("[*] NPU refreshing fw4 hooks for live FTTR ports");
            if let Err(error) =
                command::run_async("/etc/init.d/firewall", &["reload"], Duration::from_secs(5))
                    .await
            {
                eprintln!("[-] NPU fw4 refresh failed: {error}");
            }
        }
        return Ok(POLL_FAST);
    }

    state.reload_requested = false;
    // Commands can suspend for seconds. Do not publish a snapshot invalidated
    // during hook inspection (including a kernel withdrawal with the same mask).
    let cache = state.cache.as_ref().unwrap();
    if policy_files(Path::new("/etc/config"), Path::new("/tmp/.uci"))? != cache.files
        || self::topology(&cache.config, &cache.firewall, net, mask_path)? != *topology
        || admission.lock().unwrap().current()? != current
    {
        admission.lock().unwrap().set(0)?;
        state.last = None;
        state.armed = false;
        state.stable = 0;
        return Ok(POLL_FAST);
    }
    admission.lock().unwrap().set(topology.mask)?;
    state.armed = true;
    state.cache.as_mut().unwrap().verified = Instant::now();
    Ok(POLL_STEADY)
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
        state.armed = true;
        state.last = Some(pair("old"));
        assert!(!state.observe(pair("old"), 7));
    }

    #[test]
    fn steady_cache_requires_full_topology_policy_mask_and_periodic_hook_checks() {
        let now = Instant::now();
        let files = vec![Some(b"enabled".to_vec())];
        let state = Reconciler {
            last: Some(pair("old")),
            armed: true,
            cache: Some(PolicyCache {
                config: String::new(),
                firewall: String::new(),
                files: files.clone(),
                verified: now,
            }),
            ..Default::default()
        };
        assert!(state.can_skip(&pair("old"), 3, &files, now));
        for mask in [0, 1, 7] {
            assert!(!state.can_skip(&pair("old"), mask, &files, now));
        }
        assert!(!state.can_skip(&pair("new"), 3, &files, now));
        let mut added = pair("old");
        added.mask = 7;
        added.ports.push(("fttr3".into(), vec![]));
        assert!(!state.can_skip(&added, 3, &files, now));
        assert!(!state.can_skip(&pair("old"), 3, &[None], now));
        assert!(!state.can_skip(&pair("old"), 3, &files, now + FULL_CHECK));
    }

    #[test]
    fn policy_watch_includes_uncommitted_changes_and_removal() {
        let root = std::env::temp_dir().join(format!("fttr-npu-policy-{}", std::process::id()));
        let config = root.join("config");
        let delta = root.join("delta");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&delta).unwrap();
        let initial = policy_files(&config, &delta).unwrap();
        fs::write(config.join("fttr"), "enabled").unwrap();
        let committed = policy_files(&config, &delta).unwrap();
        assert_ne!(initial, committed);
        fs::write(delta.join("firewall"), "disabled").unwrap();
        assert_ne!(committed, policy_files(&config, &delta).unwrap());
        fs::remove_file(delta.join("firewall")).unwrap();
        assert_eq!(committed, policy_files(&config, &delta).unwrap());
        fs::remove_dir_all(root).unwrap();
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
            #[cfg(unix)]
            std::os::unix::fs::symlink("../br-lan", root.join(port).join("master")).unwrap();
            #[cfg(not(unix))]
            let _ = fs::write(root.join(port).join("master"), "../br-lan");
        }
        fs::write(&mask, "0").unwrap();
        let firewall = "firewall.@defaults[0]=defaults\nfirewall.@defaults[0].flow_offloading='1'\nfirewall.@defaults[0].flow_offloading_hw='1'\n";
        let discover =
            |config: &str, policy: &str| topology(config, policy, &root, &mask).unwrap().mask;
        assert_eq!(discover("", firewall), 3);
        let before = topology("", firewall, &root, &mask).unwrap();
        fs::write(root.join("fttr1/mtu"), "1400").unwrap();
        let after = topology("", firewall, &root, &mask).unwrap();
        assert_eq!(before.mask, after.mask);
        assert_ne!(before, after);
        fs::write(root.join("fttr1/mtu"), "1500").unwrap();
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
        fs::remove_file(root.join("br-lan/bridge/vlan_filtering")).unwrap();
        assert_eq!(discover("", firewall), 0);
        fs::write(root.join("br-lan/bridge/vlan_filtering"), "0").unwrap();
        fs::write(root.join("fttr0/carrier"), "0").unwrap();
        assert_eq!(discover("", firewall), 0);
        let _ = fs::remove_dir_all(root);
    }
}

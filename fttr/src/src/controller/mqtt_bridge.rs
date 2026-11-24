//! Independent, opt-in (default-off) bridge-mode MQTT redirect coordinator.
//!
//! Never starts or alters the embedded MQTT broker, ACS, OMCI, or NAT-mode
//! devices whose addresses come from the master gateway's LAN DHCP pool.
#[cfg(feature = "mqtt")]
mod conntrack;
#[cfg(feature = "mqtt")]
mod nft;

#[cfg(feature = "mqtt")]
mod active {
    use super::super::mqtt_bridge_evidence::{
        attach_onu_capture_filter, decode_frame, decode_mqtt_syn, MqttSyn, Origin, ServerOrigins,
        Transactions,
    };
    use super::{conntrack, nft};
    use crate::platform::openwrt::packet::PacketSocket;
    use crate::provisioning::model::Device;
    use fttr_api::{DeviceMqttBridgeStatus, MqttBridgeRule, MqttBridgeStatus};
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::Ipv4Addr;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    };
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const IDENTITIES_PATH: &str = "/var/lib/fttr/mqtt-identities.json";
    const SUBDEV_DIR: &str = "/tmp/fttr_subdev";
    const SYN_FRESHNESS: Duration = Duration::from_secs(120);
    const TRIAL_DURATION: Duration = Duration::from_secs(15);
    const TRIAL_COOLDOWN: Duration = Duration::from_secs(60);

    #[derive(Clone)]
    struct LanContext {
        origin: Origin,
        lan_ip: Option<Ipv4Addr>,
        lan_mask: u8,
        upstream_gateway: Option<Ipv4Addr>,
        upstream_dhcp_server: Option<Ipv4Addr>,
        bridge_mac: Option<[u8; 6]>,
        local_lan_dhcp: bool,
        local_leases_by_mac: BTreeMap<[u8; 6], Ipv4Addr>,
        local_lease_ips: BTreeSet<Ipv4Addr>,
        reason: Option<String>,
    }

    impl Default for LanContext {
        fn default() -> Self {
            Self {
                origin: Origin::Unknown,
                lan_ip: None,
                lan_mask: 24,
                upstream_gateway: None,
                upstream_dhcp_server: None,
                bridge_mac: None,
                local_lan_dhcp: false,
                local_leases_by_mac: BTreeMap::new(),
                local_lease_ips: BTreeSet::new(),
                reason: Some("LAN state not yet inspected".into()),
            }
        }
    }

    #[derive(Clone, Copy)]
    struct SynObservation {
        source: Ipv4Addr,
        destination: Ipv4Addr,
        observed_at: Instant,
        epoch_secs: u64,
    }

    #[derive(Clone, Copy)]
    struct TrialState {
        mac: [u8; 6],
        started_at: Instant,
        failed_until: Option<Instant>,
    }

    struct CaptureControl {
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
        desired_interfaces: BTreeMap<u8, String>,
    }

    struct Coordinator {
        enabled: bool,
        seeded_identities: bool,
        broker_ip: Option<Ipv4Addr>,
        lan: LanContext,
        transactions: Transactions,
        leases: BTreeMap<[u8; 6], super::super::mqtt_bridge_evidence::Lease>,
        syn_attempts: BTreeMap<(String, [u8; 6]), SynObservation>,
        trials: BTreeMap<String, TrialState>,
        applied_rules: Vec<nft::RedirectRule>,
        tracked_matchers: BTreeMap<(Ipv4Addr, Ipv4Addr), conntrack::FlowMatcher>,
        cleanup_error: Option<String>,
        rule_error: Option<String>,
        global: MqttBridgeStatus,
        devices: BTreeMap<String, DeviceMqttBridgeStatus>,
        capture: Option<CaptureControl>,
    }

    impl Default for Coordinator {
        fn default() -> Self {
            Self {
                enabled: false,
                seeded_identities: false,
                broker_ip: None,
                lan: LanContext::default(),
                transactions: Transactions::default(),
                leases: BTreeMap::new(),
                syn_attempts: BTreeMap::new(),
                trials: BTreeMap::new(),
                applied_rules: Vec::new(),
                tracked_matchers: BTreeMap::new(),
                cleanup_error: None,
                rule_error: None,
                global: MqttBridgeStatus {
                    enabled: false,
                    state: "disabled".into(),
                    lan_origin: "unknown".into(),
                    ..Default::default()
                },
                devices: BTreeMap::new(),
                capture: None,
            }
        }
    }

    static COORDINATOR: OnceLock<Mutex<Coordinator>> = OnceLock::new();

    fn coordinator() -> &'static Mutex<Coordinator> {
        COORDINATOR.get_or_init(|| Mutex::new(Coordinator::default()))
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn origin_str(origin: Origin) -> &'static str {
        match origin {
            Origin::Local => "local",
            Origin::Upstream => "upstream",
            Origin::Unknown => "unknown",
        }
    }

    fn parse_mac_hex12(hex: &str) -> Option<[u8; 6]> {
        let norm = crate::mqtt_identity::normalize_target(hex)?;
        let mut out = [0u8; 6];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&norm[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(out)
    }

    fn seed_identities_once(c: &mut Coordinator) {
        if c.seeded_identities {
            return;
        }
        c.seeded_identities = true;
        if let Ok(bytes) = std::fs::read(IDENTITIES_PATH) {
            if let Ok(map) = serde_json::from_slice::<BTreeMap<String, String>>(&bytes) {
                if let Ok(mut reg) = crate::mqtt_identity::shared().lock() {
                    for (sn, mac) in map {
                        reg.seed_verified_report(&sn, &mac);
                    }
                }
            }
        }
        #[cfg(feature = "device-hl202")]
        if let Ok(entries) = std::fs::read_dir(SUBDEV_DIR) {
            let mut count = 0usize;
            for entry in entries.flatten() {
                if count >= 64 {
                    break;
                }
                count += 1;
                let dir_name = entry.file_name();
                let Some(topic_mac) = dir_name.to_str() else {
                    continue;
                };
                let bin_path = entry.path().join("telemetry.bin");
                let Ok(meta) = std::fs::metadata(&bin_path) else {
                    continue;
                };
                if meta.len() > 64 * 1024 {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&bin_path) else {
                    continue;
                };
                if let Ok(report) = crate::devices::hl202::telemetry::parse(&bytes) {
                    if let Some((serial, mac)) =
                        crate::devices::hl202::telemetry::identity_claim(&report, topic_mac)
                    {
                        if let Ok(mut reg) = crate::mqtt_identity::shared().lock() {
                            reg.seed_verified_report(&serial, &mac);
                        }
                    }
                }
            }
        }
    }

    fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, mask: u8) -> bool {
        if mask == 0 {
            return true;
        }
        if mask > 32 {
            return false;
        }
        let bits = if mask == 32 {
            u32::MAX
        } else {
            u32::MAX << (32 - mask)
        };
        (u32::from(a) & bits) == (u32::from(b) & bits)
    }

    fn read_local_lan_dhcp_enabled() -> Result<bool, String> {
        let mut uci = openwrt_uci::Uci::new().map_err(|e| format!("open UCI: {e}"))?;
        let ignore = uci
            .get("dhcp", "lan", "ignore")
            .map_err(|e| e.to_string())?;
        let dhcpv4 = uci
            .get("dhcp", "lan", "dhcpv4")
            .map_err(|e| e.to_string())?;
        Ok(ignore.as_deref() != Some("1") && dhcpv4.as_deref() != Some("disabled"))
    }

    fn read_local_leases(
        lan_ip: Option<Ipv4Addr>,
        lan_mask: u8,
        local_lan_dhcp: bool,
    ) -> (BTreeMap<[u8; 6], Ipv4Addr>, BTreeSet<Ipv4Addr>) {
        let mut by_mac = BTreeMap::new();
        let mut ips = BTreeSet::new();
        if !local_lan_dhcp {
            return (by_mac, ips);
        }
        let Some(lan_ip) = lan_ip else {
            return (by_mac, ips);
        };
        let Ok(meta) = std::fs::metadata("/tmp/dhcp.leases") else {
            return (by_mac, ips);
        };
        if meta.len() > 1024 * 1024 {
            return (by_mac, ips);
        }
        let Ok(text) = std::fs::read_to_string("/tmp/dhcp.leases") else {
            return (by_mac, ips);
        };
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (_expiry, Some(mac_str), Some(ip_str)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Some(mac), Ok(ip)) = (parse_mac_hex12(mac_str), ip_str.parse::<Ipv4Addr>()) else {
                continue;
            };
            if same_subnet(ip, lan_ip, lan_mask) {
                by_mac.insert(mac, ip);
                ips.insert(ip);
            }
        }
        (by_mac, ips)
    }

    fn inspect_lan_context() -> LanContext {
        let mut ctx = LanContext::default();
        let status = match openwrt_ubus::Ubus::connect(None)
            .map_err(|e| e.to_string())
            .and_then(|mut bus| crate::platform::openwrt::NetifdClient::new(&mut bus).status("lan"))
        {
            Ok(s) => s,
            Err(error) => {
                ctx.reason = Some(format!("LAN status unavailable: {error}"));
                return ctx;
            }
        };
        if !status.up {
            ctx.reason = Some("LAN interface is down".into());
            return ctx;
        }
        let Some(addr_entry) = status.ipv4_address.first() else {
            ctx.reason = Some("LAN interface has no IPv4 address".into());
            return ctx;
        };
        let Ok(lan_ip) = addr_entry.address.parse::<Ipv4Addr>() else {
            ctx.reason = Some("LAN IPv4 address is invalid".into());
            return ctx;
        };
        ctx.lan_ip = Some(lan_ip);
        ctx.lan_mask = addr_entry.mask.min(32);
        ctx.upstream_gateway = status
            .route
            .iter()
            .find(|r| r.target == "0.0.0.0" && r.mask == 0)
            .and_then(|r| r.nexthop.parse::<Ipv4Addr>().ok());
        ctx.upstream_dhcp_server = status
            .data
            .dhcpserver
            .as_deref()
            .and_then(|s| s.parse::<Ipv4Addr>().ok());
        let bridge_name = if !status.l3_device.is_empty() {
            status.l3_device.as_str()
        } else if !status.device.is_empty() {
            status.device.as_str()
        } else {
            "br-lan"
        };
        if bridge_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            if let Ok(mac_str) =
                std::fs::read_to_string(format!("/sys/class/net/{bridge_name}/address"))
            {
                ctx.bridge_mac = parse_mac_hex12(mac_str.trim());
            }
        }
        ctx.local_lan_dhcp = read_local_lan_dhcp_enabled().unwrap_or(false);
        (ctx.local_leases_by_mac, ctx.local_lease_ips) =
            read_local_leases(ctx.lan_ip, ctx.lan_mask, ctx.local_lan_dhcp);

        ctx.origin = if status.proto == "dhcp"
            && !ctx.local_lan_dhcp
            && ctx.upstream_dhcp_server.is_some_and(|srv| srv != lan_ip)
        {
            ctx.reason = None;
            Origin::Upstream
        } else if ctx.local_lan_dhcp {
            ctx.reason = Some("LAN runs local DHCP server (NAT/router mode)".into());
            Origin::Local
        } else {
            ctx.reason = Some("LAN address origin cannot be confirmed as upstream DHCP".into());
            Origin::Unknown
        };
        ctx
    }

    fn server_origins(ctx: &LanContext) -> ServerOrigins {
        let mut origins = ServerOrigins::new();
        if ctx.local_lan_dhcp {
            if let Some(ip) = ctx.lan_ip {
                origins.insert(ip, Origin::Local);
            }
        }
        if ctx.origin == Origin::Upstream {
            if let Some(srv) = ctx.upstream_dhcp_server {
                origins.insert(srv, Origin::Upstream);
            }
        }
        origins
    }

    fn stop_capture(c: &mut Coordinator) {
        if let Some(mut cap) = c.capture.take() {
            cap.stop.store(true, Ordering::Release);
            if let Some(handle) = cap.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn ensure_capture(c: &mut Coordinator, target_interfaces: BTreeMap<u8, String>) {
        if !c.enabled {
            stop_capture(c);
            return;
        }
        if let Some(cap) = c.capture.as_ref() {
            if cap.desired_interfaces == target_interfaces
                && cap.handle.as_ref().is_some_and(|h| !h.is_finished())
            {
                return;
            }
        }
        stop_capture(c);
        if target_interfaces.is_empty() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let interfaces = target_interfaces.clone();
        let handle = std::thread::Builder::new()
            .name("mqtt-bridge-cap".into())
            .stack_size(128 * 1024)
            .spawn(move || run_capture_loop(interfaces, stop_flag))
            .ok();
        c.capture = Some(CaptureControl {
            stop,
            handle,
            desired_interfaces: target_interfaces,
        });
    }

    fn run_capture_loop(interfaces: BTreeMap<u8, String>, stop: Arc<AtomicBool>) {
        let mut sockets: Vec<(String, PacketSocket)> = Vec::new();
        for iface in interfaces.values() {
            match PacketSocket::open_filtered(iface, false, |fd| attach_onu_capture_filter(fd)) {
                Ok(sock) => sockets.push((iface.clone(), sock)),
                Err(err) => eprintln!("[-] MQTT bridge capture unavailable on {iface}: {err}"),
            }
        }
        if sockets.is_empty() {
            return;
        }
        let mut pollfds: Vec<libc::pollfd> = sockets
            .iter()
            .map(|(_, s)| libc::pollfd {
                fd: s.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let mut buffer = [0u8; 4096];
        while !stop.load(Ordering::Acquire) {
            let ret = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, 500) };
            if ret <= 0 {
                continue;
            }
            for (idx, pfd) in pollfds.iter_mut().enumerate() {
                if pfd.revents & libc::POLLIN == 0 {
                    continue;
                }
                pfd.revents = 0;
                let (iface, sock) = &sockets[idx];
                loop {
                    match sock.receive_into(&mut buffer) {
                        Ok(Some((bytes, kind, _vlan))) => {
                            let outgoing = kind == 4;
                            let now_inst = Instant::now();
                            let epoch = now_secs();
                            if let Some(dhcp) = decode_frame(bytes) {
                                if (dhcp.request && !outgoing) || (!dhcp.request && outgoing) {
                                    let wake_needed = if let Ok(mut c) = coordinator().lock() {
                                        let origins = server_origins(&c.lan);
                                        if let Some(lease) = c.transactions.observe(
                                            dhcp.payload,
                                            dhcp.source,
                                            now_inst,
                                            &origins,
                                        ) {
                                            let changed =
                                                c.leases.get(&lease.client) != Some(&lease);
                                            c.leases.insert(lease.client, lease);
                                            changed
                                        } else {
                                            false
                                        }
                                    } else {
                                        false
                                    };
                                    if wake_needed {
                                        crate::provisioning::controller::wake();
                                    }
                                }
                            } else if !outgoing {
                                if let Some(syn) = decode_mqtt_syn(bytes) {
                                    handle_syn(iface, syn, now_inst, epoch);
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            eprintln!("[-] MQTT bridge capture read error on {iface}: {e}");
                            break;
                        }
                    }
                }
            }
        }
    }

    fn handle_syn(iface: &str, syn: MqttSyn, now_inst: Instant, epoch_secs: u64) {
        let should_wake = if let Ok(mut c) = coordinator().lock() {
            if c.syn_attempts.len() >= 128
                && !c
                    .syn_attempts
                    .contains_key(&(iface.to_owned(), syn.ethernet_source))
            {
                c.syn_attempts.retain(|_, v| {
                    now_inst.saturating_duration_since(v.observed_at) <= SYN_FRESHNESS
                });
                if c.syn_attempts.len() >= 128 {
                    return;
                }
            }
            let key = (iface.to_owned(), syn.ethernet_source);
            let is_new_or_changed = c.syn_attempts.get(&key).is_none_or(|prev| {
                prev.source != syn.source || prev.destination != syn.destination
            });
            c.syn_attempts.insert(
                key,
                SynObservation {
                    source: syn.source,
                    destination: syn.destination,
                    observed_at: now_inst,
                    epoch_secs,
                },
            );
            is_new_or_changed
        } else {
            false
        };
        if should_wake {
            crate::provisioning::controller::wake();
        }
    }

    fn cleanup_rules_and_conntrack(c: &mut Coordinator) -> Result<(), String> {
        let mut errors = Vec::new();
        if let Err(err) = nft::remove() {
            errors.push(err);
        }
        let matchers: Vec<_> = c.tracked_matchers.values().copied().collect();
        match conntrack::cleanup_redirected_flows(&matchers, c.broker_ip) {
            Ok(_) => c.tracked_matchers.clear(),
            Err(err) => errors.push(err),
        }
        match nft::tables_present() {
            Ok((false, false)) => {}
            Ok((b, i)) => errors.push(format!(
                "nft cleanup incomplete (bridge_present={b}, ip_present={i})"
            )),
            Err(err) => errors.push(err),
        }
        c.applied_rules.clear();
        if errors.is_empty() {
            c.cleanup_error = None;
            Ok(())
        } else {
            let msg = errors.join("; ");
            c.cleanup_error = Some(msg.clone());
            Err(msg)
        }
    }

    fn is_hl202_candidate(device: &Device) -> bool {
        device.omci_adapter == "hl202"
            || device.provisioning_adapter == "hl202-mqtt"
            || device.bootstrap_adapter == "hl202-mqtt-acs"
            || (device.omci_adapter == "auto" && device.optical_sn.starts_with("H3CT"))
    }

    pub(crate) fn configure(enabled: bool) {
        let mut c = coordinator().lock().unwrap();
        let changed = c.enabled != enabled;
        c.enabled = enabled;
        if !enabled {
            stop_capture(&mut c);
            c.trials.clear();
            c.rule_error = None;
            let need_cleanup =
                !c.applied_rules.is_empty() || !c.tracked_matchers.is_empty() || changed;
            if need_cleanup {
                let res = cleanup_rules_and_conntrack(&mut c);
                for dev in c.devices.values_mut() {
                    dev.rule_installed = false;
                    dev.mode = "direct".into();
                    dev.state = if res.is_err() {
                        "cleanup_failed".into()
                    } else {
                        "disabled".into()
                    };
                    dev.reason = Some(
                        res.as_ref()
                            .err()
                            .cloned()
                            .unwrap_or_else(|| "Bridge MQTT redirect is disabled".into()),
                    );
                }
            }
            c.global.enabled = false;
            c.global.active_rules = 0;
            c.global.rules.clear();
            c.global.state = if c.cleanup_error.is_some() {
                "cleanup_failed".into()
            } else {
                "disabled".into()
            };
            c.global.error = c.cleanup_error.clone();
            c.global.reason = if c.cleanup_error.is_some() {
                c.cleanup_error.clone()
            } else {
                Some("Disabled by configuration".into())
            };
        } else {
            c.cleanup_error = None;
            c.global.enabled = true;
            if c.global.state == "disabled" {
                c.global.state = "waiting".into();
                c.global.reason = Some("Waiting for evidence reconciliation".into());
            }
        }
    }

    pub(crate) fn set_broker_address(addr: Option<Ipv4Addr>) {
        let mut c = coordinator().lock().unwrap();
        c.broker_ip = addr;
        c.global.broker_listen = addr.map(|ip| format!("{ip}:1883"));
    }

    pub(crate) fn shutdown() {
        let mut c = coordinator().lock().unwrap();
        c.enabled = false;
        stop_capture(&mut c);
        let _ = cleanup_rules_and_conntrack(&mut c);
    }

    pub(crate) fn tick(
        online_onus: &BTreeMap<u16, String>,
        interfaces: &BTreeMap<u16, String>,
        configured_devices: &[Device],
    ) {
        let now_inst = Instant::now();
        let epoch = now_secs();
        let lan = inspect_lan_context();

        let mut c = coordinator().lock().unwrap();
        seed_identities_once(&mut c);
        c.lan = lan.clone();
        c.leases.retain(|_, lease| lease.expires > now_inst);
        c.syn_attempts
            .retain(|_, syn| now_inst.saturating_duration_since(syn.observed_at) <= SYN_FRESHNESS);

        if !c.enabled {
            stop_capture(&mut c);
            if !c.applied_rules.is_empty() || !c.tracked_matchers.is_empty() {
                let _ = cleanup_rules_and_conntrack(&mut c);
            } else if c.cleanup_error.is_some() {
                let _ = cleanup_rules_and_conntrack(&mut c);
            }
            c.global = MqttBridgeStatus {
                enabled: false,
                state: if c.cleanup_error.is_some() {
                    "cleanup_failed".into()
                } else {
                    "disabled".into()
                },
                lan_origin: origin_str(lan.origin).into(),
                broker_listen: c.broker_ip.map(|ip| format!("{ip}:1883")),
                upstream_gateway: lan.upstream_gateway.map(|ip| ip.to_string()),
                active_rules: 0,
                rules: Vec::new(),
                reason: Some(
                    c.cleanup_error
                        .clone()
                        .unwrap_or_else(|| "Disabled by configuration".into()),
                ),
                error: c.cleanup_error.clone(),
            };
            c.devices
                .retain(|sn, _| configured_devices.iter().any(|d| &d.optical_sn == sn));
            let global_state = c.global.state.clone();
            for d in configured_devices {
                c.devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: global_state.clone(),
                        address_origin: origin_str(lan.origin).into(),
                        mode: "direct".into(),
                        rule_installed: false,
                        telemetry_verified: false,
                        reason: Some("Bridge MQTT redirect is disabled".into()),
                        ..Default::default()
                    },
                );
            }
            return;
        }

        // Enabled path: determine online HL202 interfaces to capture on.
        let mut capture_targets = BTreeMap::new();
        for (&onu_id, sn) in online_onus {
            let configured_hl202 = configured_devices
                .iter()
                .find(|d| &d.optical_sn == sn)
                .is_some_and(is_hl202_candidate);
            if configured_hl202 || sn.starts_with("H3CT") {
                if let (Ok(id_u8), Some(iface)) = (u8::try_from(onu_id), interfaces.get(&onu_id)) {
                    capture_targets.insert(id_u8, iface.clone());
                }
            }
        }
        ensure_capture(&mut c, capture_targets);

        let mut desired_rules = Vec::new();
        let mut next_devices = BTreeMap::new();

        for d in configured_devices {
            if !is_hl202_candidate(d) {
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: "not_applicable".into(),
                        address_origin: "unknown".into(),
                        mode: "direct".into(),
                        reason: Some("Device does not use HL202 MQTT".into()),
                        ..Default::default()
                    },
                );
                continue;
            }

            let matching_onus: Vec<(u16, String)> = online_onus
                .iter()
                .filter(|(_, sn)| *sn == &d.optical_sn)
                .filter_map(|(id, _)| interfaces.get(id).map(|iface| (*id, iface.clone())))
                .collect();

            if matching_onus.len() != 1 {
                c.trials.remove(&d.optical_sn);
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: "waiting_optical".into(),
                        address_origin: "unknown".into(),
                        mode: "direct".into(),
                        reason: Some(if matching_onus.is_empty() {
                            "Waiting for ONU to reach O5".into()
                        } else {
                            "Multiple online ONUs share this serial number".into()
                        }),
                        ..Default::default()
                    },
                );
                continue;
            }
            let (_onu_id, iface) = &matching_onus[0];

            // Snapshot identity & FDB candidates from mqtt_identity.
            let (verified_mac_str, fdb_macs, last_seen) =
                match crate::mqtt_identity::shared().lock() {
                    Ok(reg) => {
                        let eval_time = epoch.max(reg.clock());
                        let verified = reg
                            .verified_report_candidate(&d.optical_sn, eval_time)
                            .map(str::to_owned);
                        let fdb = reg.fdb_candidates_for_serial(&d.optical_sn, eval_time);
                        let seen = verified.as_deref().and_then(|m| reg.last_seen(m));
                        (verified, fdb, seen)
                    }
                    Err(_) => (None, Vec::new(), None),
                };

            let telemetry_verified = verified_mac_str.is_some()
                && last_seen
                    .is_some_and(|ts| epoch.saturating_sub(ts) <= crate::mqtt_identity::FRESHNESS);

            // Resolve candidate MAC: verified report first, otherwise bounded single-FDB-MAC trial on upstream SYN.
            let mut binding_source: Option<String> = None;
            let mut is_trial = false;
            let mut candidate_mac: Option<[u8; 6]> = None;

            if let Some(ref mac_hex) = verified_mac_str {
                if let Some(mac) = parse_mac_hex12(mac_hex) {
                    candidate_mac = Some(mac);
                    binding_source = Some("verified_report".into());
                    c.trials.remove(&d.optical_sn);
                }
            } else if fdb_macs.len() == 1 {
                if let Some(fdb_mac) = parse_mac_hex12(&fdb_macs[0]) {
                    if let Some(syn) = c.syn_attempts.get(&(iface.clone(), fdb_mac)).copied() {
                        if lan.upstream_gateway == Some(syn.destination) {
                            let trial =
                                c.trials.entry(d.optical_sn.clone()).or_insert(TrialState {
                                    mac: fdb_mac,
                                    started_at: now_inst,
                                    failed_until: None,
                                });
                            if trial.mac != fdb_mac {
                                *trial = TrialState {
                                    mac: fdb_mac,
                                    started_at: now_inst,
                                    failed_until: None,
                                };
                            }
                            if let Some(until) = trial.failed_until {
                                if now_inst < until {
                                    next_devices.insert(
                                        d.optical_sn.clone(),
                                        DeviceMqttBridgeStatus {
                                            state: "trial_cooldown".into(),
                                            address_origin: origin_str(lan.origin).into(),
                                            mode: "direct".into(),
                                            interface: Some(iface.clone()),
                                            client_mac: Some(nft::format_mac(fdb_mac)),
                                            client_ip: Some(syn.source.to_string()),
                                            target_broker: Some(format!("{}:1883", syn.destination)),
                                            binding_source: Some("trial_cooldown".into()),
                                            reason: Some(
                                                "Previous 15s identity trial did not receive verified telemetry; cooling down".into(),
                                            ),
                                            ..Default::default()
                                        },
                                    );
                                    continue;
                                }
                                trial.failed_until = None;
                                trial.started_at = now_inst;
                            }
                            if now_inst.saturating_duration_since(trial.started_at) > TRIAL_DURATION
                            {
                                trial.failed_until = Some(now_inst + TRIAL_COOLDOWN);
                                next_devices.insert(
                                    d.optical_sn.clone(),
                                    DeviceMqttBridgeStatus {
                                        state: "trial_cooldown".into(),
                                        address_origin: origin_str(lan.origin).into(),
                                        mode: "direct".into(),
                                        interface: Some(iface.clone()),
                                        client_mac: Some(nft::format_mac(fdb_mac)),
                                        client_ip: Some(syn.source.to_string()),
                                        target_broker: Some(format!("{}:1883", syn.destination)),
                                        binding_source: Some("trial_timeout".into()),
                                        reason: Some(
                                            "Trial redirect expired without matching HL202 telemetry SN".into(),
                                        ),
                                        ..Default::default()
                                    },
                                );
                                continue;
                            }
                            candidate_mac = Some(fdb_mac);
                            binding_source = Some("trial_syn".into());
                            is_trial = true;
                        }
                    }
                }
            }

            let Some(mac) = candidate_mac else {
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: "waiting_identity".into(),
                        address_origin: origin_str(lan.origin).into(),
                        mode: "direct".into(),
                        interface: Some(iface.clone()),
                        telemetry_verified,
                        mqtt_last_seen: last_seen,
                        reason: Some(if fdb_macs.len() > 1 {
                            "Multiple MACs learned on ONU port and no verified HL202 identity cached".into()
                        } else {
                            "Waiting for verified HL202 identity or unique FDB + upstream 1883 SYN".into()
                        }),
                        ..Default::default()
                    },
                );
                continue;
            };

            // Establish per-device address origin and target broker.
            let mut address_origin = Origin::Unknown;
            let mut client_ip: Option<Ipv4Addr> = None;
            let mut target_gw: Option<Ipv4Addr> = None;
            let mut evidence_source: Option<String> = None;
            let mut evidence_updated_at: Option<u64> = None;
            let mut wait_reason: Option<String> = None;

            if let Some(&local_ip) = lan.local_leases_by_mac.get(&mac) {
                address_origin = Origin::Local;
                client_ip = Some(local_ip);
                evidence_source = Some("local_dhcp_lease".into());
                evidence_updated_at = Some(epoch);
                wait_reason = Some(
                    "Device holds a local LAN DHCP lease (NAT mode); redirect disabled".into(),
                );
            } else if let Some(lease) = c.leases.get(&mac) {
                address_origin = lease.origin;
                client_ip = Some(lease.address);
                target_gw = lease.gateway.or(lan.upstream_gateway);
                evidence_source = Some("dhcp_ack".into());
                evidence_updated_at = Some(epoch);
                if lease.origin == Origin::Local {
                    wait_reason =
                        Some("Observed DHCP ACK from local LAN server; redirect disabled".into());
                } else if lease.origin == Origin::Unknown {
                    wait_reason = Some("DHCP server origin is unknown; redirect withheld".into());
                }
            } else if lan.origin == Origin::Upstream {
                if let (Some(lan_ip), Some(gw)) = (lan.lan_ip, lan.upstream_gateway) {
                    if let Some(syn) = c.syn_attempts.get(&(iface.clone(), mac)) {
                        if syn.destination == gw
                            && same_subnet(syn.source, lan_ip, lan.lan_mask)
                            && syn.source != lan_ip
                            && !lan.local_lease_ips.contains(&syn.source)
                        {
                            address_origin = Origin::Upstream;
                            client_ip = Some(syn.source);
                            target_gw = Some(gw);
                            evidence_source = Some("upstream_lan_mqtt_attempt".into());
                            evidence_updated_at = Some(syn.epoch_secs);
                        }
                    }
                }
                if address_origin != Origin::Upstream {
                    wait_reason =
                        Some("Waiting for DHCP ACK or upstream 1883 SYN from this device".into());
                }
            } else {
                wait_reason = lan
                    .reason
                    .clone()
                    .or_else(|| Some("LAN origin is not confirmed as upstream".into()));
            }

            if address_origin == Origin::Local {
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: "local_nat".into(),
                        address_origin: "local".into(),
                        mode: "direct".into(),
                        rule_installed: false,
                        telemetry_verified,
                        interface: Some(iface.clone()),
                        client_mac: Some(nft::format_mac(mac)),
                        client_ip: client_ip.map(|ip| ip.to_string()),
                        target_broker: c.broker_ip.map(|ip| format!("{ip}:1883")),
                        binding_source,
                        evidence_source,
                        evidence_updated_at,
                        mqtt_last_seen: last_seen,
                        reason: wait_reason,
                        ..Default::default()
                    },
                );
                continue;
            }

            if address_origin != Origin::Upstream {
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: "waiting_evidence".into(),
                        address_origin: origin_str(address_origin).into(),
                        mode: "direct".into(),
                        rule_installed: false,
                        telemetry_verified,
                        interface: Some(iface.clone()),
                        client_mac: Some(nft::format_mac(mac)),
                        client_ip: client_ip.map(|ip| ip.to_string()),
                        binding_source,
                        evidence_source,
                        evidence_updated_at,
                        mqtt_last_seen: last_seen,
                        reason: wait_reason,
                        ..Default::default()
                    },
                );
                continue;
            }

            let (Some(client_ip), Some(target_gw), Some(broker_ip), Some(bridge_mac)) =
                (client_ip, target_gw, c.broker_ip, lan.bridge_mac)
            else {
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: "waiting_broker".into(),
                        address_origin: "upstream".into(),
                        mode: "direct".into(),
                        rule_installed: false,
                        telemetry_verified,
                        interface: Some(iface.clone()),
                        client_mac: Some(nft::format_mac(mac)),
                        client_ip: client_ip.map(|ip| ip.to_string()),
                        target_broker: target_gw.map(|ip| format!("{ip}:1883")),
                        binding_source,
                        evidence_source,
                        evidence_updated_at,
                        mqtt_last_seen: last_seen,
                        reason: Some(
                            "Waiting for local MQTT broker and bridge MAC readiness".into(),
                        ),
                    },
                );
                continue;
            };

            if target_gw == broker_ip {
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: if telemetry_verified {
                            "direct_verified"
                        } else {
                            "direct"
                        }
                        .into(),
                        address_origin: "upstream".into(),
                        mode: "direct".into(),
                        rule_installed: false,
                        telemetry_verified,
                        interface: Some(iface.clone()),
                        client_mac: Some(nft::format_mac(mac)),
                        client_ip: Some(client_ip.to_string()),
                        target_broker: Some(format!("{broker_ip}:1883")),
                        binding_source,
                        evidence_source,
                        evidence_updated_at,
                        mqtt_last_seen: last_seen,
                        reason: Some("Target broker already matches local broker".into()),
                    },
                );
                continue;
            }

            desired_rules.push(nft::RedirectRule {
                interface: iface.clone(),
                client_mac: mac,
                client_ip,
                target_gw,
                broker_ip,
                bridge_mac,
            });
            c.tracked_matchers.insert(
                (client_ip, target_gw),
                conntrack::FlowMatcher {
                    client_ip,
                    target_gw,
                    broker_ip,
                },
            );
            next_devices.insert(
                d.optical_sn.clone(),
                DeviceMqttBridgeStatus {
                    state: if is_trial {
                        "trial".into()
                    } else if telemetry_verified {
                        "verified".into()
                    } else {
                        "redirected".into()
                    },
                    address_origin: "upstream".into(),
                    mode: "redirected".into(),
                    rule_installed: true,
                    telemetry_verified,
                    interface: Some(iface.clone()),
                    client_mac: Some(nft::format_mac(mac)),
                    client_ip: Some(client_ip.to_string()),
                    target_broker: Some(format!("{target_gw}:1883 -> {broker_ip}:1883")),
                    binding_source,
                    evidence_source,
                    evidence_updated_at,
                    mqtt_last_seen: last_seen,
                    reason: Some(if is_trial {
                        "15s trial redirect active; waiting for matching HL202 telemetry SN".into()
                    } else if telemetry_verified {
                        "Redirect active and fresh HL202 telemetry verified".into()
                    } else {
                        "Redirect rule installed; waiting for HL202 MQTT reconnect".into()
                    }),
                },
            );
        }

        // Reconcile nftables rules atomically when changed or missing.
        let tables_ok = nft::tables_present();
        let needs_sync = match tables_ok {
            Ok((b, i)) => {
                if desired_rules.is_empty() {
                    b || i || !c.applied_rules.is_empty()
                } else {
                    !b || !i || c.applied_rules != desired_rules
                }
            }
            Err(_) => true,
        };

        if needs_sync {
            if desired_rules.is_empty() {
                if let Err(err) = cleanup_rules_and_conntrack(&mut c) {
                    c.rule_error = Some(err);
                } else {
                    c.rule_error = None;
                }
            } else {
                // Clean up conntrack for any previously redirected tuples that are no longer desired.
                let desired_keys: BTreeSet<(Ipv4Addr, Ipv4Addr)> = desired_rules
                    .iter()
                    .map(|r| (r.client_ip, r.target_gw))
                    .collect();
                let removed_matchers: Vec<_> = c
                    .tracked_matchers
                    .iter()
                    .filter(|(k, _)| !desired_keys.contains(k))
                    .map(|(_, m)| *m)
                    .collect();
                match nft::apply(&desired_rules) {
                    Ok(()) => {
                        c.applied_rules = desired_rules.clone();
                        c.rule_error = None;
                        if !removed_matchers.is_empty() {
                            if let Err(err) =
                                conntrack::cleanup_redirected_flows(&removed_matchers, None)
                            {
                                c.rule_error = Some(err);
                            } else {
                                c.tracked_matchers.retain(|k, _| desired_keys.contains(k));
                            }
                        }
                    }
                    Err(err) => {
                        c.rule_error = Some(err);
                        for dev in next_devices.values_mut() {
                            if dev.rule_installed {
                                dev.rule_installed = false;
                                dev.state = "error".into();
                                dev.reason = c.rule_error.clone();
                            }
                        }
                    }
                }
            }
        }

        c.devices = next_devices;
        let active_count = c.devices.values().filter(|d| d.rule_installed).count() as u32;
        let rules = if active_count > 0 {
            build_global_rules(&c.applied_rules, &c.devices, configured_devices)
        } else {
            Vec::new()
        };
        c.global = MqttBridgeStatus {
            enabled: true,
            state: if c.rule_error.is_some() || c.cleanup_error.is_some() {
                "error".into()
            } else if active_count > 0 {
                "active".into()
            } else {
                "waiting".into()
            },
            lan_origin: origin_str(lan.origin).into(),
            broker_listen: c.broker_ip.map(|ip| format!("{ip}:1883")),
            upstream_gateway: lan.upstream_gateway.map(|ip| ip.to_string()),
            active_rules: active_count,
            rules,
            reason: c
                .rule_error
                .clone()
                .or_else(|| c.cleanup_error.clone())
                .or_else(|| lan.reason.clone()),
            error: c.rule_error.clone().or_else(|| c.cleanup_error.clone()),
        };
    }

    fn build_global_rules(
        applied_rules: &[nft::RedirectRule],
        devices: &BTreeMap<String, DeviceMqttBridgeStatus>,
        configured_devices: &[Device],
    ) -> Vec<MqttBridgeRule> {
        applied_rules
            .iter()
            .map(|rule| {
                let client_mac = nft::format_mac(rule.client_mac);
                let bridge_mac = nft::format_mac(rule.bridge_mac);
                let matched = devices.iter().find(|(_, status)| {
                    status.rule_installed
                        && status.interface.as_deref() == Some(rule.interface.as_str())
                        && status.client_mac.as_deref() == Some(client_mac.as_str())
                });
                let optical_sn = matched.map(|(sn, _)| sn.clone());
                let device = optical_sn.as_ref().and_then(|sn| {
                    configured_devices
                        .iter()
                        .find(|d| &d.optical_sn == sn)
                        .map(|d| d.id.clone())
                });
                let status = matched.map(|(_, s)| s);
                MqttBridgeRule {
                    interface: rule.interface.clone(),
                    client_mac,
                    client_ip: rule.client_ip.to_string(),
                    target_gw: rule.target_gw.to_string(),
                    broker_ip: rule.broker_ip.to_string(),
                    bridge_mac,
                    device,
                    optical_sn,
                    state: status.map(|s| s.state.clone()),
                    binding_source: status.and_then(|s| s.binding_source.clone()),
                    evidence_source: status.and_then(|s| s.evidence_source.clone()),
                    bridge_rule: rule.bridge_rule_expr(),
                    ip_rule: rule.ip_rule_expr(),
                }
            })
            .collect()
    }

    pub(crate) fn global_view() -> Value {
        let c = coordinator().lock().unwrap();
        json!(&c.global)
    }

    pub(crate) fn device_view(optical_sn: &str) -> Value {
        let c = coordinator().lock().unwrap();
        c.devices
            .get(optical_sn)
            .map(|d| json!(d))
            .unwrap_or(Value::Null)
    }
    #[cfg(test)]
    mod tests {
        use super::super::super::mqtt_bridge_evidence::Origin;
        use super::*;
        use std::net::Ipv4Addr;

        #[test]
        fn subnet_and_mac_parsing_helpers_reject_invalid_inputs() {
            assert!(same_subnet(
                Ipv4Addr::new(192, 0, 2, 44),
                Ipv4Addr::new(192, 0, 2, 244),
                24
            ));
            assert!(!same_subnet(
                Ipv4Addr::new(192, 0, 2, 44),
                Ipv4Addr::new(192, 168, 46, 144),
                24
            ));
            assert_eq!(
                parse_mac_hex12("1c:94:68:5d:f9:98"),
                Some([0x1c, 0x94, 0x68, 0x5d, 0xf9, 0x98])
            );
            assert_eq!(parse_mac_hex12("not-a-mac"), None);
        }

        #[test]
        fn local_leases_only_include_lan_subnet_when_local_dhcp_enabled() {
            let lan_ip = Some(Ipv4Addr::new(192, 0, 2, 1));
            let (by_mac, ips) = read_local_leases(lan_ip, 24, false);
            assert!(by_mac.is_empty());
            assert!(ips.is_empty());
            assert!(same_subnet(
                Ipv4Addr::new(192, 0, 2, 100),
                lan_ip.unwrap(),
                24
            ));
            assert!(!same_subnet(
                Ipv4Addr::new(192, 168, 46, 144),
                lan_ip.unwrap(),
                24
            ));
        }

        #[test]
        fn default_coordinator_view_is_disabled_and_unknown() {
            let mut c = Coordinator::default();
            assert!(!c.global.enabled);
            assert_eq!(c.global.state, "disabled");
            c.lan.origin = Origin::Local;
            c.lan.lan_ip = Some(Ipv4Addr::new(192, 0, 2, 1));
            c.lan.local_lan_dhcp = true;
            let origins = server_origins(&c.lan);
            assert_eq!(
                origins.get(&Ipv4Addr::new(192, 0, 2, 1)),
                Some(&Origin::Local)
            );
        }

        #[test]
        fn build_global_rules_enriches_applied_rules_with_device_metadata_and_nft_expressions() {
            let applied = vec![nft::RedirectRule {
                interface: "fttr127".into(),
                client_mac: [0x02, 0, 0, 0, 0, 1],
                client_ip: Ipv4Addr::new(192, 0, 2, 244),
                target_gw: Ipv4Addr::new(192, 0, 2, 1),
                broker_ip: Ipv4Addr::new(192, 0, 2, 44),
                bridge_mac: [0x02, 0, 0, 0, 0, 2],
            }];
            let mut devices = BTreeMap::new();
            devices.insert(
                "H3CT00000001".into(),
                DeviceMqttBridgeStatus {
                    state: "verified".into(),
                    address_origin: "upstream".into(),
                    mode: "redirected".into(),
                    rule_installed: true,
                    telemetry_verified: true,
                    interface: Some("fttr127".into()),
                    client_mac: Some("02:00:00:00:00:01".into()),
                    client_ip: Some("192.0.2.244".into()),
                    target_broker: Some("192.0.2.1:1883 -> 192.0.2.44:1883".into()),
                    binding_source: Some("verified_report".into()),
                    evidence_source: Some("dhcp_ack".into()),
                    ..Default::default()
                },
            );
            let configured = vec![Device {
                id: "onu_hl202".into(),
                optical_sn: "H3CT00000001".into(),
                cwmp_id: String::new(),
                omci_adapter: "hl202".into(),
                bootstrap_adapter: "none".into(),
                provisioning_adapter: "hl202-mqtt".into(),
                resolved_contract: None,
                connection_request: Default::default(),
                items: Vec::new(),
                wifi_cleanup: String::new(),
                wifi_preserve: BTreeSet::new(),
            }];

            let rules = build_global_rules(&applied, &devices, &configured);
            assert_eq!(rules.len(), 1);
            let r = &rules[0];
            assert_eq!(r.device.as_deref(), Some("onu_hl202"));
            assert_eq!(r.optical_sn.as_deref(), Some("H3CT00000001"));
            assert_eq!(r.state.as_deref(), Some("verified"));
            assert_eq!(r.binding_source.as_deref(), Some("verified_report"));
            assert_eq!(r.evidence_source.as_deref(), Some("dhcp_ack"));
            assert_eq!(r.bridge_mac, "02:00:00:00:00:02");
            assert!(r.bridge_rule.contains("iifname \"fttr127\""));
            assert!(r.bridge_rule.contains("ether daddr set 02:00:00:00:00:02"));
            assert!(r.ip_rule.contains("dnat to 192.0.2.44:1883"));
        }
    }
}

#[cfg(feature = "mqtt")]
pub(crate) use active::{configure, device_view, global_view, set_broker_address, shutdown, tick};

#[cfg(not(feature = "mqtt"))]
pub(crate) fn configure(_enabled: bool) {}
#[cfg(not(feature = "mqtt"))]
pub(crate) fn set_broker_address(_addr: Option<std::net::Ipv4Addr>) {}
#[cfg(not(feature = "mqtt"))]
pub(crate) fn shutdown() {}
#[cfg(not(feature = "mqtt"))]
pub(crate) fn tick(
    _online_onus: &std::collections::BTreeMap<u16, String>,
    _interfaces: &std::collections::BTreeMap<u16, String>,
    _configured_devices: &[crate::provisioning::model::Device],
) {
}
#[cfg(not(feature = "mqtt"))]
pub(crate) fn global_view() -> serde_json::Value {
    serde_json::json!(fttr_api::MqttBridgeStatus {
        enabled: false,
        state: "unavailable".into(),
        lan_origin: "unknown".into(),
        reason: Some("MQTT feature not compiled".into()),
        ..Default::default()
    })
}
#[cfg(not(feature = "mqtt"))]
pub(crate) fn device_view(_optical_sn: &str) -> serde_json::Value {
    serde_json::Value::Null
}

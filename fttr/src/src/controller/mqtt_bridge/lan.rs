use super::evidence::{Origin, ServerOrigins};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

#[derive(Clone)]
pub(super) struct LanContext {
    pub origin: Origin,
    pub lan_ip: Option<Ipv4Addr>,
    pub lan_mask: u8,
    pub upstream_gateway: Option<Ipv4Addr>,
    pub upstream_dhcp_server: Option<Ipv4Addr>,
    pub bridge_mac: Option<[u8; 6]>,
    pub local_lan_dhcp: bool,
    pub local_leases_by_mac: BTreeMap<[u8; 6], Ipv4Addr>,
    pub local_lease_ips: BTreeSet<Ipv4Addr>,
    pub reason: Option<String>,
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

pub(super) fn origin_str(origin: Origin) -> &'static str {
    match origin {
        Origin::Local => "local",
        Origin::Upstream => "upstream",
        Origin::Unknown => "unknown",
    }
}

pub(super) fn parse_mac_hex12(hex: &str) -> Option<[u8; 6]> {
    let norm = crate::mqtt_identity::normalize_target(hex)?;
    let mut out = [0u8; 6];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&norm[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub(super) fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, mask: u8) -> bool {
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

pub(super) fn read_local_leases(
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

pub(super) fn inspect_lan_context() -> LanContext {
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

pub(super) fn server_origins(ctx: &LanContext) -> ServerOrigins {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            parse_mac_hex12("02:00:00:00:00:01"),
            Some([0x02, 0x00, 0x00, 0x00, 0x00, 0x01])
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
    fn local_origin_maps_lan_ip_as_local_dhcp_server() {
        let lan = LanContext {
            origin: Origin::Local,
            lan_ip: Some(Ipv4Addr::new(192, 0, 2, 1)),
            local_lan_dhcp: true,
            ..Default::default()
        };
        let origins = server_origins(&lan);
        assert_eq!(
            origins.get(&Ipv4Addr::new(192, 0, 2, 1)),
            Some(&Origin::Local)
        );
    }
}

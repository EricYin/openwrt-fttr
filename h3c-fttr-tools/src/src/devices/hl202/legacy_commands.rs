/// Historical shortcuts, dispatched through the same validated MAC contract.
pub fn handle(line: &str) -> crate::model::IpcResponse {
    use crate::model::IpcResponse;
    let result = (|| -> Result<(), String> {
        let (kind, rest) = line.split_once(' ').ok_or("missing command")?;
        let (target, arguments) = rest.split_once(' ').ok_or("missing target/arguments")?;
        let mac = super::command::normalize_mac(target).map_err(|error| error.to_string())?;
        let words: Vec<_> = arguments.split_whitespace().collect();
        let command = if kind == "EXEC" { arguments.to_string() } else {
            match words.as_slice() {
                ["passwd", ..] if words.len() >= 2 => cmd_set_admin_password(arguments["passwd".len()..].trim()),
                ["user-passwd", ..] if words.len() >= 2 => cmd_set_user_password(arguments["user-passwd".len()..].trim()),
                ["admin", "enable"] => cmd_enable_admin(),
                ["admin", "disable"] => cmd_disable_admin(),
                ["admin", "unlock"] => cmd_unlock_admin(),
                ["telnet", "enable", ..] => cmd_enable_telnet(words.get(2).and_then(|port| port.parse().ok()).unwrap_or(23)),
                ["telnet", "disable"] => cmd_disable_telnet(),
                ["tr069", "disable"] => cmd_disable_tr069(),
                ["tr069", "set", url, ..] => cmd_set_tr069(url, words.get(3).and_then(|vlan| vlan.parse().ok()), true),
                ["iptv", "enable", ..] => cmd_set_iptv(true, words.get(2).and_then(|v| v.parse().ok()).unwrap_or(43),
                    words.get(3).and_then(|v| v.parse().ok()).unwrap_or(2), words.get(4).and_then(|v| v.parse().ok())),
                ["iptv", "disable"] => cmd_set_iptv(false, 0, 2, None),
                ["internet-vlan", vlan, ports @ ..] => {
                    let vlan = vlan.parse().map_err(|_| "invalid VLAN")?;
                    let ports = if ports.is_empty() { vec![1,2,3,4] } else {
                        ports.iter().map(|port| port.parse().map_err(|_| "invalid LAN port")).collect::<Result<Vec<u8>,_>>()?
                    };
                    cmd_set_internet_vlan(vlan, &ports)
                },
                ["wifi", ssid, password, ..] => cmd_set_wifi_local(ssid, password,
                    !words.get(3).is_some_and(|value| ["no", "false", "0"].contains(value))),
                ["mesh", ..] => return Err("mesh payload contract is unverified; no command sent".into()),
                ["reboot"] => cmd_reboot(),
                ["factory-reset"] => cmd_factory_reset(),
                ["status"] => cmd_query_status(),
                _ => return Err("invalid subdev action/arguments".into()),
            }
        };
        super::command::dispatch(&mac, &command).map_err(|error| error.to_string())?;
        Ok(())
    })();
    match result {
        Ok(()) => IpcResponse::ok("dispatched; device execution not confirmed"),
        Err(error) => IpcResponse::err(&error),
    }
}
// Historical TCAPI generators. Execution transport is verified; individual
// configuration shortcuts still require device-side verification by their caller.

/// Generates the TCAPI shell command to update the CUAdmin (super admin) password,
/// ensuring the account is set to Active, unlocked, and persisted to Flash.
pub fn cmd_set_admin_password(password: &str) -> String {
    format!(
        "tcapi set Account_Entry0 username CUAdmin; \
         tcapi set Account_Entry0 web_passwd '{}'; \
         tcapi set Account_Entry0 Active Yes; \
         tcapi set Account_Entry0 isLockState 0; \
         tcapi set Account_Entry0 loginTimes 0; \
         tcapi commit Account; \
         tcapi save",
        escape_single_quotes(password)
    )
}

/// Generates the TCAPI shell command to update the user (standard user) password.
pub fn cmd_set_user_password(password: &str) -> String {
    format!(
        "tcapi set Account_Entry1 username user; \
         tcapi set Account_Entry1 web_passwd '{}'; \
         tcapi set Account_Entry1 Active Yes; \
         tcapi commit Account; \
         tcapi save",
        escape_single_quotes(password)
    )
}

/// Generates the command to enable the CUAdmin super admin account.
pub fn cmd_enable_admin() -> String {
    "tcapi set Account_Entry0 Active Yes; \
     tcapi set Account_Entry0 isLockState 0; \
     tcapi set Account_Entry0 loginTimes 0; \
     tcapi commit Account; \
     tcapi save"
        .to_string()
}

/// Generates the command to disable the CUAdmin super admin account.
pub fn cmd_disable_admin() -> String {
    "tcapi set Account_Entry0 Active No; \
     tcapi commit Account; \
     tcapi save"
        .to_string()
}

/// Generates the command to unlock CUAdmin if locked by repeated login failures.
pub fn cmd_unlock_admin() -> String {
    "tcapi set Account_Entry0 isLockState 0; \
     tcapi set Account_Entry0 loginTimes 0; \
     tcapi commit Account; \
     tcapi save"
        .to_string()
}

/// Generates the command to enable the Telnet debugging daemon with a root shell.
pub fn cmd_enable_telnet(port: u16) -> String {
    format!(
        "tcapi set Account_TelnetEntry Active Yes; \
         tcapi set Account_TelnetEntry telnet_port {}; \
         tcapi commit Account; \
         tcapi save; \
         killall telnetd 2>/dev/null; \
         telnetd -p {} -l /bin/sh &",
        port, port
    )
}

/// Generates the command to stop and disable the Telnet debugging daemon.
pub fn cmd_disable_telnet() -> String {
    "killall telnetd 2>/dev/null; \
     tcapi set Account_TelnetEntry Active No; \
     tcapi commit Account; \
     tcapi save"
        .to_string()
}

/// Generates the command to configure or disable TR-069 (CWMP) management.
pub fn cmd_set_tr069(acs_url: &str, vlan: Option<u16>, enable: bool) -> String {
    let mut parts = Vec::new();
    let act = if enable { "Yes" } else { "No" };
    parts.push(format!("tcapi set Cwmp_Entry Active {}", act));
    parts.push(format!("tcapi set Cwmp_Entry acsUrl '{}'", escape_single_quotes(acs_url)));
    parts.push(format!("tcapi set Cwmp_Entry SyncAcsUrl '{}'", escape_single_quotes(acs_url)));
    parts.push("tcapi commit Cwmp".to_string());

    if let Some(vid) = vlan {
        parts.push(format!("tcapi set Sys_subtr069 SubTr069Enable {}", act));
        parts.push(format!("tcapi set Sys_subtr069 SubTr069Vlan {}", vid));
        parts.push("tcapi commit Sys_subtr069".to_string());
    }
    parts.push("tcapi save".to_string());
    parts.join("; ")
}

/// Generates the command to completely shut down and kill TR-069.
pub fn cmd_disable_tr069() -> String {
    "tcapi set Cwmp_Entry Active No; \
     tcapi set Sys_subtr069 SubTr069Enable No; \
     tcapi commit Cwmp; \
     tcapi commit Sys_subtr069; \
     tcapi save; \
     killall tr69 2>/dev/null"
        .to_string()
}

/// Generates the command to configure IPTV multi-play service on sub-gateway.
pub fn cmd_set_iptv(enable: bool, vlan: u16, eth_port: u8, mvlan: Option<u16>) -> String {
    let act = if enable { "Yes" } else { "No" };
    let mut parts = vec![
        format!("tcapi set Sys_subiptv IPTVEnable {}", act),
        format!("tcapi set Sys_subiptv IPTVEthPort {}", eth_port),
        format!("tcapi set Sys_subiptv IPTVVlanID {}", vlan),
        "tcapi commit Sys_subiptv".to_string(),
    ];
    if let Some(mvid) = mvlan {
        parts.push(format!("tcapi set Sys_subiptv McVlan {}", mvid));
        parts.push("tcapi commit Sys_subiptv".to_string());
    }
    parts.push("tcapi save".to_string());
    parts.join("; ")
}

/// Generates the command to configure downstream Internet VLAN tagging on sub-gateway.
pub fn cmd_set_internet_vlan(vlan_id: u16, lan_ports: &[u8]) -> String {
    let mut parts = vec![
        "tcapi set Wan_PVC1 Active Yes".to_string(),
        "tcapi set Wan_PVC1 ServiceList INTERNET".to_string(),
        format!("tcapi set Wan_PVC1 VLANID {}", vlan_id),
    ];
    for &p in lan_ports {
        parts.push(format!("tcapi set Wan_PVC1 LAN{} Yes", p));
    }
    parts.push("tcapi commit Wan_PVC1; tcapi save".to_string());
    parts.join("; ")
}

/// Generates the command to configure local Wi-Fi SSIDs and credentials via direct TCAPI.
pub fn cmd_set_wifi_local(ssid: &str, password: &str, enable_5g: bool) -> String {
    let esc_ssid = escape_single_quotes(ssid);
    let esc_pwd = escape_single_quotes(password);
    let mut parts = vec![
        format!("tcapi set Wlan_Entry0 SSID '{}'", esc_ssid),
        format!("tcapi set Wlan_Entry0 WPAPSK '{}'", esc_pwd),
        "tcapi set Wlan_Entry0 AuthMode WPA2PSK".to_string(),
        "tcapi set Wlan_Entry0 EncrypType AES".to_string(),
        "tcapi commit Wlan_Entry0".to_string(),
    ];
    if enable_5g {
        parts.push(format!("tcapi set Wlan11ac_Entry0 SSID '{}'", esc_ssid));
        parts.push(format!("tcapi set Wlan11ac_Entry0 WPAPSK '{}'", esc_pwd));
        parts.push("tcapi set Wlan11ac_Entry0 AuthMode WPA2PSK".to_string());
        parts.push("tcapi set Wlan11ac_Entry0 EncrypType AES".to_string());
        parts.push("tcapi commit Wlan11ac_Entry0".to_string());
    }
    parts.push("tcapi save".to_string());
    parts.join("; ")
}

use serde::Serialize;

#[derive(Serialize)]
struct WifiMeshPayload<'a> {
    action: &'static str,
    ssid: &'a str,
    wpa_key: &'a str,
    encryption: &'static str,
    channel_2g: u8,
    channel_5g: u8,
    bandwidth_5g: &'a str,
    roaming: bool,
}

/// Generates the payload for Wi-Fi Mesh EasyMesh JSON synchronization broadcast.
pub fn build_wifi_mesh_json(
    ssid: &str,
    password: &str,
    ch_2g: u8,
    ch_5g: u8,
    bw_5g: &str,
    roaming: bool,
) -> String {
    let payload = WifiMeshPayload {
        action: "mesh_sync",
        ssid,
        wpa_key: password,
        encryption: "sae-mixed",
        channel_2g: ch_2g,
        channel_5g: ch_5g,
        bandwidth_5g: bw_5g,
        roaming,
    };
    serde_json::to_string(&payload).unwrap_or_default()
}

/// Generates the reboot command for the sub-gateway.
pub fn cmd_reboot() -> String {
    "reboot".to_string()
}

/// Generates the factory reset command for the sub-gateway.
pub fn cmd_factory_reset() -> String {
    "tcapi default; reboot".to_string()
}

/// Generates the status and account inspection command for the sub-gateway.
pub fn cmd_query_status() -> String {
    "tcapi show Account; tcapi show Sys_subiptv; tcapi show Cwmp; ip -4 addr show br0".to_string()
}

/// Returns the primary MQTT command execution topic for a sub-gateway.
pub fn topic_subdev_exec(sn: &str) -> String {
    format!("/fttr/subdev/exec/{}", sn)
}

/// Returns the response topic for a sub-gateway execution response.
pub fn topic_subdev_resp(sn: &str) -> String {
    format!("/fttr/subdev/exec/{}/resp", sn)
}

/// Returns the sub-gateway configuration topic (for mesh parameters).
pub fn topic_subdev_config(sn: &str) -> String {
    format!("/fttr/subdev/config/{}", sn)
}

fn escape_single_quotes(s: &str) -> String {
    s.replace('\'', "'\\''")
}

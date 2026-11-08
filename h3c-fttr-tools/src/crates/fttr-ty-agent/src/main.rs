use rumqttc::{Client, Event, MqttOptions, Packet, QoS};
use serde::{Deserialize, Serialize};
use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct ControlCommand {
    cmd: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    ssid_2g: Option<String>,
    #[serde(default)]
    key_2g: Option<String>,
    #[serde(default)]
    ssid_5g: Option<String>,
    #[serde(default)]
    key_5g: Option<String>,
    #[serde(default)]
    command: Option<String>,
}

#[derive(Debug, Serialize)]
struct WifiSlot {
    ssid: String,
    key: String,
}

#[derive(Debug, Serialize)]
struct WifiInfo {
    slot_2g: Option<WifiSlot>,
    slot_5g: Option<WifiSlot>,
}

#[derive(Debug, Serialize)]
struct CommandResponse {
    cmd: String,
    seq: u64,
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wifi: Option<WifiInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

fn get_mac_address() -> String {
    // 1. Try /sys/class/net/br0/address
    if let Ok(addr) = fs::read_to_string("/sys/class/net/br0/address") {
        let clean = addr.trim().replace(':', "").to_lowercase();
        if !clean.is_empty() {
            return clean;
        }
    }
    // 2. Try mdm
    if let Ok(out) = Command::new("/bin/mdm")
        .args([
            "getvalue",
            "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.BSSID",
        ])
        .output()
    {
        let clean = String::from_utf8_lossy(&out.stdout)
            .trim()
            .replace(':', "")
            .to_lowercase();
        if !clean.is_empty() && clean != "null" {
            return clean;
        }
    }
    "ty_subgw_default".to_string()
}

fn get_gateway_ip() -> String {
    if let Ok(routes) = fs::read_to_string("/proc/net/route") {
        for line in routes.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 3 && fields[1] == "00000000" {
                if let Ok(hex_ip) = u32::from_str_radix(fields[2], 16) {
                    let b1 = (hex_ip & 0xff) as u8;
                    let b2 = ((hex_ip >> 8) & 0xff) as u8;
                    let b3 = ((hex_ip >> 16) & 0xff) as u8;
                    let b4 = ((hex_ip >> 24) & 0xff) as u8;
                    return format!("{}.{}.{}.{}", b1, b2, b3, b4);
                }
            }
        }
    }
    "192.168.1.1".to_string()
}

fn mdm_get(path: &str) -> String {
    let out = Command::new("/bin/mdm").args(["getvalue", path]).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

fn mdm_set(path: &str, val: &str) -> bool {
    let out = Command::new("/bin/mdm")
        .args(["setvalue", path, val])
        .status();
    matches!(out, Ok(s) if s.success())
}

fn mdm_save() -> bool {
    let out = Command::new("/bin/mdm").arg("save").status();
    matches!(out, Ok(s) if s.success())
}

fn read_wifi_status() -> WifiInfo {
    let s2 = mdm_get("InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.SSID");
    let k2 = mdm_get(
        "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.PreSharedKey",
    );

    let s5 = mdm_get("InternetGatewayDevice.LANDevice.1.WLANConfiguration.5.SSID");
    let k5 = mdm_get(
        "InternetGatewayDevice.LANDevice.1.WLANConfiguration.5.PreSharedKey.1.PreSharedKey",
    );

    WifiInfo {
        slot_2g: Some(WifiSlot { ssid: s2, key: k2 }),
        slot_5g: Some(WifiSlot { ssid: s5, key: k5 }),
    }
}

fn handle_command(payload: &[u8]) -> Option<CommandResponse> {
    let cmd: ControlCommand = match serde_json::from_slice(payload) {
        Ok(c) => c,
        Err(e) => {
            return Some(CommandResponse {
                cmd: "unknown".to_string(),
                seq: 0,
                code: -1,
                message: format!("json error: {}", e),
                wifi: None,
                output: None,
            });
        }
    };

    match cmd.cmd.as_str() {
        "get_wifi" => {
            let wifi = read_wifi_status();
            Some(CommandResponse {
                cmd: cmd.cmd,
                seq: cmd.seq,
                code: 0,
                message: "ok".to_string(),
                wifi: Some(wifi),
                output: None,
            })
        }
        "set_wifi" => {
            let mut changed = false;
            if let Some(ref s2) = cmd.ssid_2g {
                mdm_set(
                    "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.SSID",
                    s2,
                );
                changed = true;
            }
            if let Some(ref k2) = cmd.key_2g {
                mdm_set("InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.PreSharedKey", k2);
                changed = true;
            }
            if let Some(ref s5) = cmd.ssid_5g {
                mdm_set(
                    "InternetGatewayDevice.LANDevice.1.WLANConfiguration.5.SSID",
                    s5,
                );
                changed = true;
            }
            if let Some(ref k5) = cmd.key_5g {
                mdm_set("InternetGatewayDevice.LANDevice.1.WLANConfiguration.5.PreSharedKey.1.PreSharedKey", k5);
                changed = true;
            }
            if changed {
                mdm_save();
            }
            let wifi = read_wifi_status();
            Some(CommandResponse {
                cmd: cmd.cmd,
                seq: cmd.seq,
                code: 0,
                message: "applied".to_string(),
                wifi: Some(wifi),
                output: None,
            })
        }
        "exec" => {
            let script = cmd.command.unwrap_or_default();
            let out = Command::new("/bin/sh").arg("-c").arg(&script).output();
            let res = match out {
                Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
                Err(e) => format!("exec error: {}", e),
            };
            Some(CommandResponse {
                cmd: cmd.cmd,
                seq: cmd.seq,
                code: 0,
                message: "executed".to_string(),
                wifi: None,
                output: Some(res),
            })
        }
        _ => Some(CommandResponse {
            cmd: cmd.cmd,
            seq: cmd.seq,
            code: -2,
            message: "unsupported command".to_string(),
            wifi: None,
            output: None,
        }),
    }
}

fn main() {
    let mac = get_mac_address();
    let gw = get_gateway_ip();
    println!("[ty-agent] Starting for MAC: {}, Gateway: {}", mac, gw);

    let client_id = format!("ty-agent-{}", mac);
    let mut mqttoptions = MqttOptions::new(&client_id, &gw, 11883);
    mqttoptions.set_keep_alive(Duration::from_secs(30));

    let cmd_topic = format!("fttr/ty/{}/cmd", mac);
    let resp_topic = format!("fttr/ty/{}/resp", mac);
    let online_topic = format!("fttr/ty/{}/online", mac);

    loop {
        let (mut client, mut connection) = Client::new(mqttoptions.clone(), 10);

        // Publish online banner
        let online_payload = serde_json::json!({
            "status": "online",
            "mac": mac,
            "version": "1.0.0",
            "wifi": read_wifi_status()
        });
        let _ = client.publish(
            &online_topic,
            QoS::AtLeastOnce,
            false,
            online_payload.to_string(),
        );

        // Subscribe to command topic
        if let Err(e) = client.subscribe(&cmd_topic, QoS::AtLeastOnce) {
            eprintln!("[ty-agent] Subscribe error: {}, retrying...", e);
            thread::sleep(Duration::from_secs(3));
            continue;
        }

        println!("[ty-agent] Connected and subscribed to {}", cmd_topic);

        // Event loop
        for notification in connection.iter() {
            match notification {
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    if publish.topic == cmd_topic {
                        if let Some(resp) = handle_command(&publish.payload) {
                            if let Ok(json_str) = serde_json::to_string(&resp) {
                                let _ =
                                    client.publish(&resp_topic, QoS::AtLeastOnce, false, json_str);
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("[ty-agent] Connection lost: {}, reconnecting in 5s...", e);
                    break;
                }
            }
        }

        thread::sleep(Duration::from_secs(5));
    }
}

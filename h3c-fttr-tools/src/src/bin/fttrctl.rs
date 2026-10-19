use std::env;
use std::path::Path;
use std::process::{exit, Command};

use h3c_fttr_tools::bosa;
use h3c_fttr_tools::fpga;
use h3c_fttr_tools::ipc;
use h3c_fttr_tools::model::{FttrSystemStatus, IpcResponse};
#[cfg(feature = "device-hl202")]
#[path = "fttrctl/hl202.rs"]
mod hl202;
#[cfg(feature = "device-hl202")]
#[path = "fttrctl/mqtt_command.rs"]
mod mqtt_command;
#[cfg(feature = "device-hl202")]
use mqtt_command::handle as handle_mqtt_command;

fn print_usage() {
    eprintln!(
        "Usage: fttrctl <command> [arguments]\n\n\
Commands:\n  \
  config check|reload  Validate or reload /etc/config/fttr desired state\n  \
  sync plan|apply [DEVICE|all] [--item SECTION] [--management] [--wait SECONDS]\n  \
  load-fpga [path]     Program FPGA bitstream (default: /lib/firmware/FTTR_TOP.sbit)\n  \
  bosa-init            Initialize BOSA optical transceiver registers\n  \
  bridge-setup         Ensure downstream FTTR switch ports (fttr1~16) are bridged to br-lan\n  \
  status [--json|-j]   Display active FTTR optical link and DSA switch ports (table or JSON)\n  \
  mib ONU_ID [--summary|--json] [--tables]  Read structured OMCI inventory (default: JSON)\n  \
  fpga-read <addr>     Directly read 32-bit FPGA hardware register\n  \
  fpga-write <a> <v>   Directly write 32-bit FPGA hardware register\n  \
  bosa-diag [--json]   Read calibrated UX3326 optical diagnostics (Temp, Vcc, Bias, TX)\n  \
  bosa-read <addr>     Directly read 8-bit BOSA UX3326 optical register\n  \
  bosa-write <a> <v>   Directly write 8-bit BOSA UX3326 optical register\n  \
  discover             Trigger PLOAM discovery sequence on optical link\n  \
  register <sn>        Register and activate sub-gateway by SN (e.g. H3CT1A2B3C4D)\n  \
  auth [onu_id]        Send OMCI LOID authentication success to sub-gateway (default: 1)\n  \
  sync-wifi            Push Wi-Fi SSID and credentials to sub-gateways via MQTT\n  \
  provision <onu_id>   Trigger G.988 OMCI provisioning for specific ONU\n  \
   exec [options] <mac> <command>  Execute, wait for combined output and exit status\n  \
   cmd [options] <mac> <command>   Dispatch without waiting for device execution\n  \
   broadcast [options] <command>  Broadcast dispatch, or --wait for per-device replies\n  \
  subdev <sn> <action> Manage sub-gateway proprietary features (passwords, telnet, iptv, etc.)\n  \
  help                 Show this help message"
    );
}

fn status_ipc_error(error: &str) -> serde_json::Value {
    serde_json::json!({
        "status": "unavailable",
        "error": "fttrd status IPC failed",
        "detail": error,
    })
}

#[cfg(not(feature = "device-hl202"))]
fn handle_mqtt_command(_: &[String]) -> Result<i32, String> {
    Err("HL202 adapter not compiled; enable device-hl202".into())
}

fn print_subdev_usage() {
    eprintln!(
        "Usage: fttrctl subdev <sn> <action> [arguments...] [--json|-j]\n\n\
Sub-Gateway Management Actions:\n  \
  passwd <new_pwd>                  Update super administrator (CUAdmin) password and unlock account\n  \
  user-passwd <new_pwd>             Update standard user account password\n  \
  admin <enable|disable|unlock>     Enable, disable, or unlock CUAdmin account\n  \
  telnet <enable|disable> [port]    Enable temporary root Telnet daemon (default port: 23) or disable it\n  \
  tr069 set <acs_url> [vlan]        Configure TR-069 ACS URL and management VLAN\n  \
  tr069 disable                     Completely shut down and disable TR-069\n  \
  iptv enable <vlan> [port] [mvlan] Configure IPTV multi-play VLAN and dedicated LAN port (e.g. 43 2 4094)\n  \
  iptv disable                      Disable IPTV service\n  \
  internet-vlan <vlan> [ports...]   Configure Internet VLAN tagging on sub-gateway LAN ports (e.g. 41 1 2)\n  \
  wifi <ssid> <pwd> [--no-5g]       Configure local Wi-Fi SSIDs and credentials via direct TCAPI\n  \
  mesh <ssid> <pwd> [options]       Broadcast EasyMesh JSON configuration to sub-gateway\n  \
  reboot                            Remote reboot sub-gateway\n  \
  factory-reset                     Restore sub-gateway to factory defaults (tcapi default)\n  \
  status                            Query sub-gateway Account, IPTV, and TR-069 status\n\n\
Examples:\n  \
  fttrctl subdev H3CT1A2B3C4D passwd MyPassw0rd!\n  \
  fttrctl subdev H3CT1A2B3C4D admin unlock\n  \
  fttrctl subdev H3CT1A2B3C4D telnet enable 2323\n  \
  fttrctl subdev H3CT1A2B3C4D iptv enable 43 2 4094\n  \
  fttrctl subdev H3CT1A2B3C4D tr069 disable"
    );
}

fn handle_subdev_command(args: &[String]) {
    let mut is_json = false;
    let filtered_args: Vec<&str> = args
        .iter()
        .map(|s| s.as_str())
        .filter(|&a| {
            if a == "--json" || a == "-j" {
                is_json = true;
                false
            } else {
                true
            }
        })
        .collect();

    // filtered_args[0] is "subdev"
    if filtered_args.len() < 3 {
        print_subdev_usage();
        exit(1);
    }

    let sn = filtered_args[1];
    let action = filtered_args[2];
    let extra_args = &filtered_args[3..];

    let mut processed_extra: Vec<&str> = Vec::new();
    for &arg in extra_args {
        if arg == "--no-5g" {
            processed_extra.push("false");
        } else {
            processed_extra.push(arg);
        }
    }

    let payload = if processed_extra.is_empty() {
        format!("SUBDEV {sn} {action}")
    } else {
        format!("SUBDEV {sn} {action} {}", processed_extra.join(" "))
    };

    match ipc::send_ipc_command(&payload) {
        Ok(resp) => {
            if is_json {
                print!("{resp}");
            } else {
                match serde_json::from_str::<IpcResponse>(&resp) {
                    Ok(r) => {
                        if let Some(err) = r.error {
                            eprintln!("[-] Error from sub-gateway daemon: {err}");
                            exit(1);
                        } else {
                            println!("[+] Successfully dispatched action '{action}' to sub-gateway {sn} via MQTT.");
                        }
                    }
                    Err(_) => {
                        println!("{resp}");
                    }
                }
            }
        }
        Err(e) => {
            if is_json {
                println!("{{\"error\": \"fttrd IPC error: {}\"}}", e);
            } else {
                eprintln!("[-] Failed to communicate with fttrd daemon: {e}");
            }
            exit(1);
        }
    }
}

fn setup_bridge() {
    println!("[*] Configuring downstream FTTR DSA switch ports (fttr1 ~ fttr16) to br-lan...");
    for id in 1..=16 {
        let ifname = format!("fttr{id}");
        let _ = Command::new("ip")
            .args(["link", "set", &ifname, "up"])
            .status();
        let _ = Command::new("ip")
            .args(["link", "set", &ifname, "master", "br-lan"])
            .status();
    }
    println!("[+] Downstream switch ports fttr1 ~ fttr16 configured and bridged to br-lan.");
}

fn print_status_table(status: &FttrSystemStatus) {
    println!("==========================================================================================");
    println!(
        "                   H3C HM2004-DU FTTR Micro-OLT Subsystem Status                         "
    );
    println!("==========================================================================================");

    // 0. FPGA Core Status
    let fpga_badge = match status.fpga_status.as_str() {
        "ready" => "[READY / ACTIVE]",
        "programming" => "[PROGRAMMING / IN PROGRESS]",
        "failed" => "[FAILED / ERROR]",
        _ => "[UNKNOWN / NOT STARTED]",
    };
    println!(
        "[*] FPGA Micro-OLT Core:    {} (Status: {})",
        fpga_badge, status.fpga_status
    );
    if !status.provisioning.is_null() {
        println!(
            "[*] Provisioning: {} / {} / revision {}",
            status.provisioning["enabled"],
            status.provisioning["mode"].as_str().unwrap_or("unknown"),
            status.provisioning["revision"]
                .as_str()
                .unwrap_or("unknown")
        );
        if let Some(error) = status.provisioning["error"].as_str() {
            println!("    Config error: {error}");
        }
    }
    println!();

    // 1. Optical Link
    let opt = &status.optical_transceiver;
    let cdr_str = match opt.cdr_locked {
        Some(true) => "LOCKED (OK)",
        Some(false) => "UNLOCKED",
        None => "UNKNOWN",
    };

    println!("[*] Optical Transceiver (BOSA {}):", opt.model);
    println!(
        "    Wavelength: TX {}nm / RX {}nm  |  Rate: 2.488G / 1.244G  |  CDR: {}",
        opt.wavelength_tx_nm, opt.wavelength_rx_nm, cdr_str
    );
    if opt.temperature_celsius.is_some()
        || opt.vcc_voltage.is_some()
        || opt.laser_bias_current_ma.is_some()
        || opt.tx_power_dbm.is_some()
    {
        let temp_str = opt
            .temperature_celsius
            .map(|t| format!("{t:.2} °C"))
            .unwrap_or_else(|| "N/A".into());
        let volt_str = opt
            .vcc_voltage
            .map(|v| format!("{v:.2} V"))
            .unwrap_or_else(|| "N/A".into());
        let bias_str = opt
            .laser_bias_current_ma
            .map(|b| format!("{b:.2} mA"))
            .unwrap_or_else(|| "N/A".into());
        let tx_str = opt
            .tx_power_dbm
            .map(|p| format!("{p:.2} dBm"))
            .unwrap_or_else(|| "N/A".into());
        println!(
            "    Diagnostics: Temp: {}  |  Voltage: {}  |  Bias: {}  |  TX: {}",
            temp_str, volt_str, bias_str, tx_str
        );
    } else {
        println!("    Optical diagnostics: unavailable (no verified hardware readings)");
    }
    println!();

    // 2. Switch Ports Table
    println!("------------------------------------------------------------------------------------------");
    println!(" Downstream DSA Switch Ports (fttr1 ~ fttr16)");
    println!("----------------------------------------------------------------------------------------------------");
    println!(
        " {:<6} {:<10} {:<10} {:<12} {:<8} {:<16} {:<24}",
        "Port", "Interface", "Carrier", "Link Speed", "ONU-ID", "ONU Serial", "Link State"
    );
    println!("----------------------------------------------------------------------------------------------------");

    let mut active_count = 0;
    let mut in_prog_count = 0;
    for p in &status.switch_ports {
        let is_up = p.carrier;
        let speed = "Unavailable";
        let onu_id_str = p
            .onu_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-".to_string());
        let onu_sn_str = p.onu_sn.as_deref().unwrap_or("-");
        let link_state = if !p.link_state.is_empty() {
            p.link_state.as_str()
        } else if is_up {
            "O5Operation (Online)"
        } else {
            "Offline"
        };

        if is_up {
            active_count += 1;
        } else if p.onu_id.is_some() || link_state != "Offline" {
            in_prog_count += 1;
        }

        println!(
            " {:<6} {:<10} {:<10} {:<12} {:<8} {:<16} {:<24}",
            p.port, p.interface, p.status, speed, onu_id_str, onu_sn_str, link_state
        );
    }
    println!("----------------------------------------------------------------------------------------------------");
    if in_prog_count > 0 {
        println!(
            " Downlink Ports: {} Carrier Up, {} Assigned Without Carrier / 16 Total",
            active_count, in_prog_count
        );
    } else {
        println!(" Active Downlink Ports: {} / 16", active_count);
    }
    println!();

    // 3. Sub-Gateways Details (if any)
    if !status.sub_gateways.is_empty() {
        println!("------------------------------------------------------------------------------------------");
        println!(" Active Sub-Gateway Telemetry & Mesh Details");
        println!("------------------------------------------------------------------------------------------");
        for sg in &status.sub_gateways {
            println!(
                "[+] Sub-Gateway #{} [{}] - {} {}",
                sg.onu_id, sg.serial_number, sg.vendor, sg.model
            );
            println!(
                "    Switch Port:    fttr{} (VLAN Tag: {}, State: {:?})",
                sg.onu_id, sg.onu_id, sg.state
            );
            println!("    Business:       {}", sg.business_status);
            if !sg.provisioning.is_null() {
                println!(
                    "    CWMP:           {} | Sync: {} | Wi-Fi: {}",
                    sg.management["state"].as_str().unwrap_or("unknown"),
                    sg.provisioning["state"].as_str().unwrap_or("not_started"),
                    sg.wifi_sync
                );
                for item in sg.services.iter().chain(sg.ssids.iter()) {
                    println!(
                        "      {} [{}]: {} {}",
                        item["id"].as_str().unwrap_or("?"),
                        item["service_type"].as_str().unwrap_or("?"),
                        item["state"].as_str().unwrap_or("?"),
                        item["reason"].as_str().unwrap_or("")
                    );
                }
            }
            println!("    Hardware:       {}", sg.hardware_version);
            println!("    Software:       {}", sg.firmware_version);
            if let Some(identity) = &sg.identity {
                let age = sg
                    .identity_age_seconds
                    .map(|seconds| format!("{seconds}s ago"))
                    .unwrap_or_else(|| "unknown age".into());
                println!(
                    "    Identity:       OMCI cache, {}, {age}",
                    if sg.identity_current {
                        "current registration"
                    } else {
                        "offline / cached"
                    }
                );
                println!(
                    "    OMCC version:   {}",
                    identity
                        .omcc_version
                        .map(|value| format!("0x{value:02x}"))
                        .unwrap_or_else(|| "Unavailable".into())
                );
                for error in &identity.errors {
                    println!("    Identity read:  {error}");
                }
            } else {
                println!("    Identity:       not collected yet");
            }
            if let Some(bits) = sg.equalization_delay_bits {
                println!(
                    "    Measured EqD:   {bits} bits (Ranging_Time sent; see protocol state above)"
                );
            }
            println!(
                "    Fiber Distance: {} | Optical RX: {} | Optical TX: {}",
                sg.fiber_distance_m
                    .map(|v| format!("{v:.1} m"))
                    .unwrap_or_else(|| "Unavailable".into()),
                sg.optical_rx_power_dbm
                    .map(|v| format!("{v:.3} dBm"))
                    .unwrap_or_else(|| "Unavailable".into()),
                sg.optical_tx_power_dbm
                    .map(|v| format!("{v:.3} dBm"))
                    .unwrap_or_else(|| "Unavailable".into())
            );
            println!(
                "    Wi-Fi Mesh:     2.4G Ch {} / 5G Ch {} ({})",
                sg.wifi_mesh
                    .channel_2g
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "Unavailable".into()),
                sg.wifi_mesh
                    .channel_5g
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "Unavailable".into()),
                sg.wifi_mesh
                    .bandwidth_5g
                    .as_deref()
                    .unwrap_or("Unavailable")
            );

            if let Some(clients) = &sg.connected_clients {
                println!("    Connected Mesh Clients:");
                for c in clients {
                    println!(
                        "      - {} | {} | {} ({} dBm) | {}/{} Mbps",
                        c.mac,
                        c.ip,
                        c.band,
                        c.rssi_dbm
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "Unavailable".into()),
                        c.rx_rate_mbps
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "Unavailable".into()),
                        c.tx_rate_mbps
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "Unavailable".into())
                    );
                }
            }
            println!();
        }
    }
    println!("==========================================================================================");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        exit(1);
    }

    match args[1].as_str() {
        "acs" => match h3c_fttr_tools::acs_cli::run(&args[2..]) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("ACS: {e}");
                std::process::exit(1);
            }
        },
        "config" | "sync" => match h3c_fttr_tools::provisioning_cli::run(&args[1], &args[2..]) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("Provisioning: {e}");
                exit(1);
            }
        },
        "load-fpga" => {
            let path_str = args
                .get(2)
                .map(|s| s.as_str())
                .unwrap_or("/lib/firmware/FTTR_TOP.sbit");
            let path = Path::new(path_str);
            if let Err(e) = fpga::load_bitstream(path) {
                eprintln!("[-] Error programming FPGA: {e}");
                exit(1);
            }
        }
        "bosa-init" => {
            if let Err(e) = bosa::init_optical_transceiver() {
                eprintln!("[-] Error initializing BOSA: {e}");
                exit(1);
            }
        }
        "bridge-setup" => {
            setup_bridge();
        }
        "fpga-read" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl fpga-read <hex_or_dec_addr>");
                exit(1);
            }
            let addr_str = &args[2];
            let addr = if addr_str.starts_with("0x") || addr_str.starts_with("0X") {
                u32::from_str_radix(&addr_str[2..], 16).unwrap_or(0)
            } else {
                addr_str.parse::<u32>().unwrap_or(0)
            };
            match bosa::read_fpga_reg(addr) {
                Ok(val) => println!("FPGA Reg 0x{:08X} = 0x{:08X} ({})", addr, val, val),
                Err(e) => {
                    eprintln!("[-] Error reading FPGA reg 0x{:08X}: {e}", addr);
                    exit(1);
                }
            }
        }
        "fpga-write" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl fpga-write <hex_or_dec_addr> <hex_or_dec_val>");
                exit(1);
            }
            let addr = if args[2].starts_with("0x") || args[2].starts_with("0X") {
                u32::from_str_radix(&args[2][2..], 16).unwrap_or(0)
            } else {
                args[2].parse::<u32>().unwrap_or(0)
            };
            let val = if args[3].starts_with("0x") || args[3].starts_with("0X") {
                u32::from_str_radix(&args[3][2..], 16).unwrap_or(0)
            } else {
                args[3].parse::<u32>().unwrap_or(0)
            };
            match bosa::write_fpga_reg(addr, val) {
                Ok(()) => println!(
                    "[+] Successfully wrote FPGA reg 0x{:08X} = 0x{:08X}",
                    addr, val
                ),
                Err(e) => {
                    eprintln!("[-] Error writing FPGA reg 0x{:08X}: {e}", addr);
                    exit(1);
                }
            }
        }
        "bosa-diag" => {
            let is_json = args.iter().any(|a| a == "--json" || a == "-j");
            let opt = bosa::get_optical_status();
            if is_json {
                println!("{}", serde_json::to_string_pretty(&opt).unwrap_or_default());
            } else {
                let cdr_str = match opt.cdr_locked {
                    Some(true) => "LOCKED (OK)",
                    Some(false) => "UNLOCKED",
                    None => "UNKNOWN",
                };
                println!("[*] Optical Transceiver (BOSA {}):", opt.model);
                println!(
                    "    Wavelength: TX {}nm / RX {}nm  |  Rate: 2.488G / 1.244G  |  CDR: {}",
                    opt.wavelength_tx_nm, opt.wavelength_rx_nm, cdr_str
                );
                let temp_str = opt
                    .temperature_celsius
                    .map(|t| format!("{t:.2} °C"))
                    .unwrap_or_else(|| "N/A".into());
                let volt_str = opt
                    .vcc_voltage
                    .map(|v| format!("{v:.2} V"))
                    .unwrap_or_else(|| "N/A".into());
                let bias_str = opt
                    .laser_bias_current_ma
                    .map(|b| format!("{b:.2} mA"))
                    .unwrap_or_else(|| "N/A".into());
                let tx_str = opt
                    .tx_power_dbm
                    .map(|p| format!("{p:.2} dBm"))
                    .unwrap_or_else(|| "N/A".into());
                println!(
                    "    Diagnostics: Temp: {}  |  Voltage: {}  |  Bias: {}  |  TX: {}",
                    temp_str, volt_str, bias_str, tx_str
                );
                println!("    Measurements Evidence:");
                for (name, ev) in &opt.measurements {
                    let raw_val = ev.raw.as_deref().unwrap_or("-");
                    let reason_val = ev.reason.as_deref().unwrap_or("none");
                    println!("      - {name:<22}: raw={raw_val:<6} unit={:<5} source=\"{}\" (reason: {reason_val})", ev.unit, ev.source);
                }
            }
        }
        "bosa-read" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl bosa-read <hex_or_dec_addr>");
                exit(1);
            }
            let addr = if args[2].starts_with("0x") || args[2].starts_with("0X") {
                u32::from_str_radix(&args[2][2..], 16).unwrap_or(0)
            } else {
                args[2].parse::<u32>().unwrap_or(0)
            };
            match bosa::read_bosa_reg(addr) {
                Ok(val) => println!("BOSA Reg 0x{:02X} = 0x{:02X} ({})", addr, val, val),
                Err(e) => {
                    eprintln!("[-] Error reading BOSA reg 0x{:02X}: {e}", addr);
                    exit(1);
                }
            }
        }
        "bosa-write" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl bosa-write <hex_or_dec_addr> <hex_or_dec_val>");
                exit(1);
            }
            let addr = if args[2].starts_with("0x") || args[2].starts_with("0X") {
                u32::from_str_radix(&args[2][2..], 16).unwrap_or(0)
            } else {
                args[2].parse::<u32>().unwrap_or(0)
            };
            let val = if args[3].starts_with("0x") || args[3].starts_with("0X") {
                u32::from_str_radix(&args[3][2..], 16).unwrap_or(0)
            } else {
                args[3].parse::<u32>().unwrap_or(0)
            };
            match bosa::write_bosa_reg(addr, val) {
                Ok(()) => println!(
                    "[+] Successfully wrote BOSA reg 0x{:02X} = 0x{:02X}",
                    addr, val
                ),
                Err(e) => {
                    eprintln!("[-] Error writing BOSA reg 0x{:02X}: {e}", addr);
                    exit(1);
                }
            }
        }
        "status" => {
            let is_json = args.iter().any(|a| a == "--json" || a == "-j");
            match ipc::send_ipc_command("STATUS") {
                Ok(resp) => {
                    if is_json {
                        println!("{resp}");
                    } else {
                        match serde_json::from_str::<FttrSystemStatus>(&resp) {
                            Ok(parsed) => print_status_table(&parsed),
                            Err(e) => {
                                eprintln!("[-] Failed to parse daemon status ({e}). Raw response:\n{resp}");
                            }
                        }
                    }
                }
                Err(error) => {
                    let fpga_st = fpga::get_fpga_status();
                    if fpga_st == fpga::FpgaStatus::Programming {
                        if is_json {
                            println!("{{\"fpga_status\": \"programming\", \"message\": \"FPGA bitstream is currently programming in background, please wait...\"}}");
                        } else {
                            println!("[*] FPGA 位流正在内核后台烧录中（Micro-OLT 硬件正在初始化，约需数十秒），请稍候...");
                        }
                        return;
                    } else if fpga_st == fpga::FpgaStatus::Failed {
                        if is_json {
                            println!("{{\"fpga_status\": \"failed\", \"error\": \"FPGA bitstream programming failed in kernel\"}}");
                        } else {
                            eprintln!(
                                "[!] 错误：内核 FPGA 位流烧录失败！请检查系统日志（dmesg）。"
                            );
                        }
                        return;
                    }

                    if is_json {
                        println!("{}", status_ipc_error(&error.to_string()));
                    } else {
                        eprintln!("[-] fttrd status unavailable: IPC request failed: {error}");
                    }
                    exit(1);
                }
            }
        }
        "mib" => {
            let id = args
                .get(2)
                .and_then(|value| value.parse::<u8>().ok())
                .filter(|id| (1..=15).contains(id));
            let summary = args.iter().any(|arg| arg == "--summary");
            let mut progress_id = None;
            let Some(id) = id else {
                eprintln!("Usage: fttrctl mib ONU_ID [--summary|--json] [--tables]");
                exit(1);
            };
            let mut options = args[3..].iter();
            while let Some(option) = options.next() {
                match option.as_str() {
                    "--summary" | "--json" | "--tables" => {}
                    "--progress-id" if progress_id.is_none() => {
                        progress_id = options.next().filter(|value| !value.is_empty()).cloned();
                        if progress_id.is_none() {
                            eprintln!("Usage: fttrctl mib ONU_ID [--summary|--json] [--tables] [--progress-id ID]");
                            exit(1);
                        }
                    }
                    _ => {
                        eprintln!("Usage: fttrctl mib ONU_ID [--summary|--json] [--tables] [--progress-id ID]");
                        exit(1);
                    }
                }
            }
            if summary && args.iter().any(|arg| arg == "--json") {
                eprintln!(
                    "Usage: fttrctl mib ONU_ID [--summary|--json] [--tables] [--progress-id ID]"
                );
                exit(1);
            }
            if let Err(error) = h3c_fttr_tools::controller::inventory::read_with_progress(
                id,
                summary,
                args.iter().any(|arg| arg == "--tables"),
                progress_id,
            ) {
                eprintln!("[-] MIB read failed: {error}");
                exit(1);
            }
        }
        "discover" => match ipc::send_ipc_command("DISCOVER") {
            Ok(resp) => println!("{resp}"),
            Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
        },
        "register" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl register <sn> (e.g. H3CT1A2B3C4D)");
                exit(1);
            }
            let sn = &args[2];
            let payload = format!("REGISTER {sn}");
            match ipc::send_ipc_command(&payload) {
                Ok(resp) => println!("{resp}"),
                Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
            }
        }
        "auth" => {
            let id = args.get(2).map(|s| s.as_str()).unwrap_or("1");
            let payload = format!("AUTH {id}");
            match ipc::send_ipc_command(&payload) {
                Ok(resp) => println!("{resp}"),
                Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
            }
        }
        "sync-wifi" => match ipc::send_ipc_command("SYNC_WIFI") {
            Ok(resp) => println!("{resp}"),
            Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
        },
        "provision" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl provision <onu_id>");
                exit(1);
            }
            let id = &args[2];
            let payload = format!("PROVISION {id}");
            match ipc::send_ipc_command(&payload) {
                Ok(resp) => println!("{resp}"),
                Err(e) => eprintln!("[-] Failed to communicate with fttrd: {e}"),
            }
        }
        "exec" | "cmd" | "broadcast" => match handle_mqtt_command(&args[1..]) {
            Ok(code) => exit(code),
            Err(error) => {
                eprintln!("[-] {error}");
                exit(1);
            }
        },
        "hl202" => {
            #[cfg(feature = "device-hl202")]
            match hl202::handle(&args[2..]) {
                Ok(code) => exit(code),
                Err(error) => {
                    eprintln!("[-] {error}");
                    exit(1);
                }
            }
            #[cfg(not(feature = "device-hl202"))]
            {
                eprintln!("HL202 adapter not compiled");
                exit(1);
            }
        }
        "subdev" => {
            handle_subdev_command(&args[1..]);
        }
        "read-reg" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl read-reg <addr>");
                exit(1);
            }
            let addr_str = &args[2];
            let addr = u32::from_str_radix(
                addr_str.trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex address");
            match bosa::read_fpga_reg(addr) {
                Ok(val) => println!("FPGA [0x{:08X}] = 0x{:08X}", addr, val),
                Err(e) => eprintln!("[-] Error reading FPGA register: {e}"),
            }
        }
        "write-reg" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl write-reg <addr> <val>");
                exit(1);
            }
            let addr = u32::from_str_radix(
                args[2].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex address");
            let val = u32::from_str_radix(
                args[3].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex value");
            match bosa::write_fpga_reg(addr, val) {
                Ok(_) => println!("FPGA [0x{:08X}] <= 0x{:08X}", addr, val),
                Err(e) => eprintln!("[-] Error writing FPGA register: {e}"),
            }
        }
        "read-bosa" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl read-bosa <reg>");
                exit(1);
            }
            let reg = u32::from_str_radix(
                args[2].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex register");
            match bosa::read_bosa_reg(reg) {
                Ok(val) => println!("BOSA [0x{:02X}] = 0x{:02X}", reg, val),
                Err(e) => eprintln!("[-] Error reading BOSA register: {e}"),
            }
        }
        "write-bosa" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl write-bosa <reg> <val>");
                exit(1);
            }
            let reg = u32::from_str_radix(
                args[2].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex register");
            let val = u32::from_str_radix(
                args[3].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex value");
            match bosa::write_bosa_reg(reg, val) {
                Ok(_) => println!("BOSA [0x{:02X}] <= 0x{:02X}", reg, val),
                Err(e) => eprintln!("[-] Error writing BOSA register: {e}"),
            }
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        unknown => {
            eprintln!("[-] Unknown command: {unknown}\n");
            print_usage();
            exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::status_ipc_error;

    #[test]
    fn ipc_failure_is_an_explicit_unavailable_result_not_device_status() {
        let response = status_ipc_error("Connection refused");
        assert_eq!(response["status"], "unavailable");
        assert_eq!(response["error"], "fttrd status IPC failed");
        assert_eq!(response["detail"], "Connection refused");
        assert!(response.get("devices").is_none());
        assert!(response.get("sub_gateways").is_none());
    }
}

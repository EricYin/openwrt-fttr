use h3c_fttr_tools::devices::hl202::{
    command::{self, CommandResult},
    script,
};
use h3c_fttr_tools::{ipc, model::FttrSystemStatus};
use std::{path::Path, time::Duration};

fn usage() {
    eprintln!("Usage:\n  fttrctl exec [--wait|--no-wait] [--timeout SECONDS] [--json] <mac> 'shell command'\n  fttrctl exec --script-file PATH [--timeout SECONDS] [--json] <mac>\n  fttrctl cmd [--wait] [--json] <mac> 'shell command'\n  fttrctl broadcast [--wait] [--targets MAC,MAC] [--timeout SECONDS] [--json] 'shell command'\n\nOptions must precede the shell command; -- ends option parsing.\nThe target is the child br0 MAC, not its optical SN. Default timeout: 5 seconds (1..300).\nBroadcast --wait fans out to fresh telemetry devices (last seen <=15s), or explicit --targets.\nDispatch success only means a QoS 0 socket write. Timeout does not cancel execution.");
}

pub fn handle(args: &[String]) -> Result<i32, String> {
    let mode = args.first().ok_or("missing command mode")?.as_str();
    let broadcast = mode == "broadcast";
    let mut wait = mode == "exec";
    let mut json = false;
    let mut seconds = 5u64;
    let mut target = None;
    let mut targets = None;
    let mut script_file = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                usage();
                return Ok(0);
            }
            "--wait" => wait = true,
            "--no-wait" => wait = false,
            "--json" | "-j" => json = true,
            "--timeout" => {
                i += 1;
                seconds = args
                    .get(i)
                    .ok_or("--timeout needs seconds")?
                    .parse()
                    .map_err(|_| "invalid timeout")?;
                if !(1..=300).contains(&seconds) {
                    return Err("timeout must be 1..300 seconds".into());
                }
            }
            "--script-file" => {
                if mode != "exec" || script_file.is_some() {
                    return Err("--script-file is accepted once, for exec only".into());
                }
                i += 1;
                script_file = Some(args.get(i).ok_or("--script-file needs a path")?);
            }
            "--targets" => {
                i += 1;
                if !broadcast {
                    return Err("--targets is only valid for broadcast --wait".into());
                }
                targets = Some(
                    args.get(i)
                        .ok_or("--targets needs comma-separated MACs")?
                        .clone(),
                );
            }
            "--" => {
                i += 1;
                break;
            }
            option if option.starts_with('-') => return Err(format!("unknown option: {option}")),
            _ if !broadcast && target.is_none() => target = Some(args[i].clone()),
            _ => break,
        }
        i += 1;
    }
    if (!broadcast && target.is_none()) || (script_file.is_none() && i >= args.len()) {
        usage();
        return Err("missing target or shell command".into());
    }
    if targets.is_some() && !wait {
        return Err("--targets requires --wait".into());
    }
    let source = match script_file {
        Some(path) => {
            if i != args.len() {
                return Err("provide either --script-file or a positional shell command".into());
            }
            script::read_file(Path::new(path))
                .map_err(|e| format!("cannot read script input: {e}"))?
        }
        None => script::validate(args[i..].join(" ")).map_err(|e| e.to_string())?,
    };
    let devices = if broadcast && wait {
        let values = if let Some(values) = targets {
            values.split(',').map(str::to_string).collect::<Vec<_>>()
        } else {
            let reply = ipc::send_ipc_command("STATUS")
                .map_err(|e| format!("cannot discover devices: {e}; use --targets"))?;
            let status: FttrSystemStatus = serde_json::from_str(&reply)
                .map_err(|e| format!("invalid STATUS response: {e}"))?;
            status
                .telemetry_gateways
                .into_iter()
                .filter(|d| d.last_seen_sec_ago <= 15)
                .map(|d| d.id.split('/').next().unwrap_or("").to_string())
                .collect()
        };
        let mut macs = Vec::new();
        for value in values {
            let mac = command::normalize_mac(&value).map_err(|e| e.to_string())?;
            if !macs.contains(&mac) {
                macs.push(mac);
            }
        }
        if macs.is_empty() {
            return Err("no fresh telemetry devices; specify --targets MAC,MAC".into());
        }
        macs
    } else if broadcast {
        vec!["all".to_string()]
    } else {
        vec![command::normalize_mac(&target.ok_or("missing target")?).map_err(|e| e.to_string())?]
    };
    let mut results = Vec::new();
    for device in devices {
        let result = if wait {
            command::execute(&device, &source, Duration::from_secs(seconds))
        } else {
            command::dispatch(&device, &source)
        }
        .unwrap_or_else(|e| CommandResult::failed(&device, e));
        if !json {
            if let Some(output) = &result.output {
                print!("{output}");
            }
            if let Some(error) = &result.error {
                eprintln!("[-] {} {}: {error}", result.target, result.status);
            } else if let Some(code) = result.exit_code {
                eprintln!(
                    "[{}] {} executed, exit={code}",
                    if code == 0 { "+" } else { "-" },
                    result.target
                );
            } else {
                println!(
                    "[+] {} dispatched (device execution not confirmed)",
                    result.target
                );
            }
        }
        results.push(result);
    }
    let code = if results.iter().all(CommandResult::success) {
        0
    } else if results.len() == 1 {
        results[0]
            .exit_code
            .filter(|c| (1..=255).contains(c))
            .unwrap_or(1)
    } else {
        1
    };
    if json {
        let output = if broadcast {
            serde_json::to_string(&results)
        } else {
            serde_json::to_string(&results[0])
        }
        .map_err(|e| e.to_string())?;
        println!("{output}");
    }
    Ok(code)
}

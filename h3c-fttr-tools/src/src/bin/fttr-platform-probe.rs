// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::process;

fn test_uci() -> Result<(), String> {
    println!("[PROBE] Testing openwrt-uci dynamic loader...");
    let mut uci = match openwrt_uci::Uci::new() {
        Ok(u) => {
            println!("  [+] Successfully loaded OpenWrt libuci library and created context.");
            u
        }
        Err(e) => {
            println!("  [-] libuci load/context failed: {e}");
            return Err(e.to_string());
        }
    };

    println!("[PROBE] Testing UCI read on 'system' package...");
    match uci.get("system", "@system[0]", "hostname") {
        Ok(Some(hostname)) => {
            println!("  [+] Retrieved system hostname: {hostname}");
        }
        Ok(None) => {
            println!("  [*] system.hostname option not found (empty or non-default).");
        }
        Err(e) => {
            println!("  [*] UCI get failed (may be normal if package not loaded): {e}");
        }
    }

    Ok(())
}

fn test_ubus() -> Result<(), String> {
    println!("[PROBE] Testing openwrt-ubus dynamic loader...");
    let mut ubus = match openwrt_ubus::Ubus::connect(None) {
        Ok(u) => {
            println!("  [+] Successfully connected to OpenWrt ubus daemon.");
            u
        }
        Err(e) => {
            println!("  [-] ubus connect/load failed: {e}");
            return Err(e.to_string());
        }
    };

    println!("[PROBE] Calling 'system.info' via ubus RPC...");
    match ubus.call("system", "info", None, 3000) {
        Ok(res) => {
            println!("  [+] system.info response: {res}");
        }
        Err(e) => {
            println!("  [*] system.info call returned: {e}");
        }
    }

    Ok(())
}

fn main() {
    println!("=== FTTR OpenWrt Native Platform Capability Probe ===");
    let uci_ok = test_uci().is_ok();
    let ubus_ok = test_ubus().is_ok();

    println!("=====================================================");
    println!("Summary: UCI={uci_ok}, UBUS={ubus_ok}");
    if uci_ok && ubus_ok {
        println!("All native platform capabilities are verified on this device.");
        process::exit(0);
    } else {
        println!("Running in non-OpenWrt host or libraries absent (expected on dev workstation).");
        process::exit(0);
    }
}

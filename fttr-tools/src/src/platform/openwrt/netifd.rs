// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use openwrt_ubus::Ubus;
use serde::Deserialize;

#[derive(Deserialize, Debug, Default, Clone)]
pub struct NetifdDump {
    #[serde(default)]
    pub interface: Vec<NetifdDumpEntry>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct NetifdDumpEntry {
    #[serde(default)]
    pub interface: String,
    #[serde(default)]
    pub up: bool,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct InterfaceStatus {
    #[serde(default)]
    pub up: bool,
    #[serde(rename = "ipv4-address", default)]
    pub ipv4_address: Vec<Ipv4AddressEntry>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Ipv4AddressEntry {
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub mask: u8,
}

pub struct NetifdClient<'a> {
    ubus: &'a mut Ubus,
}

impl<'a> NetifdClient<'a> {
    pub fn new(ubus: &'a mut Ubus) -> Self {
        Self { ubus }
    }

    pub fn dump(&mut self) -> Result<NetifdDump, String> {
        self.ubus
            .call_empty::<NetifdDump>("network.interface", "dump", 3000)
            .map_err(|e| format!("netifd dump failed: {e}"))
    }

    pub fn status(&mut self, ifname: &str) -> Result<InterfaceStatus, String> {
        let object = format!("network.interface.{ifname}");
        self.ubus
            .call_empty::<InterfaceStatus>(&object, "status", 3000)
            .map_err(|e| format!("netifd status for {ifname} failed: {e}"))
    }

    pub fn reload(&mut self) -> Result<(), String> {
        let _: serde_json::Value = self
            .ubus
            .call_empty("network", "reload", 5000)
            .map_err(|e| format!("network reload failed: {e}"))?;
        Ok(())
    }
}

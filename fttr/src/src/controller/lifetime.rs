use crate::ipc::SubDevice;
use std::sync::{Arc, Mutex};
#[cfg(feature = "mqtt")]
use std::time::Duration;

/// Process-owned MQTT resources. Process termination reclaims upstream threads;
/// rumqttd does not expose a graceful shutdown handle.
pub(crate) struct Lifetime {
    #[cfg(feature = "mqtt")]
    broker: crate::broker::EmbeddedBroker,
    #[cfg(feature = "mqtt")]
    address: std::net::SocketAddr,
    #[cfg(all(feature = "device-scty", feature = "mqtt"))]
    boot_mock: Option<crate::devices::scty::ty_fs803g::boot_mock::BootMockServer>,
}

impl Lifetime {
    pub(crate) async fn start() -> Result<Self, String> {
        #[cfg(feature = "mqtt")]
        {
            let lan = std::env::var("FTTR_MQTT_LAN_ADDR")
                .map_err(|_| "FTTR_MQTT_LAN_ADDR is required for MQTT")?;
            let addresses = crate::broker::lan_addresses(&lan)?;
            let broker = crate::broker::EmbeddedBroker::start(&addresses).await?;
            println!("[+] Owned MQTT broker ready on {addresses:?}");
            #[cfg(all(feature = "device-scty", feature = "mqtt"))]
            let boot_mock = {
                let lan_addr = addresses
                    .iter()
                    .find(|addr| !addr.ip().is_loopback())
                    .copied()
                    .unwrap_or(addresses[0]);
                crate::devices::scty::ty_fs803g::boot_mock::start_from_env(lan_addr).await?
            };
            Ok(Self {
                broker,
                address: addresses[0],
                #[cfg(all(feature = "device-scty", feature = "mqtt"))]
                boot_mock,
            })
        }
        #[cfg(not(feature = "mqtt"))]
        {
            Ok(Self {})
        }
    }

    pub(crate) async fn wait(self, devices: Arc<Mutex<Vec<SubDevice>>>) -> Result<(), String> {
        #[cfg(feature = "mqtt")]
        {
            let mut broker = self.broker;
            #[cfg(all(feature = "device-scty", feature = "mqtt"))]
            let boot_mock = self.boot_mock;
            let telemetry = super::mqtt::run(self.address, devices);
            tokio::pin!(telemetry);
            loop {
                tokio::select! {
                    result = &mut telemetry => return Err(format!("telemetry worker stopped: {result:?}")),
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {
                        broker.check_health().await?;
                        #[cfg(all(feature = "device-scty", feature = "mqtt"))]
                        if boot_mock.as_ref().is_some_and(|server| server.is_finished()) {
                            return Err("SCTY IF5 Boot Mock stopped".into());
                        }
                    }
                }
            }
        }
        #[cfg(not(feature = "mqtt"))]
        {
            let _ = devices;
            std::future::pending().await
        }
    }
}

#[cfg(test)]
mod tests;

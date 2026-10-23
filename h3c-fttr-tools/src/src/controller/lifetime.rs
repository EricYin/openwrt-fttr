use crate::ipc::SubDevice;
use std::sync::{Arc, Mutex};
#[cfg(feature = "mqtt")]
use std::time::Duration;

/// Process-owned MQTT resources. Process termination reclaims upstream threads;
/// rumqttd does not expose a graceful shutdown handle.
pub(super) struct Lifetime {
    #[cfg(feature = "mqtt")]
    broker: crate::broker::EmbeddedBroker,
    #[cfg(feature = "mqtt")]
    address: std::net::SocketAddr,
}

impl Lifetime {
    pub(super) async fn start() -> Result<Self, String> {
        #[cfg(feature = "mqtt")]
        {
            let lan = std::env::var("FTTR_MQTT_LAN_ADDR")
                .map_err(|_| "FTTR_MQTT_LAN_ADDR is required for MQTT")?;
            let addresses = crate::broker::lan_addresses(&lan)?;
            let broker = crate::broker::EmbeddedBroker::start(&addresses).await?;
            println!("[+] Owned MQTT broker ready on {addresses:?}");
            Ok(Self {
                broker,
                address: addresses[0],
            })
        }
        #[cfg(not(feature = "mqtt"))]
        {
            Ok(Self {})
        }
    }

    pub(super) async fn wait(self, devices: Arc<Mutex<Vec<SubDevice>>>) -> Result<(), String> {
        #[cfg(feature = "mqtt")]
        {
            let mut broker = self.broker;
            let telemetry = super::mqtt::run(self.address, devices);
            tokio::pin!(telemetry);
            loop {
                tokio::select! {
                    result = &mut telemetry => return Err(format!("telemetry worker stopped: {result:?}")),
                    _ = tokio::time::sleep(Duration::from_secs(10)) => broker.check_health().await?,
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

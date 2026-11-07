use crate::ipc::SubDevice;
use std::sync::{Arc, Mutex};
#[cfg(feature = "mqtt")]
use std::time::Duration;

/// Process-owned MQTT resources. Process termination reclaims upstream threads;
/// rumqttd does not expose a graceful shutdown handle.
pub(super) struct Lifetime {
    #[cfg(feature = "mqtt")]
    broker: Option<crate::broker::EmbeddedBroker>,
    #[cfg(feature = "mqtt")]
    address: std::net::SocketAddr,
    #[cfg(feature = "mqtt")]
    settings: crate::mqtt_settings::Settings,
}

impl Lifetime {
    pub(super) async fn start() -> Result<Self, String> {
        #[cfg(feature = "mqtt")]
        {
            let settings = crate::mqtt_settings::Settings::load()?;
            if !settings.any() {
                return Ok(Self {
                    broker: None,
                    address: "127.0.0.1:1883".parse().unwrap(),
                    settings,
                });
            }
            let lan = std::env::var("FTTR_MQTT_LAN_ADDR")
                .map_err(|_| "FTTR_MQTT_LAN_ADDR is required for MQTT")?;
            let addresses =
                crate::broker::adapter_addresses(&lan, settings.hl202(), settings.ty_fs803g())?;
            let broker = crate::broker::EmbeddedBroker::start(&addresses).await?;
            println!("[+] Owned MQTT broker ready on {addresses:?}");
            Ok(Self {
                broker: Some(broker),
                address: addresses[0],
                settings,
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
            let Some(mut broker) = self.broker else {
                return std::future::pending().await;
            };
            let telemetry = async {
                #[cfg(feature = "mqtt-hl202")]
                if self.settings.hl202() {
                    return super::mqtt::run(self.address, devices).await;
                }
                #[cfg(not(feature = "mqtt-hl202"))]
                let _ = (self.address, devices);
                std::future::pending::<Result<(), String>>().await
            };
            let ty = async {
                let _ = self.settings;
                #[cfg(feature = "mqtt-ty-fs803g")]
                if self.settings.ty_fs803g() {
                    return crate::devices::ty_mqtt::run("127.0.0.1:11883".parse().unwrap()).await;
                }
                std::future::pending::<()>().await
            };
            tokio::pin!(telemetry);
            tokio::pin!(ty);
            loop {
                tokio::select! {
                    result = &mut telemetry => return Err(format!("telemetry worker stopped: {result:?}")),
                    _ = &mut ty => return Err("TY MQTT worker stopped".into()),
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

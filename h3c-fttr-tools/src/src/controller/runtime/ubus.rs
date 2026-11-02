//! Registrations driven by shared wire contracts; business owners stay in place.
use super::{OpticalClient, OpticalCommand, OpticalReplyExt};
use fttr_api::*;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::atomic::Ordering;

fn bind<Q: DeserializeOwned + 'static, R: Serialize + 'static>(
    method: &Method<Q, R>,
    handler: impl Fn(Q) -> Result<R, i32> + Send + Sync + 'static,
) -> (String, openwrt_ubus::MethodHandler) {
    (
        method.name.into(),
        Box::new(move |_, message| {
            let request =
                serde_json::from_value(message.cloned().unwrap_or_else(|| serde_json::json!({})))
                    .map_err(|_| 2)?;
            serde_json::to_value(handler(request)?).map_err(|_| 9)
        }),
    )
}

fn optical(optics: &OpticalClient, command: OpticalCommand) -> Result<OpticalReply, i32> {
    let response = match optics.request(command) {
        Ok(response) => response,
        Err(error) => OpticalReply::unavailable(
            command,
            optics.hardware_ready.load(Ordering::Acquire),
            error,
        ),
    };
    serde_json::from_value::<OpticalReply>(response).map_err(|_| 9)
}

#[derive(Clone)]
struct Handlers {
    optics: OpticalClient,
    devices: std::sync::Arc<std::sync::Mutex<Vec<crate::ipc::SubDevice>>>,
    engine: std::sync::Arc<std::sync::Mutex<crate::pon_engine::PonEngine>>,
    services: crate::controller::service::States,
}

impl Handlers {
    fn get_capabilities(&self, _: EmptyRequest) -> Result<Capabilities, i32> {
        Ok(crate::capabilities::compiled())
    }
    fn optical_status(&self, _: EmptyRequest) -> Result<OpticalReply, i32> {
        optical(&self.optics, OpticalCommand::Status)
    }
    fn optical_set(&self, request: OpticalSetRequest) -> Result<OpticalReply, i32> {
        optical(
            &self.optics,
            if request.enabled {
                OpticalCommand::On
            } else {
                OpticalCommand::Off
            },
        )
    }
    fn get_status(&self, _: EmptyRequest) -> Result<StatusReply, i32> {
        let provisioning = crate::provisioning::snapshot();
        let devices = self.devices.lock().map_err(|_| 9)?;
        let engine = self.engine.lock().map_err(|_| 9)?;
        let status =
            crate::controller::status::snapshot(&engine, &self.services, (&devices, provisioning));
        let mut value = serde_json::to_value(status).map_err(|_| 9)?;
        value["ipc"] =
            serde_json::to_value(super::ipc_server::metrics::snapshot()).map_err(|_| 9)?;
        serde_json::from_value(value).map_err(|_| 9)
    }
    fn get_sync(&self, _: EmptyRequest) -> Result<SyncStatusReply, i32> {
        serde_json::from_value(crate::provisioning::snapshot()).map_err(|_| 9)
    }
    fn config_check(&self, _: EmptyRequest) -> Result<ConfigCheckReply, i32> {
        serde_json::from_str(&crate::provisioning::control("{\"op\":\"check\"}")).map_err(|_| 9)
    }
    fn npu_status(&self, _: EmptyRequest) -> Result<NpuStatus, i32> {
        #[cfg(feature = "npu")]
        {
            Ok(crate::npu::status_from(
                std::path::Path::new("/etc/config/fttr"),
                std::path::Path::new("/sys/kernel/debug/ppe0/entries"),
                std::path::Path::new("/sys/module/h3c_fmcs/parameters"),
            ))
        }
        #[cfg(not(feature = "npu"))]
        {
            Err(4)
        }
    }
}

pub(super) fn methods(
    optics: OpticalClient,
    devices: std::sync::Arc<std::sync::Mutex<Vec<crate::ipc::SubDevice>>>,
    engine: std::sync::Arc<std::sync::Mutex<crate::pon_engine::PonEngine>>,
    services: crate::controller::service::States,
) -> Vec<(String, openwrt_ubus::MethodHandler)> {
    let handlers = Handlers {
        optics,
        devices,
        engine,
        services,
    };
    macro_rules! register {
        ($( $id:ident, $wire:ident, $sdk:ident, $req:ident, $res:ident, $access:ident, $availability:ident, [$($param:ident),*]; )*) => {{
            let mut methods = Vec::new();
            $(if $id.availability != Availability::Npu || cfg!(feature = "npu") {
                let handler = handlers.clone();
                methods.push(bind(&$id, move |request| handler.$wire(request)));
            })*
            // Preserve the CLI-compatible alias, using the same typed handler.
            let handler = handlers.clone();
            let (_, status) = bind(&GET_STATUS, move |request| handler.get_status(request));
            methods.push(("status".into(), status));
            methods
        }
        };
    }
    fttr_methods!(register)
}

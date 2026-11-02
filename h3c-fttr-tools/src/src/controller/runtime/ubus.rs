//! Typed registrations for the first shared contracts. Other methods stay in runtime.
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
struct Handlers(OpticalClient);

impl Handlers {
    fn get_capabilities(&self, _: EmptyRequest) -> Result<Capabilities, i32> {
        Ok(crate::capabilities::compiled())
    }
    fn optical_status(&self, _: EmptyRequest) -> Result<OpticalReply, i32> {
        optical(&self.0, OpticalCommand::Status)
    }
    fn optical_set(&self, request: OpticalSetRequest) -> Result<OpticalReply, i32> {
        optical(
            &self.0,
            if request.enabled {
                OpticalCommand::On
            } else {
                OpticalCommand::Off
            },
        )
    }
}

pub(super) fn methods(optics: OpticalClient) -> Vec<(String, openwrt_ubus::MethodHandler)> {
    let handlers = Handlers(optics);
    macro_rules! register {
        ($( $id:ident, $wire:ident, $sdk:ident, $req:ident, $res:ident, $access:ident, [$($param:ident),*]; )*) => {
            vec![$({ let handler = handlers.clone(); bind(&$id, move |request| handler.$wire(request)) }),*]
        };
    }
    fttr_methods!(register)
}

//! Platform-independent optical request queue and single-owner transition policy.
use crate::platform::context::PlatformContext;
use fttr_api::{OpticalAction, OpticalError, OpticalReply, OpticalState, ReplyStatus};
use serde_json::Value;
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(crate) enum OpticalCommand {
    Set(fttr_api::OpticalSettings),
    On,
    Off,
    Status,
    Reload,
    Reinitialize,
    Shutdown,
}
impl OpticalCommand {
    pub(crate) fn action(self) -> OpticalAction {
        match self {
            Self::Set(_) | Self::Reload => OpticalAction::OpticalReload,
            Self::On => OpticalAction::OpticalOn,
            Self::Off => OpticalAction::OpticalOff,
            Self::Status => OpticalAction::OpticalStatus,
            Self::Reinitialize => OpticalAction::OpticalReinitialize,
            Self::Shutdown => OpticalAction::OpticalShutdown,
        }
    }
}
pub(crate) trait OpticalReplyExt {
    fn value(self) -> Value;
    fn unavailable(command: OpticalCommand, ready: bool, error: io::Error) -> Value;
}
impl OpticalReplyExt for OpticalReply {
    fn value(self) -> Value {
        serde_json::to_value(self).expect("optical reply is serializable")
    }
    fn unavailable(command: OpticalCommand, ready: bool, error: io::Error) -> Value {
        Self {
            settings: None,
            pon_mac: None,
            schema_version: 1,
            status: ReplyStatus::Error,
            action: command.action(),
            desired_enabled: None,
            tx_enabled: None,
            actor_state: OpticalState::Unknown,
            hardware_ready: ready,
            discovery_active: false,
            cleanup_pending: false,
            error: Some(OpticalError {
                code: format!("{:?}", error.kind()),
                message: error.to_string(),
            }),
        }
        .value()
    }
}
pub(crate) struct OpticalRequest {
    pub command: OpticalCommand,
    pub reply: SyncSender<Value>,
    pub deadline: Instant,
}
#[derive(Clone)]
pub(crate) struct OpticalClient {
    pub platform: PlatformContext,
    pub requests: SyncSender<OpticalRequest>,
    pub hardware_ready: Arc<AtomicBool>,
    pub stopping: Arc<AtomicBool>,
}
impl OpticalClient {
    pub(crate) fn request(&self, command: OpticalCommand) -> io::Result<Value> {
        if matches!(command, OpticalCommand::Shutdown) {
            self.stopping.store(true, Ordering::Release);
        } else if self.stopping.load(Ordering::Acquire)
            && !matches!(command, OpticalCommand::Status)
        {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "controller is stopping",
            ));
        }
        if !self.hardware_ready.load(Ordering::Acquire)
            && !matches!(command, OpticalCommand::Shutdown)
        {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "hardware initialization incomplete",
            ));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.requests
            .try_send(OpticalRequest {
                command,
                reply,
                deadline: Instant::now() + Duration::from_secs(5),
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "optical request queue full")
                }
                mpsc::TrySendError::Disconnected(_) => {
                    io::Error::new(io::ErrorKind::NotConnected, "optical owner unavailable")
                }
            })?;
        response
            .recv_timeout(Duration::from_secs(6))
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{error}; optical operation outcome unknown; query status"),
                )
            })
    }
}
/// Owner policy hooks, distinct from the platform transport/resource contracts.
pub(crate) trait OpticalBackend {
    fn validate_calibration(&self) -> io::Result<()> {
        Ok(())
    }
    fn save_settings(&mut self, _settings: fttr_api::OpticalSettings) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "optical settings are not writable",
        ))
    }
    fn mac_tx_allowed(&self) -> bool {
        true
    }
    fn ready(&self) -> bool;
    fn cleanup_pending(&self) -> bool;
    fn quiesce(&mut self) -> io::Result<()>;
    fn set_tx(&mut self, enabled: bool) -> io::Result<()>;
    fn read_tx(&mut self) -> io::Result<bool>;
    fn desired(&self) -> io::Result<bool>;
    fn save(&mut self, enabled: bool) -> io::Result<()>;
}
pub(crate) struct OpticalActor {
    pub state: OpticalState,
}
impl OpticalActor {
    pub(crate) fn dispatch(
        &mut self,
        request: &OpticalRequest,
        backend: &mut impl OpticalBackend,
        stopping: &AtomicBool,
    ) -> Value {
        if Instant::now() > request.deadline {
            OpticalReply::unavailable(
                request.command,
                backend.ready(),
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "optical request expired before execution",
                ),
            )
        } else if stopping.load(Ordering::Acquire)
            && !matches!(
                request.command,
                OpticalCommand::Shutdown | OpticalCommand::Status
            )
        {
            OpticalReply::unavailable(
                request.command,
                backend.ready(),
                io::Error::new(io::ErrorKind::NotConnected, "controller is stopping"),
            )
        } else {
            self.handle(request.command, backend)
        }
    }
    pub(crate) fn handle(
        &mut self,
        command: OpticalCommand,
        backend: &mut impl OpticalBackend,
    ) -> Value {
        let mut tx_enabled = None;
        let mut quiesce_attempted = false;
        let desired = backend.desired();
        let mut desired_enabled = desired.as_ref().ok().copied();
        let result = if !backend.ready() && !matches!(command, OpticalCommand::Shutdown) {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "hardware initialization incomplete",
            ))
        } else {
            match command {
                OpticalCommand::Set(settings) => match backend.save_settings(settings) {
                    Ok(()) => return self.handle(OpticalCommand::Reload, backend),
                    Err(error) => {
                        let mut reply = self.handle(OpticalCommand::Status, backend);
                        reply["status"] = "error".into();
                        reply["error"] = serde_json::json!({"code":"Configuration", "message":error.to_string()});
                        return reply;
                    }
                },
                OpticalCommand::On
                | OpticalCommand::Off
                | OpticalCommand::Reload
                | OpticalCommand::Reinitialize
                | OpticalCommand::Shutdown => {
                    let target = match command {
                        OpticalCommand::On | OpticalCommand::Off => {
                            let enabled = matches!(command, OpticalCommand::On);
                            backend.save(enabled).map(|()| {
                                desired_enabled = Some(enabled);
                                enabled
                            })
                        }
                        OpticalCommand::Shutdown => Ok(false),
                        OpticalCommand::Reinitialize => backend.validate_calibration().and(desired),
                        _ => desired,
                    };
                    // Persist first; failures must not falsely report a saved setting.
                    if let Err(error) = target {
                        let mut reply = if matches!(
                            command,
                            OpticalCommand::Reload | OpticalCommand::Reinitialize
                        ) {
                            self.handle(OpticalCommand::Shutdown, backend)
                        } else {
                            self.handle(OpticalCommand::Status, backend)
                        };
                        reply["status"] = "error".into();
                        reply["action"] = serde_json::to_value(command.action()).unwrap();
                        reply["error"] = serde_json::json!({"code":"Configuration", "message":error.to_string()});
                        return reply;
                    }
                    let enabled = target.unwrap();
                    if matches!(command, OpticalCommand::Reload)
                        && self.state
                            == if enabled {
                                OpticalState::Enabled
                            } else {
                                OpticalState::Disabled
                            }
                        && backend.read_tx().ok() == Some(enabled)
                    {
                        let mut reply = self.handle(OpticalCommand::Status, backend);
                        reply["action"] = serde_json::to_value(command.action()).unwrap();
                        return reply;
                    }
                    let was_enabled = self.state != OpticalState::Disabled;
                    self.state = OpticalState::Unknown;
                    let quiesced = if enabled
                        && !was_enabled
                        && !matches!(command, OpticalCommand::Reinitialize)
                    {
                        Ok(())
                    } else {
                        quiesce_attempted = true;
                        backend.quiesce()
                    };
                    // A failed quiesce must still permit reset, never a fresh enable.
                    let target_tx = enabled && quiesced.is_ok();
                    let transition = backend.set_tx(target_tx).and_then(|()| {
                        let actual = backend.read_tx()?;
                        tx_enabled = Some(actual);
                        if actual == target_tx {
                            Ok(())
                        } else {
                            Err(io::Error::other("optical TX readback mismatch"))
                        }
                    });
                    let transition = transition.map_err(|error| {
                        if enabled {
                            let reset = backend.set_tx(false);
                            tx_enabled = backend.read_tx().ok();
                            if let Err(reset) = reset {
                                return io::Error::other(format!(
                                    "{error}; recovery reset failed: {reset}"
                                ));
                            }
                        }
                        if tx_enabled.is_none() {
                            tx_enabled = backend.read_tx().ok();
                        }
                        error
                    });
                    let result = match (quiesced, transition) {
                        (Ok(()), result) => result,
                        (Err(error), Ok(())) => Err(error),
                        (Err(quiesce), Err(transition)) => Err(io::Error::other(format!(
                            "quiesce failed: {quiesce}; optical transition failed: {transition}"
                        ))),
                    };
                    result.map(|()| {
                        self.state = if enabled {
                            OpticalState::Enabled
                        } else {
                            OpticalState::Disabled
                        };
                    })
                }
                OpticalCommand::Status => backend.read_tx().and_then(|actual| {
                    tx_enabled = Some(actual);
                    if self.state == OpticalState::Enabled && !actual {
                        self.state = OpticalState::Disabled;
                        quiesce_attempted = true;
                        backend.quiesce()?;
                    }
                    desired.map(|_| ())
                }),
            }
        };
        let result = match result {
            Err(error) => {
                self.state = OpticalState::Unknown;
                if backend.ready() && !quiesce_attempted {
                    match backend.quiesce() {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(io::Error::other(format!(
                            "{error}; quiesce failed: {cleanup}"
                        ))),
                    }
                } else {
                    Err(error)
                }
            }
            result => result,
        };
        OpticalReply {
            settings: Some(crate::optical::settings()),
            pon_mac: Some(super::pon_mac::snapshot()),
            schema_version: 1,
            status: if result.is_ok() {
                ReplyStatus::Success
            } else {
                ReplyStatus::Error
            },
            action: command.action(),
            desired_enabled,
            tx_enabled,
            actor_state: self.state,
            hardware_ready: backend.ready(),
            discovery_active: backend.ready()
                && backend.mac_tx_allowed()
                && self.state == OpticalState::Enabled
                && !backend.cleanup_pending(),
            cleanup_pending: backend.cleanup_pending(),
            error: result.err().map(|error| OpticalError {
                code: format!("{:?}", error.kind()),
                message: error.to_string(),
            }),
        }
        .value()
    }
}

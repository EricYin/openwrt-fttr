//! Registrations driven by shared wire contracts; business owners stay in place.
use super::{OpticalClient, OpticalCommand, OpticalReplyExt};
use fttr_api::*;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::atomic::Ordering;
mod metrics;

fn bind<Q: DeserializeOwned + 'static, R: Serialize + 'static>(
    method: &Method<Q, R>,
    handler: impl Fn(Q) -> Result<R, i32> + Send + Sync + 'static,
) -> (String, openwrt_ubus::MethodHandler) {
    let name = method.name;
    (
        method.name.into(),
        Box::new(move |_, message| {
            let started = std::time::Instant::now();
            let result = (|| {
                let request = serde_json::from_value(
                    message.cloned().unwrap_or_else(|| serde_json::json!({})),
                )
                .map_err(|_| 2)?;
                serde_json::to_value(handler(request)?).map_err(|_| 9)
            })();
            metrics::record(name, started.elapsed(), result.is_err());
            result
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
    jobs: fttr_jobs::Jobs,
}

impl Handlers {
    fn npu_set(&self, request: NpuSetRequest) -> Result<NpuSetReply, i32> {
        #[cfg(feature = "npu")]
        {
            response(
                match crate::npu::settings::set(
                    request.enabled,
                    (!request.bridge.is_empty()).then_some(request.bridge.as_str()),
                ) {
                    Ok(()) => serde_json::json!({"status":"success","enabled":request.enabled}),
                    Err(error) => serde_json::json!({"status":"error","error":error.to_string()}),
                },
            )
        }
        #[cfg(not(feature = "npu"))]
        {
            let _ = request;
            Err(4)
        }
    }
    fn mib_start(&self, request: MibStartRequest) -> Result<JobStartReply, i32> {
        use crate::pon_engine::ploam::OnuState;
        if request.onu_id >= crate::pon_engine::ONU_SLOTS {
            return Err(2);
        }
        let engine = self.engine.try_lock().map_err(|_| 9)?;
        if !engine.active_onus.iter().any(|onu| {
            onu.onu_id == request.onu_id
                && onu.full_sn_str() == request.optical_sn
                && onu.state == OnuState::O5Operation
        }) {
            return response(fttr_jobs::error(
                "Device registration changed; reopen device details",
            ));
        }
        let generation = self
            .services
            .try_lock()
            .map_err(|_| 9)?
            .get(&request.onu_id)
            .map(|s| s.generation)
            .ok_or(9)?;
        drop(engine);
        let handler = self.clone();
        let guard: crate::controller::inventory::RegistrationGuard =
            std::sync::Arc::new(move || {
                let Ok(engine) = handler.engine.lock() else {
                    return false;
                };
                let Ok(services) = handler.services.lock() else {
                    return false;
                };
                engine.active_onus.iter().any(|onu| {
                    onu.onu_id == request.onu_id
                        && onu.full_sn_str() == request.optical_sn
                        && onu.state == OnuState::O5Operation
                }) && services
                    .get(&request.onu_id)
                    .is_some_and(|s| s.generation == generation)
            });
        let states = self.services.clone();
        let id = request.onu_id;
        let tables = request.tables;
        response(self.jobs.start_with_progress("mib", move |progress| {
            progress.update(fttr_jobs::Phase::Starting, 0, None);
            match crate::controller::inventory::control_guarded(
                id,
                tables,
                Some(crate::controller::inventory::MibProgressReporter::Job(
                    progress,
                )),
                &states,
                Some(guard.clone()),
            ) {
                Ok(report) if guard() => serde_json::json!({"status":"ready","report":report}),
                Ok(_) => fttr_jobs::error("Device registration changed during MIB read"),
                Err(error) => fttr_jobs::error(&error.to_string()),
            }
        }))
    }
    fn mib_result(&self, request: JobRequest) -> Result<JobChunkReply, i32> {
        response(self.jobs.result(&request.job, request.chunk, Some("mib")))
    }
    fn job_result(&self, request: JobRequest) -> Result<JobChunkReply, i32> {
        response(self.jobs.result(&request.job, request.chunk, None))
    }
    fn job_release(&self, request: JobReleaseRequest) -> Result<JobReleaseReply, i32> {
        response(self.jobs.release(&request.job))
    }
    fn hl_query(
        &self,
        value: serde_json::Value,
        background: bool,
    ) -> Result<serde_json::Value, i32> {
        #[cfg(feature = "device-hl202")]
        {
            let input = value.to_string();
            if input.len() > crate::hl202_ipc::MAX_REQUEST_BYTES
                || serde_json::from_str::<crate::devices::hl202::api::Query>(&input).is_err()
            {
                return Err(2);
            }
            if !background {
                return Ok(crate::controller::hl202::control(
                    &input,
                    &self.engine,
                    &self.services,
                ));
            }
            let handler = self.clone();
            Ok(self.jobs.start("hl202", move || {
                crate::controller::hl202::control(&input, &handler.engine, &handler.services)
            }))
        }
        #[cfg(not(feature = "device-hl202"))]
        {
            let _ = (value, background);
            Err(4)
        }
    }
    fn hl202_list(&self, _: EmptyRequest) -> Result<HlListReply, i32> {
        response(self.hl_query(serde_json::json!({"method":"list"}), false)?)
    }
    fn hl202_report(&self, request: HlReportRequest) -> Result<JobStartReply, i32> {
        let source = matches!(&request.target, HlTarget::Source { .. });
        response(self.hl_query(serde_json::json!({"method":if source {"source_report"} else {"report"},"target":target(request.target)}), true)?)
    }
    fn hl202_accounts_start(&self, request: HlAccountsRequest) -> Result<JobStartReply, i32> {
        self.hl_run(request.target, serde_json::json!({"kind":"accounts_read"}))
    }
    fn hl202_account_update(&self, request: HlUpdateRequest) -> Result<JobStartReply, i32> {
        let action = match request.kind {
            AccountChange::AccountSetPassword => {
                serde_json::json!({"kind":"account_set_password","entry":request.entry,"password":request.password})
            }
            AccountChange::AccountSetActive => {
                serde_json::json!({"kind":"account_set_active","entry":request.entry,"active":request.active})
            }
        };
        self.hl_run(request.target, action)
    }
    fn hl202_exec_start(&self, request: HlExecRequest) -> Result<JobStartReply, i32> {
        self.hl_run(request.target, serde_json::json!({"kind":"exec","script":request.script,"timeout_seconds":request.timeout_seconds}))
    }
    fn hl_run(&self, selection: HlTarget, action: serde_json::Value) -> Result<JobStartReply, i32> {
        if matches!(selection, HlTarget::Source { .. }) {
            return Err(2);
        }
        response(self.hl_query(serde_json::json!({"method":"run","request":{"schema_version":1,"target":target(selection),"action":action}}), true)?)
    }
    fn get_calibration(&self, _: EmptyRequest) -> Result<CalibrationInfoReply, i32> {
        response(super::calibration::get().unwrap_or_else(|e| fttr_jobs::error(&e.to_string())))
    }
    fn save_calibration(
        &self,
        request: CalibrationSaveRequest,
    ) -> Result<CalibrationSaveReply, i32> {
        response(
            super::calibration::save(&request.data)
                .unwrap_or_else(|e| fttr_jobs::error(&e.to_string())),
        )
    }
    fn apply_calibration(&self, _: EmptyRequest) -> Result<JobStartReply, i32> {
        let optics = self.optics.clone();
        response(self.jobs.start("calibration", move || {
            let result = match optical(&optics, OpticalCommand::Reinitialize) {
                Ok(reply) if reply.status == ReplyStatus::Success => CalibrationApplied {
                    success: true,
                    output: Some(
                        if reply.desired_enabled == Some(true) {
                            "Calibration applied by optical owner"
                        } else {
                            "Optical TX remains disabled; calibration will load on next enable"
                        }
                        .into(),
                    ),
                    error: None,
                },
                Ok(reply) => CalibrationApplied {
                    success: false,
                    output: None,
                    error: Some(
                        reply
                            .error
                            .map(|e| e.message)
                            .unwrap_or_else(|| "Calibration failed".into()),
                    ),
                },
                Err(_) => CalibrationApplied {
                    success: false,
                    output: None,
                    error: Some(
                        "Optical owner unavailable; execution outcome may be unknown".into(),
                    ),
                },
            };
            serde_json::to_value(result).expect("calibration result is JSON serializable")
        }))
    }
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
        if let Some(jobs) = self.jobs.snapshot() {
            value["jobs"] = jobs;
        }
        if let Some(stats) = metrics::snapshot() {
            value["ubus"] = serde_json::to_value(stats).map_err(|_| 9)?;
        }
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
    fn config_reload(&self, _: EmptyRequest) -> Result<ConfigReloadReply, i32> {
        serde_json::from_str(&crate::provisioning::control("{\"op\":\"reload\"}")).map_err(|_| 9)
    }
    fn sync(&self, request: SyncRequest) -> Result<SyncReply, i32> {
        let payload = request.provisioning_request().map_err(|_| 2)?;
        serde_json::from_str(&crate::provisioning::control(&payload.to_string())).map_err(|_| 9)
    }
    fn restart_service(&self, _: EmptyRequest) -> Result<RestartReply, i32> {
        std::thread::Builder::new()
            .name("service-restart".into())
            .spawn(|| {
                // Let ubus deliver admission before the service exits. Never retry a write.
                std::thread::sleep(std::time::Duration::from_millis(500));
                // procd owns the helper: descendants of fttrd are killed with its
                // service cgroup, even if they use a separate process group.
                let result = openwrt_ubus::Ubus::connect(None).and_then(|mut bus| {
                    bus.call(
                        "service",
                        "event",
                        Some(&serde_json::json!({"type": "fttr.restart", "data": {}})),
                        5000,
                    )
                });
                if let Err(error) = result {
                    eprintln!("[ubus] service restart request failed: {error}");
                }
            })
            .map_err(|_| 9)?;
        Ok(RestartReply { restarting: true })
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
    stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Vec<(String, openwrt_ubus::MethodHandler)> {
    let handlers = Handlers {
        optics,
        devices,
        engine,
        services,
        jobs: {
            static JOBS: std::sync::OnceLock<fttr_jobs::Jobs> = std::sync::OnceLock::new();
            JOBS.get_or_init(|| fttr_jobs::Jobs::with_shutdown(stopping))
                .clone()
        },
    };
    macro_rules! register {
        ($( $id:ident, $wire:ident, $sdk:ident, $req:ident, $res:ident, $access:ident, $availability:ident, [$($param:ident),*]; )*) => {{
            let mut methods = Vec::new();
            $(if match $id.availability { Availability::Always => true, Availability::Npu => cfg!(feature = "npu"), Availability::Hl202 => cfg!(feature = "device-hl202") } {
                let handler = handlers.clone();
                methods.push(bind(&$id, move |request| handler.$wire(request)));
            })*
            methods
        }
        };
    }
    fttr_methods!(register)
}

fn target(selection: HlTarget) -> serde_json::Value {
    match selection {
        HlTarget::Optical {
            optical_sn,
            onu_id,
            binding_generation,
        } => {
            serde_json::json!({"optical_sn":optical_sn,"onu_id":onu_id,"binding_generation":binding_generation})
        }
        HlTarget::Source {
            mqtt_mac,
            binding_generation,
        } => serde_json::json!({"mqtt_mac":mqtt_mac,"binding_generation":binding_generation}),
    }
}

fn response<R: DeserializeOwned>(value: serde_json::Value) -> Result<R, i32> {
    serde_json::from_value(value).map_err(|_| 9)
}

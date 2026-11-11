//! Desired bits are reconciled only by the PLOAM hardware owner.
use std::sync::{Mutex, OnceLock};

pub(crate) fn apply<T>(
    desired: (bool, bool),
    quiesce: impl FnOnce() -> std::io::Result<()>,
    write: impl FnOnce(bool, bool) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let cleanup = if !desired.0 { quiesce() } else { Ok(()) };
    // Failure to drain workers must not prevent disabling transmission.
    let written = write(desired.0, desired.1);
    match (cleanup, written) {
        (Ok(()), result) => result,
        (Err(error), Ok(_)) => Err(error),
        (Err(cleanup), Err(write)) => Err(std::io::Error::other(format!(
            "cleanup: {cleanup}; GPON control: {write}"
        ))),
    }
}

fn cache() -> &'static Mutex<Option<fttr_api::PonMacStatus>> {
    static CACHE: OnceLock<Mutex<Option<fttr_api::PonMacStatus>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn desired() -> (bool, bool) {
    let settings = crate::optical::settings();
    (
        settings.enabled && settings.downstream_tx_enabled,
        settings.enabled && settings.fec_enabled,
    )
}

pub(crate) fn record(result: std::io::Result<crate::platform::resources::PonMacReadback>) -> bool {
    let desired = desired();
    let (readback, error) = match result {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let allowed = readback
        .as_ref()
        .is_some_and(|value| value.tx_enabled == Some(true));
    *cache().lock().unwrap() = Some(fttr_api::PonMacStatus {
        desired_tx_enabled: desired.0,
        desired_fec_enabled: desired.1,
        register: readback.as_ref().and_then(|value| value.raw),
        tx_enabled: readback.as_ref().and_then(|value| value.tx_enabled),
        fec_enabled: readback.as_ref().and_then(|value| value.fec_enabled),
        error,
    });
    allowed
}

pub(crate) fn snapshot() -> fttr_api::PonMacStatus {
    let desired = desired();
    let mut status = cache()
        .lock()
        .unwrap()
        .clone()
        .unwrap_or(fttr_api::PonMacStatus {
            desired_tx_enabled: desired.0,
            desired_fec_enabled: desired.1,
            register: None,
            tx_enabled: None,
            fec_enabled: None,
            error: Some("awaiting_hardware_owner".into()),
        });
    status.desired_tx_enabled = desired.0;
    status.desired_fec_enabled = desired.1;
    status
}

#[cfg(test)]
mod tests {
    use super::apply;
    use std::cell::RefCell;

    #[test]
    fn disable_invalidates_workers_before_write_even_when_cleanup_fails() {
        for failed in [false, true] {
            let events = RefCell::new(Vec::new());
            let result = apply(
                (false, true),
                || {
                    events.borrow_mut().push("quiesce");
                    if failed {
                        Err(std::io::Error::other("carrier"))
                    } else {
                        Ok(())
                    }
                },
                |tx, fec| {
                    assert!(!tx);
                    assert!(fec);
                    events.borrow_mut().push("write");
                    Ok(8)
                },
            );
            assert_eq!(*events.borrow(), ["quiesce", "write"]);
            assert_eq!(result.is_err(), failed);
        }
    }

    #[test]
    fn fec_changes_with_transmission_enabled_do_not_drop_registrations() {
        for fec in [false, true] {
            assert!(apply(
                (true, fec),
                || panic!("must not quiesce active transmission"),
                |tx, actual_fec| {
                    assert!(tx);
                    assert_eq!(actual_fec, fec);
                    Ok(if fec { 10 } else { 2 })
                }
            )
            .is_ok());
        }
    }
}

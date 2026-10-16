use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static ACCEPTED: AtomicU64 = AtomicU64::new(0);
static REJECTED: AtomicU64 = AtomicU64::new(0);
static READ_ERRORS: AtomicU64 = AtomicU64::new(0);
static HANDLED: AtomicU64 = AtomicU64::new(0);
static MAX_INTAKE_US: AtomicU64 = AtomicU64::new(0);
static MAX_HANDLER_US: AtomicU64 = AtomicU64::new(0);

#[derive(serde::Serialize)]
pub(in crate::controller::runtime) struct Snapshot {
    accepted: u64,
    rejected: u64,
    read_errors: u64,
    handled: u64,
    max_intake_us: u64,
    max_handler_us: u64,
}

pub(super) fn accepted() {
    ACCEPTED.fetch_add(1, Ordering::Relaxed);
}
pub(super) fn rejected() {
    REJECTED.fetch_add(1, Ordering::Relaxed);
}
pub(super) fn read_error() {
    READ_ERRORS.fetch_add(1, Ordering::Relaxed);
}
pub(super) fn intake(elapsed: Duration) {
    MAX_INTAKE_US.fetch_max(
        u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}
pub(super) fn handled(elapsed: Duration) {
    HANDLED.fetch_add(1, Ordering::Relaxed);
    MAX_HANDLER_US.fetch_max(
        u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

pub(in crate::controller::runtime) fn snapshot() -> Snapshot {
    Snapshot {
        accepted: ACCEPTED.load(Ordering::Relaxed),
        rejected: REJECTED.load(Ordering::Relaxed),
        read_errors: READ_ERRORS.load(Ordering::Relaxed),
        handled: HANDLED.load(Ordering::Relaxed),
        max_intake_us: MAX_INTAKE_US.load(Ordering::Relaxed),
        max_handler_us: MAX_HANDLER_US.load(Ordering::Relaxed),
    }
}

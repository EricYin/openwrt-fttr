//! SCTY dispatch; only the observed TY-FS803G model is enrolled.
//! SCTY dispatch; only the observed TY-FS803G model is enrolled.
#[cfg(feature = "mqtt")]
pub(crate) mod ty_fs803g;
#[cfg(not(feature = "mqtt"))]
mod ty_fs803g;

pub(super) fn matches(vendor: &str, equipment: Option<&str>) -> bool {
    vendor == "SCTY" && ty_fs803g::matches(equipment)
}

pub(super) fn configure(
    platform: crate::platform::context::PlatformContext,
    id: u8,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<()> {
    ty_fs803g::configure(platform, id, cancellation)
}

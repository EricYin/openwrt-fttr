#[cfg(feature = "device-hwtc-k155")]
fn main() -> std::io::Result<()> {
    let id = std::env::args().nth(1).and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "usage: fttr-k155-service ONU_ID"))?;
    h3c_fttr_tools::devices::hwtc_k155::service::configure(id, None)
}
#[cfg(not(feature = "device-hwtc-k155"))]
fn main() -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "K155 adapter not compiled"))
}

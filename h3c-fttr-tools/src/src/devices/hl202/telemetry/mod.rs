mod archive;
mod decode;
mod model;
pub mod store;
mod worker;
pub use model::{Field, Member, Report};

pub fn parse(input: &[u8]) -> std::io::Result<Report> {
    let (members, decompressed_bytes) = archive::read(input)?;
    let report = decode::report(members, input.len(), decompressed_bytes);
    check_json_limit(&report)?;
    Ok(report)
}

fn check_json_limit(report: &Report) -> std::io::Result<()> {
    struct ByteCount(usize);
    impl std::io::Write for ByteCount {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoded = ByteCount(0);
    serde_json::to_writer(&mut encoded, report).map_err(std::io::Error::other)?;
    if encoded.0 > 4 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "telemetry JSON limit exceeded",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

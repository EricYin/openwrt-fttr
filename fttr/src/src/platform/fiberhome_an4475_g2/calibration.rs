//! Validation of the stock 2 KiB optical table, not an optical programming recipe.
use std::{fmt, io};

pub const TABLE_BYTES: usize = 0x800;
pub const HEADER_BYTES: usize = 0x80;
pub const PAYLOAD_BYTES: usize = TABLE_BYTES - HEADER_BYTES;

/// Borrows private calibration bytes without logging them. CRC establishes file
/// integrity only; the caller must establish board, optical chip and provenance.
pub struct CalibrationTable<'a> {
    bytes: &'a [u8],
}

impl<'a> CalibrationTable<'a> {
    pub fn parse(bytes: &'a [u8]) -> io::Result<Self> {
        if bytes.len() != TABLE_BYTES {
            return Err(invalid("unsupported optical table length"));
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if word(0x78) != HEADER_BYTES as u32 || word(0x70) != PAYLOAD_BYTES as u32 {
            return Err(invalid("optical table header/payload size mismatch"));
        }
        if !valid_crc(&bytes[..0x7c], word(0x7c)) {
            return Err(invalid("optical table header CRC mismatch"));
        }
        if !valid_crc(&bytes[HEADER_BYTES..], word(0x74)) {
            return Err(invalid("optical table payload CRC mismatch"));
        }
        Ok(Self { bytes })
    }

    /// Raw stock header discriminator. This is not a live chip identification.
    pub fn chip_id(&self) -> u8 {
        self.bytes[0x30]
    }

    pub fn require_chip(&self, independently_identified_chip: u8) -> io::Result<()> {
        if self.chip_id() != independently_identified_chip {
            return Err(invalid("optical table/chip discriminator mismatch"));
        }
        Ok(())
    }

    /// Opaque data for a future chip-specific consumer; not a register dump.
    pub fn payload(&self) -> &'a [u8] {
        &self.bytes[HEADER_BYTES..]
    }
}

impl fmt::Debug for CalibrationTable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CalibrationTable")
            .field("chip_id", &self.chip_id())
            .field("payload_bytes", &PAYLOAD_BYTES)
            .finish_non_exhaustive()
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_crc(bytes: &[u8], expected: u32) -> bool {
    // The original validator accepts these seeds independently for each region.
    crc32(bytes, 0) == expected || crc32(bytes, u32::MAX) == expected
}

fn crc32(bytes: &[u8], seed: u32) -> u32 {
    let mut crc = !seed;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests;

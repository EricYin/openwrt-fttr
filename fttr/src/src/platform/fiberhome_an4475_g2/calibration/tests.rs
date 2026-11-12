use super::*;

// Synthetic pattern and CRCs produced independently with Python zlib.crc32;
// no firmware or unit calibration bytes are embedded in these fixtures.
fn fixture(alternate_seed: bool) -> Vec<u8> {
    let (data_crc, header_crc): (u32, u32) = if alternate_seed {
        (0x46c8_20e4, 0xe345_c18d)
    } else {
        (0x9380_eb30, 0xe73e_fbc1)
    };
    let mut bytes = vec![0; TABLE_BYTES];
    bytes[0x30] = 0x26;
    bytes[0x70..0x74].copy_from_slice(&(PAYLOAD_BYTES as u32).to_le_bytes());
    bytes[0x74..0x78].copy_from_slice(&data_crc.to_le_bytes());
    bytes[0x78..0x7c].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
    bytes[0x7c..0x80].copy_from_slice(&header_crc.to_le_bytes());
    for (index, byte) in bytes[HEADER_BYTES..].iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    bytes
}

#[test]
fn independently_generated_tables_and_chip_check() {
    for alternate in [false, true] {
        let bytes = fixture(alternate);
        let table = CalibrationTable::parse(&bytes).unwrap();
        assert_eq!(table.chip_id(), 0x26);
        table.require_chip(0x26).unwrap();
        assert_eq!(
            table.require_chip(0x27).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(table.payload(), &bytes[HEADER_BYTES..]);
        assert_eq!(table.payload().as_ptr(), bytes[HEADER_BYTES..].as_ptr());
    }
}

#[test]
fn header_and_payload_seed_choices_are_independent() {
    for (alternate_payload, header_crc) in [(true, 0x096d_307eu32), (false, 0x0d16_0a32)] {
        let mut bytes = fixture(alternate_payload);
        bytes[0x7c..0x80].copy_from_slice(&header_crc.to_le_bytes());
        CalibrationTable::parse(&bytes).unwrap();
    }
}

#[test]
fn crc_has_independent_known_answers_and_seed_semantics() {
    assert_eq!(crc32(b"123456789", 0), 0xcbf4_3926);
    assert_eq!(crc32(b"123456789", u32::MAX), 0xd202_d277);
    assert_eq!(crc32(b"", 0), 0);
    assert_eq!(crc32(b"", u32::MAX), u32::MAX);
}

#[test]
fn rejects_truncation_extra_bytes_and_unsupported_legacy_table() {
    let bytes = fixture(false);
    for len in [0, 0x7f, 0x80, 0x400, TABLE_BYTES - 1] {
        assert_eq!(
            CalibrationTable::parse(&bytes[..len]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    let mut extra = bytes;
    extra.push(0);
    assert!(CalibrationTable::parse(&extra).is_err());
}

#[test]
fn rejects_corrupt_header_sizes_crc_and_payload_without_returning_data() {
    for index in [
        0,
        0x30,
        0x6f,
        0x70,
        0x73,
        0x74,
        0x77,
        0x78,
        0x7b,
        0x7c,
        0x7f,
        0x80,
        0x400,
        TABLE_BYTES - 1,
    ] {
        let mut bytes = fixture(false);
        bytes[index] ^= 1;
        let error = CalibrationTable::parse(&bytes).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "index {index:#x}");
        assert!(error.to_string().starts_with("optical table"));
    }
    let bytes = fixture(false);
    assert_eq!(
        format!("{:?}", CalibrationTable::parse(&bytes).unwrap()),
        "CalibrationTable { chip_id: 38, payload_bytes: 1920, .. }"
    );
}

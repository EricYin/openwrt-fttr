// Copyright (c) 2018-present Boling Consulting Solutions (bcsw.net)
// Copyright 2020-present Open Networking Foundation
// SPDX-License-Identifier: Apache-2.0
// Modified: selected upstream fixture/assertions translated to Rust; project tests added.
//! Independent fixtures: definitions are never used to generate expected bytes.
use super::{
    attributes::{AttributeCodec, AttributeContext, AttributeValue},
    error::OmciResultError,
    messages::GetResponse,
    mib::{MibAssembler, MibRecord},
    schema::MeRegistry,
    session::{OmciSession, OmciTransport},
    tables::{TableAssembler, VlanRule},
    wire::{unhex, BaselineFrame},
};
use std::{
    collections::VecDeque,
    io,
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};

#[test]
fn captured_gpon_crc_and_strict_frame_validation() {
    let bytes = unhex("1234490a010000008000000000000000000000000000000000000000000000000000000000000000000000287edf0a44").unwrap();
    let frame = BaselineFrame::from_bytes(&bytes).unwrap();
    assert_eq!(frame.to_bytes().as_slice(), bytes);
    assert_eq!(frame.class_id, 256);
    let mut corrupt = bytes.clone();
    corrupt[10] ^= 1;
    assert!(BaselineFrame::from_bytes(&corrupt).is_err());
    assert!(BaselineFrame::from_bytes(&bytes[..47]).is_err());
}

/// OMCI-003 fixture, Apache-2.0; translated assertion subset, not generated from Rust schema.
/// https://github.com/opencord/omci-lib-go/blob/v2.2.4/mibupload_test.go#L486-L542
#[test]
fn upstream_priority_queue_upload_fixture() {
    let bytes = unhex(
        "02862e0a0002000001150000fff0000000000000000000010100000000010000000000000000000000000028",
    )
    .unwrap();
    let record = MibRecord::decode(0, &bytes[8..40]).unwrap();
    assert_eq!(
        (record.class_id, record.entity_id, record.attribute_mask),
        (277, 0, 0xfff0)
    );
    let mut mib = MibAssembler::new(1);
    mib.push(record);
    let report = mib.finish();
    assert!(report.uploaded_complete);
    assert_eq!(report.decoded_records, 1);
    let attrs = &report.entities[0].attributes;
    assert_eq!(attrs.len(), 12);
    for attr in attrs {
        let expected = match attr.definition.name {
            "RelatedPort" => 16842752,
            "Weight" => 1,
            _ => 0,
        };
        assert_eq!(
            attr.value.as_ref().unwrap().unsigned(),
            Some(expected),
            "{}",
            attr.definition.name
        );
    }
    assert!(serde_json::to_string(&report)
        .unwrap()
        .contains("omci-lib-go/v2.2.4"));
}

#[test]
fn necessary_masks_widths_and_access_match_g988() {
    let tcont = MeRegistry::get(262).unwrap();
    let (mask, bytes) = AttributeCodec::encode(
        tcont,
        &[(1, AttributeValue::number(0x123))],
        AttributeContext::SetRequest,
    )
    .unwrap();
    assert_eq!((mask, bytes), (0x8000, vec![1, 0x23]));
    let (_, create) = AttributeCodec::encode(
        MeRegistry::get(272).unwrap(),
        &[(1, AttributeValue::number(48))],
        AttributeContext::CreateRequest,
    )
    .unwrap();
    assert_eq!(create, vec![0, 48]);
    assert!(AttributeCodec::encode(
        tcont,
        &[(1, AttributeValue::number(65536))],
        AttributeContext::SetRequest
    )
    .is_err());
    assert!(AttributeCodec::encode(
        tcont,
        &[(2, AttributeValue::number(0))],
        AttributeContext::SetRequest
    )
    .is_err());
    assert_eq!(
        MeRegistry::get(11)
            .unwrap()
            .named("AdministrativeState")
            .unwrap()
            .mask,
        0x0800
    );
    assert_eq!(
        MeRegistry::get(256).unwrap().named("Version").unwrap().mask,
        0x4000
    );
    let block =
        AttributeCodec::decode(tcont, 0xe000, &[1, 0x23, 0, 2], AttributeContext::MibUpload);
    assert_eq!(block.consumed, 4);
    assert_eq!(block.undecoded_mask, 0);
    assert_eq!(block.attributes[2].value.unsigned(), Some(2));
    let unknown = AttributeCodec::decode(
        tcont,
        0xf000,
        &[1, 0x23, 0, 2, 0xaa],
        AttributeContext::MibUpload,
    );
    assert_eq!(unknown.undecoded_mask, 0x1000);
    assert_eq!(unknown.remaining_hex, "aa");
    let truncated = AttributeCodec::decode(tcont, 0x8000, &[1], AttributeContext::MibUpload);
    assert_eq!(truncated.consumed, 0);
    assert_eq!(truncated.diagnostics[0].code, "truncated_attribute");
}

#[test]
fn strings_signed_optics_and_table_context_preserve_raw() {
    let mut bytes = [0xff; 14];
    bytes[..5].copy_from_slice(b"K155 ");
    let block = AttributeCodec::decode(
        MeRegistry::get(256).unwrap(),
        0x4000,
        &bytes,
        AttributeContext::GetResponse,
    );
    assert_eq!(block.attributes[0].value.text(), Some("K155"));
    assert!(block.attributes[0].raw_hex.ends_with("ffff"));
    let optics = AttributeCodec::decode(
        MeRegistry::get(263).unwrap(),
        0x0040,
        &[0xff, 0x9c],
        AttributeContext::MibUpload,
    );
    assert_eq!(
        optics.attributes[0].value,
        AttributeValue::Signed { value: -100 }
    );
    let def = MeRegistry::get(171).unwrap();
    let table = AttributeCodec::decode(def, 0x0400, &[0, 0, 0, 48], AttributeContext::GetResponse);
    assert_eq!(
        table.attributes[0].value,
        AttributeValue::TableLength { bytes: 48 }
    );
    let upload = AttributeCodec::decode(def, 0x0400, &[0; 26], AttributeContext::MibUpload);
    assert_eq!(upload.undecoded_mask, 0x0400);
}

#[test]
fn conflicts_unknown_classes_and_missing_sequences_are_explicit() {
    let mut mib = MibAssembler::new(3);
    for (seq, value) in [(0, 1), (1, 2)] {
        let mut payload = [0; 32];
        payload[..6].copy_from_slice(&[1, 6, 0, 0, 0x80, 0]);
        payload[7] = value;
        mib.push(MibRecord::decode(seq, &payload).unwrap());
    }
    let mut vendor = [0xab; 32];
    vendor[..6].copy_from_slice(&[0xff, 0x43, 0, 1, 0x80, 0]);
    mib.push(MibRecord::decode(1, &vendor).unwrap());
    let report = mib.finish();
    assert!(!report.uploaded_complete);
    assert_eq!(report.basic_records, 1);
    assert!(report.entities[0].attributes[0].value.is_none());
    assert_eq!(report.entities[0].attributes[0].observations.len(), 2);
    assert_eq!(report.records[2].raw_hex, "ab".repeat(26));
}

#[test]
fn get_error_masks_are_not_success_padding() {
    let mut p = [0; 32];
    p[0] = 9;
    p[28..].copy_from_slice(&[0x20, 0, 0x10, 0]);
    let frame = BaselineFrame::new(1, 0x29, 268, 1, p);
    let error = GetResponse::decode(&frame, 0xe000).err().unwrap();
    let result = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OmciResultError>()
        .unwrap();
    assert_eq!(
        (result.result, result.unsupported_mask, result.failed_mask),
        (9, 0x2000, 0x1000)
    );
    let mut p = [0; 32];
    p[..5].copy_from_slice(&[9, 0x20, 0, 0x10, 0]);
    let error = super::messages::check_result(&BaselineFrame::new(1, 0x28, 268, 1, p)).unwrap_err();
    let result = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OmciResultError>()
        .unwrap();
    assert_eq!(
        (result.unsupported_mask, result.failed_mask),
        (0x2000, 0x1000)
    );
    p[..3].copy_from_slice(&[3, 0x80, 0]);
    let error = super::messages::check_result(&BaselineFrame::new(1, 0x24, 268, 1, p)).unwrap_err();
    assert_eq!(
        error
            .get_ref()
            .unwrap()
            .downcast_ref::<OmciResultError>()
            .unwrap()
            .execution_mask,
        0x8000
    );
}

#[test]
fn table_segments_cross_rows_and_trim_padding() {
    let row = unhex("f8000000f8000000000f0000000f0000").unwrap();
    let data = row.repeat(3);
    let mut assembler = TableAssembler::new(48).unwrap();
    assert!(assembler.push(1, &data[..29]).is_err());
    assembler.push(0, &data[..29]).unwrap();
    let mut tail = data[29..].to_vec();
    tail.resize(29, 0xee);
    assembler.push(1, &tail).unwrap();
    assert_eq!(assembler.finish().unwrap(), data);
    let rule = VlanRule::decode(&row).unwrap();
    assert_eq!(
        (
            rule.filter_outer_priority,
            rule.filter_outer_vid,
            rule.treatment_inner_priority,
            rule.tags_to_remove
        ),
        (15, 4096, 15, 0)
    );
    assert!(TableAssembler::new(4097).is_err());
    assert!(TableAssembler::new(1).unwrap().finish().is_err());
}

struct Script {
    responses: VecDeque<BaselineFrame>,
}
impl OmciTransport for Script {
    fn send(&mut self, request: &BaselineFrame) -> io::Result<()> {
        let mut response = *request;
        response.message_type = (request.message_type & 0x1f) | 0x20;
        response.payload = [0; 32];
        match request.message_type {
            0x4d => response.payload[1] = 1,
            0x4e => {
                response.payload[..10].copy_from_slice(&[1, 6, 0, 0, 0xe0, 0, 1, 0x23, 0, 2]);
            }
            0x49 => {
                response.payload[1..3].copy_from_slice(&request.payload[..2]);
            }
            _ => (),
        }
        let mut wrong = response;
        wrong.instance_id ^= 1;
        self.responses.extend([*request, wrong, response]);
        Ok(())
    }
    fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
        Ok(self.responses.pop_front())
    }
}
#[test]
fn session_ignores_echo_and_wrong_entity_and_uploads_via_transport() {
    let mut session = OmciSession::new(Script {
        responses: VecDeque::new(),
    });
    let report = session.upload_mib().unwrap();
    assert!(report.uploaded_complete);
    assert_eq!(report.entities[0].class_id, 262);
    assert_eq!(
        report.entities[0].attributes[0]
            .value
            .as_ref()
            .unwrap()
            .unsigned(),
        Some(0x123)
    );
    session
        .set_attributes(262, 0, &[(1, AttributeValue::number(0x123))])
        .unwrap();
    session.delete(45, 1).unwrap();
}
#[test]
fn session_guards_reject_before_any_transaction() {
    let mut session = OmciSession::new(Script {
        responses: VecDeque::new(),
    });
    session.finish_before(Instant::now());
    assert_eq!(
        session.get(256, 0, 0x8000).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    let mut session = OmciSession::new(Script {
        responses: VecDeque::new(),
    });
    session.cancel_when(Arc::new(AtomicBool::new(true)));
    assert_eq!(
        session.get(256, 0, 0x8000).unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
    let mut session = OmciSession::new(Script {
        responses: VecDeque::new(),
    });
    session.guard_registration(|| false);
    assert_eq!(
        session.get(256, 0, 0x8000).unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
}

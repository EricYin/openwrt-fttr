//! Read-only ONU downstream FEC PM. No inferred FPGA enable state or automatic CREATE.
use crate::pon_engine::omci::{attributes::AttributeBlock, OmciSession};
use fttr_api::{FecPmCounters, FecStatus, MeasurementEvidence};
use std::time::{Duration, Instant};

const MASK: u16 = 0xbe00;
const SOURCE: &str = "OMCI GET class=312 (completed downstream 15-minute interval)";

pub(super) fn unavailable(reason: impl Into<String>) -> FecStatus {
    FecStatus {
        ani_entity_id: None,
        control_supported: false,
        downstream_enabled: None,
        upstream_enabled: None,
        downstream_pm: None,
        measurement: MeasurementEvidence::unavailable(SOURCE, "PM interval counters", reason),
    }
}

#[derive(Default)]
pub(in crate::controller) struct Poll {
    retry: Option<(Instant, String)>,
}

impl Poll {
    pub fn read(&mut self, session: &mut OmciSession, ani: u16) -> FecStatus {
        let mut status = self.read_inner(session, ani);
        status.ani_entity_id = Some(ani);
        status
    }

    fn read_inner(&mut self, session: &mut OmciSession, ani: u16) -> FecStatus {
        if let Some((after, reason)) = &self.retry {
            if Instant::now() < *after {
                return unavailable(reason.clone());
            }
        }
        // A missing PM instance is not proof of absent FEC support. Probing it does
        // not change ONU configuration; failures back off independently of liveness.
        let result = session
            .with_timeout(Duration::from_secs(2), |session| {
                session.get_attributes(312, ani, MASK)
            })
            .and_then(|block| {
                let counters = parse(ani, &block)?;
                let raw = block
                    .attributes
                    .iter()
                    .map(|attr| attr.raw_hex.as_str())
                    .collect::<String>();
                Ok((counters, raw))
            });
        match result {
            Ok((counters, raw)) => {
                self.retry = None;
                FecStatus {
                    downstream_pm: Some(counters),
                    measurement: MeasurementEvidence::observed(
                        format!("{SOURCE} entity=0x{ani:04x} mask=0x{MASK:04x}"),
                        "PM interval counters",
                        raw,
                    ),
                    ..unavailable("not_observed")
                }
            }
            Err(error) => {
                let reason = format!("pm_get_failed (support and enablement unknown): {error}");
                self.retry = Some((Instant::now() + Duration::from_secs(300), reason.clone()));
                unavailable(reason)
            }
        }
    }
}

fn parse(entity_id: u16, block: &AttributeBlock) -> std::io::Result<FecPmCounters> {
    let value = |index| {
        block
            .attributes
            .iter()
            .find(|attr| attr.definition.index == index)
            .and_then(|attr| attr.value.unsigned())
            .ok_or_else(|| std::io::Error::other("incomplete FEC PM counters"))
    };
    Ok(FecPmCounters {
        entity_id,
        interval_end_time: u8::try_from(value(1)?).map_err(std::io::Error::other)?,
        corrected_bytes: value(3)?,
        corrected_code_words: value(4)?,
        uncorrectable_code_words: value(5)?,
        total_code_words: value(6)?,
        fec_seconds: u16::try_from(value(7)?).map_err(std::io::Error::other)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::{
        attributes::{AttributeCodec, AttributeContext},
        schema::MeRegistry,
        session::OmciTransport,
        wire::BaselineFrame,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct Reply {
        last: Option<BaselineFrame>,
        result: u8,
        sends: Arc<AtomicUsize>,
    }
    impl OmciTransport for Reply {
        fn send(&mut self, frame: &BaselineFrame) -> std::io::Result<()> {
            // FEC collection must never silently create or set PM entities.
            assert_eq!(frame.message_type, 0x49);
            assert_eq!(frame.class_id, 312);
            assert_eq!(&frame.payload[..2], &[0xbe, 0]);
            self.sends.fetch_add(1, Ordering::Relaxed);
            self.last = Some(frame.clone());
            Ok(())
        }
        fn receive(&mut self) -> std::io::Result<Option<BaselineFrame>> {
            Ok(self.last.take().map(|mut frame| {
                frame.message_type = 0x29;
                frame.payload = [0; 32];
                frame.payload[0] = self.result;
                frame.payload[1..3].copy_from_slice(&MASK.to_be_bytes());
                frame.payload[3..22].copy_from_slice(&[
                    7, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0xff, 0xff, 0xff, 0xff, 0, 4,
                ]);
                frame
            }))
        }
    }
    fn session(result: u8) -> (OmciSession, Arc<AtomicUsize>) {
        let sends = Arc::new(AtomicUsize::new(0));
        (
            OmciSession::new(Reply {
                last: None,
                result,
                sends: sends.clone(),
            }),
            sends,
        )
    }

    #[test]
    fn fec_pm_decodes_full_width_counters_without_claiming_enablement() {
        let (mut session, sends) = session(0);
        let status = Poll::default().read(&mut session, 0x8001);
        let counters = status.downstream_pm.unwrap();
        assert_eq!(counters.entity_id, 0x8001);
        assert_eq!(counters.interval_end_time, 7);
        assert_eq!(counters.corrected_bytes, 1);
        assert_eq!(counters.corrected_code_words, 2);
        assert_eq!(counters.uncorrectable_code_words, 3);
        assert_eq!(counters.total_code_words, u32::MAX);
        assert_eq!(counters.fec_seconds, 4);
        assert!(!status.control_supported);
        assert_eq!(status.downstream_enabled, None);
        assert_eq!(status.upstream_enabled, None);
        assert_eq!(sends.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn missing_pm_backs_off_without_asserting_no_fec_or_returning_zero_counters() {
        let (mut session, sends) = session(5);
        let mut poll = Poll::default();
        for _ in 0..2 {
            let status = poll.read(&mut session, 0);
            assert!(status.downstream_pm.is_none());
            assert!(status.measurement.reason.unwrap().contains("result=5"));
            assert_eq!(status.downstream_enabled, None);
        }
        assert_eq!(sends.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn truncated_counter_block_is_not_a_valid_zero_sample() {
        let block = AttributeCodec::decode(
            MeRegistry::get(312).unwrap(),
            MASK,
            &[0; 18],
            AttributeContext::GetResponse,
        );
        assert!(parse(0, &block).is_err());
    }
}

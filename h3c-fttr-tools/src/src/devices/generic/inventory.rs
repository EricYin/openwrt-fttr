//! Bridge endpoints and an available T-CONT from a complete ONU inventory.
use crate::pon_engine::omci::{attributes::AttributeValue, mib::MibReport};
use std::io;

pub(super) struct BridgeInventory {
    pub tcont: u16,
    pub endpoints: Vec<(u16, bool)>,
}

impl BridgeInventory {
    pub fn from_mib(mib: &MibReport, alloc: u16) -> io::Result<Self> {
        if !mib.uploaded_complete {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "generic bridge requires a complete MIB upload",
            ));
        }
        let mut tconts = Vec::new();
        let mut unis = Vec::new();
        let mut veips = Vec::new();
        for entity in &mib.entities {
            match entity.class_id {
                262 => {
                    let allocation = entity
                        .attributes
                        .iter()
                        .find(|attr| attr.definition.index == 1)
                        .and_then(|attr| attr.value.as_ref())
                        .and_then(AttributeValue::unsigned);
                    if let Some(value) = allocation {
                        if value == u32::from(alloc) || value == 0xff || value == 0xffff {
                            tconts.push((value != u32::from(alloc), entity.entity_id));
                        }
                    }
                }
                11 => unis.push(entity.entity_id),
                329 => veips.push(entity.entity_id),
                _ => {}
            }
        }
        // Prefer the existing assignment on retries; never steal another service's T-CONT.
        let (_, tcont) = tconts.into_iter().min().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "generic bridge has no matching or unassigned T-CONT in MIB",
            )
        })?;
        unis.sort_unstable();
        unis.dedup();
        veips.sort_unstable();
        veips.dedup();
        if unis.is_empty() && veips.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "generic bridge has no Ethernet UNI or VEIP in MIB",
            ));
        }
        u8::try_from(unis.len() + veips.len() + 2).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "too many generic bridge endpoints",
            )
        })?;
        let mut endpoints: Vec<_> = unis
            .into_iter()
            .map(|id| (id, false))
            .chain(veips.into_iter().map(|id| (id, true)))
            .collect();
        // Keep the shipped probe order for existing bridge/EVTO instance assignments.
        endpoints.sort_unstable_by_key(|&(id, veip)| {
            let order: &[u16] = if veip {
                &[0x601, 0x401, 1, 0x8001, 0x101, 0x201]
            } else {
                &[0x101, 0x102, 0x103, 0x104, 1, 2, 3, 4, 0x201, 0x202]
            };
            let index = order.iter().position(|candidate| *candidate == id);
            (index.is_none(), veip, index.unwrap_or(usize::MAX), id)
        });
        Ok(Self { tcont, endpoints })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::mib::{MibAssembler, MibRecord};

    fn inventory(entries: &[(u16, u16, u16)]) -> MibReport {
        let mut mib = MibAssembler::new(u16::try_from(entries.len()).unwrap());
        for (sequence, &(class_id, entity_id, alloc)) in entries.iter().enumerate() {
            let mut data = [0; 26];
            data[..2].copy_from_slice(&alloc.to_be_bytes());
            mib.push(MibRecord {
                sequence: u16::try_from(sequence).unwrap(),
                class_id,
                entity_id,
                attribute_mask: 0x8000,
                data,
            });
        }
        mib.finish()
    }

    #[test]
    fn discovers_star_entities_when_legacy_candidates_are_absent() {
        // Given: the STAR instance layout, including four UNIs and VEIP 0x0501.
        let mib = inventory(&[
            (262, 0x8001, 255),
            (262, 0x8002, 255),
            (11, 0x101, 0),
            (11, 0x102, 0),
            (11, 0x103, 0),
            (11, 0x104, 0),
            (329, 0x501, 0),
        ]);
        // When: planning the bridge for ONU 2.
        let plan = BridgeInventory::from_mib(&mib, 0x102).unwrap();
        // Then: use the advertised instances in their bridge configuration order.
        assert_eq!(
            (plan.tcont, plan.endpoints),
            (
                0x8001,
                vec![
                    (0x101, false),
                    (0x102, false),
                    (0x103, false),
                    (0x104, false),
                    (0x501, true)
                ]
            )
        );
    }

    #[test]
    fn preserves_legacy_endpoint_order_when_new_instances_are_discovered() {
        // Given: old nonascending UNI/VEIP assignments and two newly discoverable endpoints.
        let mib = inventory(&[
            (262, 0x8001, 255),
            (11, 1, 0),
            (11, 0x101, 0),
            (329, 0x601, 0),
            (11, 0x501, 0),
            (329, 0x501, 0),
        ]);
        // When: planning with inventory discovery instead of legacy probes.
        let plan = BridgeInventory::from_mib(&mib, 0x102).unwrap();
        // Then: new instances do not shift the legacy bridge/EVTO assignments.
        assert_eq!(
            plan.endpoints,
            vec![
                (0x101, false),
                (1, false),
                (0x601, true),
                (0x501, false),
                (0x501, true),
            ]
        );
    }

    #[test]
    fn reuses_assignment_when_an_earlier_tcont_is_free() {
        // Given: a free T-CONT before the existing business assignment.
        let mib = inventory(&[(262, 0x8001, 255), (262, 0x8002, 0x102), (329, 0x501, 0)]);
        // When: planning a retry.
        let plan = BridgeInventory::from_mib(&mib, 0x102).unwrap();
        // Then: retain the established allocation.
        assert_eq!(plan.tcont, 0x8002);
    }

    #[test]
    fn rejects_busy_tconts_when_no_matching_assignment_exists() {
        // Given: only a T-CONT allocated to a different service.
        let mib = inventory(&[(262, 0x8001, 0x103), (329, 0x501, 0)]);
        // When: planning another allocation.
        let result = BridgeInventory::from_mib(&mib, 0x102);
        // Then: no existing allocation is repurposed.
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn rejects_missing_endpoints_instead_of_inventing_a_uni() {
        // Given: no UNI or VEIP in a complete upload.
        let mib = inventory(&[(262, 0x8001, 255)]);
        // When: planning the bridge.
        let result = BridgeInventory::from_mib(&mib, 0x102);
        // Then: configuration stops before writes to a guessed instance.
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn rejects_partial_inventory_when_entities_are_missing() {
        // Given: an incomplete upload that happens to contain usable entities.
        let mut mib = inventory(&[(262, 0x8001, 255), (329, 0x501, 0)]);
        mib.uploaded_complete = false;
        // When: planning the bridge.
        let result = BridgeInventory::from_mib(&mib, 0x102);
        // Then: absence of other entities is not trusted.
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
    }
}

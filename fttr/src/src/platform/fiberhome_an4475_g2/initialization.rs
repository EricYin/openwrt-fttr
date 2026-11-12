//! Real, read-only initialization status; not a complete initialization recipe.
use super::{
    polling::{self, PollPolicy},
    protocol::{Bus, Fsl},
};
use std::{io, time::Duration};

pub const RAM_INIT_STATUS: u32 = 0x31;
pub const GPON_RAM_INIT_MASK: u32 = 0xbf;

/// Mirrors the stock GPON RAM test, but propagates errors and timeout. Bit6
/// (switch RAM) and bits8/9 (XGPON RAM) are deliberately not readiness criteria
/// in this stock test. A successful result is not optical/SerDes/OLT readiness.
pub fn wait_ram_initialized<B: Bus>(
    fsl: &mut Fsl<B>,
    policy: PollPolicy,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<u32> {
    polling::register(
        fsl,
        RAM_INIT_STATUS,
        GPON_RAM_INIT_MASK,
        GPON_RAM_INIT_MASK,
        policy,
        checkpoint,
    )
}

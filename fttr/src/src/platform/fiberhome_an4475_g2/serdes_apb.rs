//! Bounded access to the chip's indirect SerDes APB window.
//!
//! This is a low-level primitive only. It does not configure clocks, reset,
//! mode, or the OLT/NNI SerDes profiles.

use super::protocol::{Bus, Fsl};
use std::{io, time::Duration};

const BUS_SELECT: u32 = 0x166;
const APB_WDATA: u32 = 0x173;
const APB_RDATA: u32 = 0x174;
const APB_CTRL: u32 = 0x175;
const APB_ADDRESS_MASK: u32 = 0xffff;
const APB_WRITE: u32 = 1 << 16;
const APB_ACK: u32 = 1 << 17;
const APB_ERROR: u32 = 1 << 18;

/// The stock code performs at most 100 ACK reads with a 1 us delay between
/// reads. The production caller supplies the actual wait/cancellation policy.
pub const APB_ACK_READS: u16 = 100;
pub const APB_ACK_INTERVAL: Duration = Duration::from_micros(1);

/// Owns one FSL session because APB selector and command registers are shared.
/// A failed command or exhausted ACK budget leaves the indirect transaction
/// state unknown and permanently blocks this session; no command is replayed.
pub struct SerdesApb<B> {
    fsl: Fsl<B>,
    bus: u8,
    indeterminate: bool,
}

impl<B: Bus> SerdesApb<B> {
    pub fn new(fsl: Fsl<B>, bus: u8) -> io::Result<Self> {
        if bus > 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SerDes APB bus selector must fit the two-bit field",
            ));
        }
        Ok(Self {
            fsl,
            bus,
            indeterminate: false,
        })
    }

    pub fn needs_reconciliation(&self) -> bool {
        self.indeterminate
    }

    pub fn read(
        &mut self,
        address: u16,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<u8> {
        self.ensure_ready()?;
        checkpoint(Duration::ZERO)?;
        self.indeterminate = true;
        self.write_register(BUS_SELECT, self.bus as u32)?;
        checkpoint(Duration::ZERO)?;
        let ctrl = self.fsl.read_register(APB_CTRL)?;
        let command =
            (ctrl & !(APB_ADDRESS_MASK | APB_WRITE | APB_ACK | APB_ERROR)) | address as u32;
        self.write_register(APB_CTRL, command)?;
        let status = self.wait_ack(checkpoint)?;
        if status & APB_ERROR != 0 {
            return Err(io::Error::other("SerDes APB target reported an error"));
        }
        checkpoint(Duration::ZERO)?;
        let value = self.fsl.read_register(APB_RDATA)? as u8;
        self.indeterminate = false;
        Ok(value)
    }

    pub fn write(
        &mut self,
        address: u16,
        value: u8,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<()> {
        self.ensure_ready()?;
        checkpoint(Duration::ZERO)?;
        self.indeterminate = true;
        self.write_register(BUS_SELECT, self.bus as u32)?;
        checkpoint(Duration::ZERO)?;
        self.write_register(APB_WDATA, value as u32)?;
        checkpoint(Duration::ZERO)?;
        let ctrl = self.fsl.read_register(APB_CTRL)?;
        let command = (ctrl & !(APB_ADDRESS_MASK | APB_WRITE | APB_ACK | APB_ERROR))
            | APB_WRITE
            | address as u32;
        self.write_register(APB_CTRL, command)?;
        let status = self.wait_ack(checkpoint)?;
        if status & APB_ERROR != 0 {
            return Err(io::Error::other("SerDes APB target reported an error"));
        }
        self.indeterminate = false;
        Ok(())
    }

    fn ensure_ready(&self) -> io::Result<()> {
        if self.indeterminate {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "SerDes APB session requires reconciliation",
            ));
        }
        Ok(())
    }

    fn write_register(&mut self, address: u32, value: u32) -> io::Result<()> {
        self.fsl.write(
            super::super::diagnostics::RegisterSpace::Register,
            address,
            0,
            &[value],
        )
    }

    fn wait_ack(
        &mut self,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<u32> {
        for attempt in 0..APB_ACK_READS {
            checkpoint(if attempt == 0 {
                Duration::ZERO
            } else {
                APB_ACK_INTERVAL
            })?;
            let status = self.fsl.read_register(APB_CTRL)?;
            if status & APB_ACK != 0 {
                return Ok(status);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "SerDes APB ACK polling budget exhausted",
        ))
    }
}

#[cfg(test)]
mod tests;

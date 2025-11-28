//! Address remapping functionality for DWARF debug information conversion.
//!
//! The latest (0.32.2) gimli crate provides conversion facilities for DWARF entries
//! that allows a generic u64->u64 address conversion function to be provided. This function
//! is then used wherever an absolute address translation is needed. This however assumes that
//! the offsets remain the same - i.e, that the binary was simply shifted.
//!
//! In our use case, we required to translate the code into more "spaced out" addresses, which
//! require adjusting offsets as well. Luckily, this can easily be done using the same conversion function,
//! as long as we track the beginning of each range and calculate offsets relative to that:
//! remapped_offset = remap_address(original_start + original_offset) - remap_address(original_start)
//!
//! This module provides the `AddressRemapper` struct which handles the translation
//! of addresses and offsets, replacing the direct use of the conversion function. It supports
//! both absolute address translation and relative offset calculations within ranges.
//!
//! This struct is used by various conversion functions to replace the original address-only code.

use core::cell::Cell;

use crate::{
    write::{Address, ConvertError, ConvertResult},
    Register,
};
use log::{error, info};

/// Trait for remapping addresses and registers
pub trait RemapperTr {
    /// Remap a register from the original to the new one
    fn remap_register(&self, orig_register: Register) -> ConvertResult<Register>;
    /// Remap a single address
    fn remap_address(&self, orig_address: u64) -> ConvertResult<Address>;
    /// Begin a new range for offset remapping
    fn begin_range(&self, orig_address: u64) -> ConvertResult<Address>;
    /// End the current range for offset remapping
    fn end_range(&self) -> ConvertResult<()>;
    /// Remap an offset relative to the current range start address
    fn remap_offset(&self, orig_offset: u64) -> ConvertResult<u64>;
    /// Remap a pair of (start address, end address)
    fn remap_start_end(
        &self,
        orig_start_addr: u64,
        orig_end_addr: u64,
    ) -> ConvertResult<(Address, Address)>;
    /// Remap a pair of (start offset, end offset)
    fn remap_start_end_offsets(
        &self,
        orig_start_offset: u64,
        orig_end_offset: u64,
    ) -> ConvertResult<(u64, u64)>;
    /// Remap a pair of (start address, length)
    fn remap_start_length(
        &self,
        orig_start: u64,
        orig_length: u64,
    ) -> ConvertResult<(Address, u64)>;
}

/// Address remapper struct implementing the RemapperTr trait
pub struct Remapper<'a> {
    // Convert each original address to a new address
    convert_address: &'a dyn Fn(u64) -> ConvertResult<Address>,
    // Convert original registers to new registers
    convert_register: &'a dyn Fn(Register) -> ConvertResult<Register>,
    // Current range start addresses for offset calculations
    orig_range_start_addr: Cell<Option<u64>>,
    remap_range_start_addr: Cell<Option<u64>>,
}

impl std::fmt::Debug for Remapper<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remapper")
            .field("orig_range_start_addr", &self.orig_range_start_addr)
            .field("remap_range_start_addr", &self.remap_range_start_addr)
            .finish()
    }
}

impl<'a> Remapper<'a> {
    /// Create a new Remapper with the given conversion functions
    pub fn new(
        convert_address: &'a dyn Fn(u64) -> ConvertResult<Address>,
        convert_register: &'a dyn Fn(Register) -> ConvertResult<Register>,
    ) -> Self {
        Remapper {
            convert_address,
            convert_register,
            orig_range_start_addr: Cell::new(None),
            remap_range_start_addr: Cell::new(None),
        }
    }

    /// Create a test remapper that does identity mapping
    pub fn test_remapper() -> Self {
        Self::new(&|address| Ok(Address::Constant(address)), &|register| {
            Ok(register)
        })
    }

    // Remap and return as u64 or fail (when just the raw address is needed)
    fn remap_address_as_u64(&self, orig_address: u64) -> ConvertResult<u64> {
        match self.remap_address(orig_address) {
            Ok(Address::Constant(addr)) => Ok(addr),
            Ok(Address::Symbol { .. }) => {
                error!("Cannot remap {orig_address:#x} as u64, got a symbol",);
                Err(ConvertError::InvalidAddress(orig_address))
            }
            Err(e) => Err(e),
        }
    }

    fn verify_remapped_start_end(remap_start: u64, remap_end: u64) -> ConvertResult<()> {
        if remap_start > remap_end {
            error!(
                "Remapped end address {remap_end:#x} is less than start address {remap_start:#x}",
            );
            return Err(ConvertError::InvalidRangeRelativeAddress);
        }
        Ok(())
    }
}

impl RemapperTr for Remapper<'_> {
    fn remap_register(&self, orig_register: Register) -> ConvertResult<Register> {
        match (self.convert_register)(orig_register) {
            Ok(reg) => {
                info!("Remapped register {:?} to {:?}", orig_register, reg);
                Ok(reg)
            }
            Err(e) => {
                error!("Failed to remap register {:?}", orig_register);
                Err(e)
            }
        }
    }

    // Remap a single address
    fn remap_address(&self, orig_address: u64) -> ConvertResult<Address> {
        match (self.convert_address)(orig_address) {
            Ok(addr) => {
                info!("Remapped address {orig_address:#x} to {addr:?}",);
                Ok(addr)
            }
            Err(e) => {
                error!("Failed to remap address {orig_address:#x}");
                Err(e)
            }
        }
    }

    // Start a new range, so that offsets can be remapped relative to this address
    fn begin_range(&self, orig_address: u64) -> ConvertResult<Address> {
        let remap_address = self.remap_address_as_u64(orig_address)?;
        info!("Starting range at {orig_address:#x}/{remap_address:#x}");
        self.orig_range_start_addr.set(Some(orig_address));
        self.remap_range_start_addr.set(Some(remap_address));
        Ok(Address::Constant(remap_address))
    }

    // Explicitly indicate the end of a range
    fn end_range(&self) -> ConvertResult<()> {
        if let (Some(orig_address), Some(remap_address)) = (
            self.orig_range_start_addr.get(),
            self.remap_range_start_addr.get(),
        ) {
            self.orig_range_start_addr.set(None);
            self.remap_range_start_addr.set(None);
            info!("Ending range started at {orig_address:#x}/{remap_address:#x}",);
            Ok(())
        } else {
            error!("Ending range, but no range was started",);
            Err(ConvertError::InvalidRangeRelativeAddress)
        }
    }

    // Remap an offset relative to the current start address
    // If there is no valid start address set, return error
    fn remap_offset(&self, orig_offset: u64) -> ConvertResult<u64> {
        // Must align the offset
        const ALIGN_TO: u64 = 4;
        let aligned_offset = orig_offset & !(ALIGN_TO - 1);
        let offset_aligned_remainder = orig_offset & (ALIGN_TO - 1);
        // Verify theres a valid range start
        match (
            self.orig_range_start_addr.get(),
            self.remap_range_start_addr.get(),
        ) {
            (Some(orig_start), Some(remap_start)) => {
                // Calculate the remapped offset
                let orig_end = orig_start + aligned_offset;
                let remap_end = self.remap_address_as_u64(orig_end)?;
                Self::verify_remapped_start_end(remap_start, remap_end)?;
                // Re-add the alignment remainder (if any)
                let remap_offset = remap_end - remap_start + offset_aligned_remainder;
                info!(
                    "Remapped offset {orig_start:#x}+{orig_offset:#x} to {remap_start:#x}+{remap_offset:#x}",
                );
                Ok(remap_offset)
            }
            _ => {
                error!("Cannot remap offset if there is no range start");
                Err(ConvertError::InvalidRangeRelativeAddress)
            }
        }
    }

    // Remap a pair of (start address, end address)
    fn remap_start_end(
        &self,
        orig_start_addr: u64,
        orig_end_addr: u64,
    ) -> ConvertResult<(Address, Address)> {
        let remap_start_addr = self.remap_address_as_u64(orig_start_addr)?;
        let remap_end_addr = self.remap_address_as_u64(orig_end_addr)?;
        Self::verify_remapped_start_end(remap_start_addr, remap_end_addr)?;
        info!(
            "Remapped (start, end) {orig_start_addr:#x}:{orig_end_addr:#x} to {remap_start_addr:#x}:{remap_end_addr:#x}",
        );
        Ok((
            Address::Constant(remap_start_addr),
            Address::Constant(remap_end_addr),
        ))
    }

    // Remap a pair of (start offset, end offset)
    fn remap_start_end_offsets(
        &self,
        orig_start_offset: u64,
        orig_end_offset: u64,
    ) -> ConvertResult<(u64, u64)> {
        let remap_start_offset = self.remap_offset(orig_start_offset)?;
        let remap_end_offset = self.remap_offset(orig_end_offset)?;
        Self::verify_remapped_start_end(remap_start_offset, remap_end_offset)?;
        info!(
            "Remapped (start, end) offsets {orig_start_offset:#x}:{orig_end_offset:#x} to {remap_start_offset:#x}:{remap_end_offset:#x}",
        );
        Ok((remap_start_offset, remap_end_offset))
    }

    // Remap a pair of (start address, length)
    fn remap_start_length(
        &self,
        orig_start: u64,
        orig_length: u64,
    ) -> ConvertResult<(Address, u64)> {
        // Create a new, temporary remapper to avoid messing with the current range
        let temp_remapper = Remapper::new(self.convert_address, self.convert_register);
        let remap_start_addr = temp_remapper.remap_address_as_u64(orig_start)?;
        temp_remapper.orig_range_start_addr.set(Some(orig_start));
        temp_remapper
            .remap_range_start_addr
            .set(Some(remap_start_addr));
        let remap_length = temp_remapper.remap_offset(orig_length)?;

        info!(
            "Remapped (start, length) pair {orig_start:#x}:{orig_length:#x} to {remap_start_addr:#x}:{remap_length:#x}",
        );

        Ok((Address::Constant(remap_start_addr), remap_length))
    }
}

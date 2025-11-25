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

use crate::write::{Address, ConvertError, ConvertResult};
use log::{info, warn};
use std::string::{String, ToString};

pub(crate) struct AddressRemapper<'a> {
    // Same as used by other conversion functions throughout gimli::write
    convert_address: &'a dyn Fn(u64) -> Option<Address>,
    // Current range start addresses for offset calculations
    // Can be set manually for each new range - starts automatically at 0
    orig_range_start_addr: Option<u64>,
    remap_range_start_addr: Option<u64>,
    // Used for logging purposes
    cur_module: &'a str,
    cur_loc: &'a str,
}

impl<'a> AddressRemapper<'a> {
    pub(crate) fn new(
        convert_address: &'a dyn Fn(u64) -> Option<Address>,
        cur_module: &'a str,
    ) -> Self {
        AddressRemapper {
            convert_address,
            orig_range_start_addr: Some(0),
            remap_range_start_addr: Some(0),
            cur_module,
            cur_loc: "<NONE>",
        }
    }

    // Update the current location for logging purposes
    pub(crate) fn set_cur_loc(&mut self, cur_loc: &'a str) {
        self.cur_loc = cur_loc;
    }

    // Logging utilities
    fn log_info(&self, msg: String) {
        info!("[{}::{}] {}", self.cur_module, self.cur_loc, msg);
    }
    fn log_warn(&self, msg: String) {
        warn!("[{}::{}] {}", self.cur_module, self.cur_loc, msg);
    }

    // Convert the raw address or fail
    pub(crate) fn remap_address(&self, orig_address: u64) -> ConvertResult<Address> {
        match (self.convert_address)(orig_address) {
            Some(address) => Ok(address),
            None => {
                self.log_warn(format!("Failed to remap address {orig_address:#x}"));
                Err(ConvertError::InvalidAddress)
            }
        }
    }

    // Remap and return as u64 or fail (when just the raw address is needed)
    fn remap_address_as_u64(&self, orig_address: u64) -> ConvertResult<u64> {
        match self.remap_address(orig_address) {
            Ok(Address::Constant(addr)) => Ok(addr),
            Ok(Address::Symbol { .. }) => {
                self.log_warn(format!(
                    "Failed to remap {orig_address:#x} as u64, got symbol"
                ));
                Err(ConvertError::InvalidAddress)
            }
            Err(e) => Err(e),
        }
    }

    // Explicitly indicate the beginning of a range
    pub(crate) fn begin_range(&mut self, orig_address: u64) -> ConvertResult<Address> {
        let remap_address = self.remap_address_as_u64(orig_address).map_err(|e| {
            // If there's an error remapping, clear the range start so there's no ambiguity about
            // the current range (or lack thereof)
            self.orig_range_start_addr = None;
            self.remap_range_start_addr = None;
            e
        })?;
        self.log_info(format!(
            "Starting range at {orig_address:#x} to {remap_address:#x}"
        ));
        self.orig_range_start_addr = Some(orig_address);
        self.remap_range_start_addr = Some(remap_address);
        Ok(Address::Constant(remap_address))
    }

    // Explicitly indicate the end of a range
    pub(crate) fn end_range(&mut self) -> ConvertResult<()> {
        if let (Some(orig_address), Some(remap_address)) =
            (self.orig_range_start_addr, self.remap_range_start_addr)
        {
            self.orig_range_start_addr = None;
            self.remap_range_start_addr = None;
            self.log_info(format!(
                "Ending range started at {orig_address:#x} to {remap_address:#x}",
            ));
            Ok(())
        } else {
            self.log_warn("Ending range, but no range was started".to_string());
            Err(ConvertError::InvalidRangeRelativeAddress)
        }
    }

    // Remap an offset relative to the current start address
    // If there is no valid start address set, return error
    pub(crate) fn remap_offset(&self, orig_offset: u64) -> ConvertResult<u64> {
        // Must align the offset
        const ALIGN_TO: u64 = 4;
        let aligned_offset = orig_offset & !(ALIGN_TO - 1);
        let offset_aligned_remainder = orig_offset & (ALIGN_TO - 1);
        // Verify theres a valid range start
        match (self.orig_range_start_addr, self.remap_range_start_addr) {
            (Some(orig_start), Some(remap_start)) => {
                // Calculate the remapped offset
                let orig_end = orig_start + aligned_offset;
                let remap_end = self.remap_address_as_u64(orig_end)?;
                if remap_start > remap_end {
                    self.log_warn(format!(
                        "Remapped end address {remap_end:#x} is less than start address {remap_start:#x}",
                    ));
                    return Err(ConvertError::InvalidRangeRelativeAddress);
                }
                // Re-add the remainder (if any)
                let remap_offset = remap_end - remap_start + offset_aligned_remainder;
                self.log_info(format!(
                    "Remapped offset {orig_start:#x}+{orig_offset:#x} to {remap_start:#x}+{remap_offset:#x}",
                ));
                Ok(remap_offset)
            }
            _ => {
                self.log_warn("Cannot remap offset if there is no range start".to_string());
                Err(ConvertError::InvalidRangeRelativeAddress)
            }
        }
    }

    // Remap a pair of (start address, end address)
    pub(crate) fn remap_start_end(
        &self,
        orig_start_addr: u64,
        orig_end_addr: u64,
    ) -> ConvertResult<(Address, Address)> {
        let remap_start_addr = self.remap_address_as_u64(orig_start_addr)?;
        let remap_end_addr = self.remap_address_as_u64(orig_end_addr)?;
        self.log_info(format!(
            "Remapping (start, end) {orig_start_addr:#x}:{orig_end_addr:#x} to {remap_start_addr:#x}:{remap_end_addr:#x}",
        ));
        Ok((
            Address::Constant(remap_start_addr),
            Address::Constant(remap_end_addr),
        ))
    }

    // Remap a pair of (start offset, end offset)
    pub(crate) fn remap_start_end_offsets(
        &self,
        orig_start_offset: u64,
        orig_end_offset: u64,
    ) -> ConvertResult<(u64, u64)> {
        self.log_info(format!(
            "Remapping (start offset, end offset) pair ({orig_start_offset:#x}, {orig_end_offset:#x}):"
        ));
        Ok((
            self.remap_offset(orig_start_offset)?,
            self.remap_offset(orig_end_offset)?,
        ))
    }

    // Remap a pair of (start address, length)
    pub(crate) fn remap_start_length(
        &self,
        orig_start: u64,
        orig_length: u64,
    ) -> ConvertResult<(Address, u64)> {
        let remap_begin_addr = self.remap_address(orig_start)?;
        self.log_info(format!(
            "Remapping (start, length) pair ({orig_start:#x}, {orig_length:#x}):"
        ));
        // Create a new, temporary remapper to avoid messing with the current range
        let mut temp_offset_helper = AddressRemapper::new(self.convert_address, self.cur_module);
        let _unused = temp_offset_helper.begin_range(orig_start)?;
        let remap_length = temp_offset_helper.remap_offset(orig_length)?;

        Ok((remap_begin_addr, remap_length))
    }
}

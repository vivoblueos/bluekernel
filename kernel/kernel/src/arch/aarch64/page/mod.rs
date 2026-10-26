// Copyright (c) 2026 vivo Mobile Communication Co., Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//       http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(aarch64_va_bits_39)]
mod a64_4k_39;
#[cfg(aarch64_va_bits_39)]
pub use a64_4k_39::{Level0, Level1, Level2};

#[cfg(aarch64_va_bits_48)]
mod a64_4k_48;
use crate::mm::{CachePolicy, MemAttribute, MemFlags, MemType};
#[cfg(aarch64_va_bits_48)]
pub use a64_4k_48::{Level0, Level1, Level2};
use tock_registers::{interfaces::*, register_bitfields, registers::InMemoryRegister};

const PAGE_DESCRIPTOR_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

// Keep these indices in sync with MAIR_EL1 in mmu::init_el1_enable_mmu.
const MAIR_DEVICE_INDEX: u64 = 0;
const MAIR_WRITE_BACK_INDEX: u64 = 1;
const MAIR_NON_CACHEABLE_INDEX: u64 = 2;
const MAIR_WRITE_THROUGH_INDEX: u64 = 3;

register_bitfields! {u64,
    pub PAGE_DESCRIPTOR [
        /// Unprivileged execute-never.
        UXN OFFSET(54) NUMBITS(1) [
            False = 0,
            True = 1
        ],

        /// Privileged execute-never.
        PXN OFFSET(53) NUMBITS(1) [
            False = 0,
            True = 1
        ],

        /// Current page table entry is in a continuous set of physical pages
        CONT_PHY_PAGES OFFSET(52) NUMBITS(1) [],

        /// Indicates that the page has been modified
        DMB OFFSET(51) NUMBITS(1) [],

        /// Output physical address
        OUTPUT_ADDR OFFSET(16) NUMBITS(36) [],

        /// When this bit is set, it means that the TLB page table entry corresponding to this page is process-specific.
        NG OFFSET(11) NUMBITS(1) [
            False = 0,
            True = 1
        ],

        /// Access flag, The hardware will automatically set up when you visit the page for the first time
        AF OFFSET(10) NUMBITS(1) [
            False = 0,
            True = 1
        ],

        /// Shared Memory Attributes
        SH OFFSET(8) NUMBITS(2) [
            NotShareable = 0b00,
            OuterShareable = 0b10,
            InnerShareable = 0b11
        ],

        /// Data access permissions (AP[2:1]).
        /// AP[1]:
        ///   0: not accessible via EL0, but accessible via EL1
        ///   1: accessed through EL0 and higher privilege exceptions
        /// AP[2]:
        ///   0: read and write
        ///   1: read only
        AP OFFSET(6) NUMBITS(2) [
            EL1_RW = 0b00,
            EL0_RW = 0b01,
            EL1_RO = 0b10,
            EL0_RO = 0b11
        ],

        /// Non-secure
        NS OFFSET(5) NUMBITS(1) [],

        /// Memory attributes index.
        ATTRINDX OFFSET(2) NUMBITS(3) [],

        /// 0: Reserved page table entries
        /// 1: Page table
        TYPE OFFSET(1) NUMBITS(1) [
            Reserved = 0,
            Page = 1
        ],

        /// 0: Invalid descriptor
        /// 1: valid descriptor
        VALID OFFSET(0) NUMBITS(1) [
            Invalid = 0,
            Valid = 1
        ]
    ]
}

#[derive(Copy, Clone)]
pub enum DescriptorKind {
    Block,
    Page,
}

/// Encodes mapping requirements as an EL1 block or page descriptor.
///
/// No access permissions produce an invalid descriptor. Write-only and
/// execute-only mappings are rejected because this implementation cannot
/// disable data reads on an otherwise accessible page. Device memory must not
/// be executable. The caller must align the address to the mapping's page size.
pub fn set_descriptor(
    output_addr: u64,
    attributes: MemAttribute,
    descriptor_kind: DescriptorKind,
) -> Result<usize, &'static str> {
    let flags = attributes.flags;
    if !flags.intersects(MemFlags::READ | MemFlags::WRITE | MemFlags::EXECUTE) {
        return Ok(0);
    }
    if !flags.contains(MemFlags::READ) {
        return Err("AArch64 mappings require READ for WRITE or EXECUTE");
    }

    let memory_index = match attributes.memory_type {
        MemType::Device => {
            if flags.contains(MemFlags::EXECUTE) {
                return Err("device memory cannot be executable");
            }
            MAIR_DEVICE_INDEX
        }
        MemType::Normal(CachePolicy::WriteBack) => MAIR_WRITE_BACK_INDEX,
        MemType::Normal(CachePolicy::WriteThrough) => MAIR_WRITE_THROUGH_INDEX,
        // AArch64 Normal Non-cacheable permits gathering writes, including
        // write combining. It also satisfies normal Uncached's cache bypass.
        MemType::Normal(CachePolicy::WriteCombining | CachePolicy::Uncached) => {
            MAIR_NON_CACHEABLE_INDEX
        }
    };

    let entry = InMemoryRegister::<u64, PAGE_DESCRIPTOR::Register>::new(0);
    entry.write(
        PAGE_DESCRIPTOR::VALID::Valid
            + PAGE_DESCRIPTOR::AF::True
            + PAGE_DESCRIPTOR::ATTRINDX.val(memory_index),
    );
    entry.modify(
        match (
            flags.contains(MemFlags::USER),
            flags.contains(MemFlags::WRITE),
        ) {
            (false, false) => PAGE_DESCRIPTOR::AP::EL1_RO,
            (false, true) => PAGE_DESCRIPTOR::AP::EL1_RW,
            (true, false) => PAGE_DESCRIPTOR::AP::EL0_RO,
            (true, true) => PAGE_DESCRIPTOR::AP::EL0_RW,
        },
    );

    if !flags.contains(MemFlags::EXECUTE) || flags.contains(MemFlags::USER) {
        entry.modify(PAGE_DESCRIPTOR::PXN::True);
    }
    if !flags.contains(MemFlags::EXECUTE) || !flags.contains(MemFlags::USER) {
        entry.modify(PAGE_DESCRIPTOR::UXN::True);
    }
    if !flags.contains(MemFlags::GLOBAL) {
        entry.modify(PAGE_DESCRIPTOR::NG::True);
    }
    if matches!(attributes.memory_type, MemType::Normal(_)) {
        entry.modify(PAGE_DESCRIPTOR::SH::InnerShareable);
    }

    let mut value = entry.get() | (output_addr & PAGE_DESCRIPTOR_ADDR_MASK);
    if matches!(descriptor_kind, DescriptorKind::Page) {
        value |= PAGE_DESCRIPTOR::TYPE::Page.value;
    }
    Ok(value as usize)
}

#[cfg(test)]
mod tests;

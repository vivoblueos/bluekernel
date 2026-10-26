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

use crate::mm::{
    paging::PageFormat, CachePolicy, MemAttribute, MemFlags, MemType,
};
use tock_registers::{interfaces::*, register_bitfields, registers::InMemoryRegister};

const OUTPUT_ADDR_MASK: usize = 0x0000_ffff_ffff_f000;
const MAIR_DEVICE_INDEX: usize = 0;
const MAIR_WRITE_BACK_INDEX: usize = 1;
const MAIR_NON_CACHEABLE_INDEX: usize = 2;
const MAIR_WRITE_THROUGH_INDEX: usize = 3;

register_bitfields! {usize,
    pub PAGE_FORMAT [
        /// Level 1 table index, VA[38:30].
        INDEX1 OFFSET(30) NUMBITS(9) [],
        /// Level 2 table index, VA[29:21].
        INDEX2 OFFSET(21) NUMBITS(9) [],
        /// Level 3 table index, VA[20:12].
        INDEX3 OFFSET(12) NUMBITS(9) [],
        /// Offset within a 4 KiB page, VA[11:0].
        PAGE_OFFSET OFFSET(0) NUMBITS(12) [],
    ],

    pub LEAF_PAGE [
        VALID OFFSET(0) NUMBITS(1) [
            Invalid = 0,
            Valid = 1
        ],
        /// 0 is a block at levels 1/2; 1 is a page at level 3.
        TYPE OFFSET(1) NUMBITS(1) [
            Block = 0,
            Page = 1
        ],
        ATTRINDX OFFSET(2) NUMBITS(3) [],
        NS OFFSET(5) NUMBITS(1) [],
        AP OFFSET(6) NUMBITS(2) [
            EL1_RW = 0b00,
            EL0_RW = 0b01,
            EL1_RO = 0b10,
            EL0_RO = 0b11
        ],
        SH OFFSET(8) NUMBITS(2) [
            NotShareable = 0b00,
            OuterShareable = 0b10,
            InnerShareable = 0b11
        ],
        AF OFFSET(10) NUMBITS(1) [
            False = 0,
            True = 1
        ],
        NG OFFSET(11) NUMBITS(1) [
            Global = 0,
            NonGlobal = 1
        ],
        ADDR OFFSET(12) NUMBITS(36) [],
        CONTIGUOUS OFFSET(52) NUMBITS(1) [],
        PXN OFFSET(53) NUMBITS(1) [
            False = 0,
            True = 1
        ],
        UXN OFFSET(54) NUMBITS(1) [
            False = 0,
            True = 1
        ],
    ],

    pub PAGE_DIR [
        VALID OFFSET(0) NUMBITS(1) [
            Invalid = 0,
            Valid = 1
        ],
        TYPE OFFSET(1) NUMBITS(1) [
            Table = 1
        ],
        ADDR OFFSET(12) NUMBITS(36) [],
        PXN_TABLE OFFSET(59) NUMBITS(1) [],
        UXN_TABLE OFFSET(60) NUMBITS(1) [],
        AP_TABLE OFFSET(61) NUMBITS(2) [],
        NS_TABLE OFFSET(63) NUMBITS(1) [],
    ]
}

pub struct PageEntry;

impl PageFormat for PageEntry {
    fn get_translate(base: usize, att: MemAttribute, is_block: bool) -> usize {
        let flags = att.flags;
        if !flags.intersects(MemFlags::READ | MemFlags::WRITE | MemFlags::EXECUTE) {
            return 0;
        }
        assert!(
            flags.contains(MemFlags::READ),
            "AArch64 mappings require READ for WRITE or EXECUTE"
        );

        let memory_index = match att.memory_type {
            MemType::Device => {
                assert!(
                    !flags.contains(MemFlags::EXECUTE),
                    "device memory cannot be executable"
                );
                MAIR_DEVICE_INDEX
            }
            MemType::Normal(CachePolicy::WriteBack) => MAIR_WRITE_BACK_INDEX,
            MemType::Normal(CachePolicy::WriteThrough) => MAIR_WRITE_THROUGH_INDEX,
            MemType::Normal(CachePolicy::WriteCombining | CachePolicy::Uncached) => {
                MAIR_NON_CACHEABLE_INDEX
            }
        };

        let entry = InMemoryRegister::<usize, LEAF_PAGE::Register>::new(0);
        entry.write(
            LEAF_PAGE::VALID::Valid
                + LEAF_PAGE::AF::True
                + LEAF_PAGE::ATTRINDX.val(memory_index),
        );
        entry.modify(match (
            flags.contains(MemFlags::USER),
            flags.contains(MemFlags::WRITE),
        ) {
            (false, false) => LEAF_PAGE::AP::EL1_RO,
            (false, true) => LEAF_PAGE::AP::EL1_RW,
            (true, false) => LEAF_PAGE::AP::EL0_RO,
            (true, true) => LEAF_PAGE::AP::EL0_RW,
        });

        if !flags.contains(MemFlags::EXECUTE) || flags.contains(MemFlags::USER) {
            entry.modify(LEAF_PAGE::PXN::True);
        }
        if !flags.contains(MemFlags::EXECUTE) || !flags.contains(MemFlags::USER) {
            entry.modify(LEAF_PAGE::UXN::True);
        }
        if !flags.contains(MemFlags::GLOBAL) {
            entry.modify(LEAF_PAGE::NG::NonGlobal);
        }
        if matches!(att.memory_type, MemType::Normal(_)) {
            entry.modify(LEAF_PAGE::SH::InnerShareable);
        }

        let mut value = entry.get() | (base & OUTPUT_ADDR_MASK);
        if !is_block {
            value |= LEAF_PAGE::TYPE::Page.value;
        }
        value
    }
}

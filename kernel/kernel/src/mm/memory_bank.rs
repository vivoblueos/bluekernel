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

//! Abstraction over the memory banks available on the hardware platform.
//!
//! A memory bank is a contiguous range of physical memory described by the
//! platform (e.g. via a devicetree or firmware-provided memory map). This
//! module collects those banks so the rest of the memory management
//! subsystem can enumerate usable physical memory without knowing how a
//! particular board reports it.

use core::ptr::addr_of;

use crate::mm::{kernel_virt_to_phys, MemFlags, PhysicalAddress, VirtualAddress};
use heapless::Vec;

const MAX_REGIONS: usize = 128;

pub enum MemoryBank {
    MMIO((PhysicalAddress, PhysicalAddress)),
    RAM((PhysicalAddress, PhysicalAddress)),
}

#[derive(Debug)]
pub struct MemoryRegion {
    pub base: PhysicalAddress,
    pub flags: MemFlags,
    pub size: usize,
    pub is_reserved: bool,
    pub name: &'static str,
}

/// Describe RAM banks and the reserved boot image without allocating from the heap.
///
/// RAM bank regions overlap the reserved image regions. Consumers must give
/// reservations precedence over available RAM, regardless of region order.
pub fn create_memory_region_from_memory_bank(
    banks: &[MemoryBank],
) -> Vec<MemoryRegion, MAX_REGIONS> {
    let mut all_regions = Vec::new();
    let mut push = |r: MemoryRegion| {
        if r.size > 0 {
            all_regions.push(r).expect("too many memory regions");
        }
    };

    let physical = |addr: usize| kernel_virt_to_phys(VirtualAddress::new(addr));
    let kernel_start = physical(addr_of!(__text_start) as usize).into_raw();

    for bank in banks {
        let MemoryBank::RAM((start, end)) = bank else {
            continue;
        };
        push(MemoryRegion {
            base: *start,
            flags: MemFlags::READ | MemFlags::WRITE,
            size: end
                .into_raw()
                .checked_sub(start.into_raw())
                .expect("reversed memory bank"),
            is_reserved: false,
            name: "RAM",
        });
        if start.into_raw() < kernel_start {
            // Preserve firmware/boot data before the image (e.g. QEMU's FDT).
            push(MemoryRegion {
                base: *start,
                flags: MemFlags::READ | MemFlags::WRITE,
                size: end.into_raw().min(kernel_start) - start.into_raw(),
                is_reserved: true,
                name: "boot data",
            });
        }
    }

    // Extend sections to the next linker boundary to reserve their alignment
    // padding too. The entire image through _end must remain unavailable.
    push(MemoryRegion {
        base: physical(addr_of!(__text_start) as usize),
        flags: MemFlags::READ | MemFlags::EXECUTE,
        size: addr_of!(__rodata_start) as usize - addr_of!(__text_start) as usize,
        is_reserved: true,
        name: ".text",
    });
    push(MemoryRegion {
        base: physical(addr_of!(__rodata_start) as usize),
        size: addr_of!(__data_start) as usize - addr_of!(__rodata_start) as usize,
        flags: MemFlags::READ,
        is_reserved: true,
        name: ".rodata",
    });
    push(MemoryRegion {
        base: physical(addr_of!(__data_start) as usize),
        size: addr_of!(__bss_start) as usize - addr_of!(__data_start) as usize,
        flags: MemFlags::READ | MemFlags::WRITE,
        is_reserved: true,
        name: ".data .tdata .tbss .percpu",
    });
    push(MemoryRegion {
        base: physical(addr_of!(__bss_start) as usize),
        size: addr_of!(__sys_stack_start) as usize - addr_of!(__bss_start) as usize,
        flags: MemFlags::READ | MemFlags::WRITE,
        is_reserved: true,
        name: ".bss .init_array .bk_app_array",
    });
    push(MemoryRegion {
        base: physical(addr_of!(__sys_stack_start) as usize),
        size: addr_of!(__sys_stack_end) as usize - addr_of!(__sys_stack_start) as usize,
        flags: MemFlags::READ | MemFlags::WRITE,
        is_reserved: true,
        name: "boot stack",
    });
    push(MemoryRegion {
        base: physical(addr_of!(__sys_stack_end) as usize),
        size: addr_of!(_end) as usize - addr_of!(__sys_stack_end) as usize,
        flags: MemFlags::READ | MemFlags::WRITE,
        is_reserved: true,
        name: "boot page table",
    });

    all_regions
}

unsafe extern "C" {
    static __text_start: u8;
    static __rodata_start: u8;
    static __data_start: u8;
    static __bss_start: u8;
    static __sys_stack_start: u8;
    static __sys_stack_end: u8;
    static _end: u8;
}

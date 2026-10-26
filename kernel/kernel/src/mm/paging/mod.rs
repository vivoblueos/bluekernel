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

pub mod page_format;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{
    arch::page::PAGE_FORMAT,
    mm::{CachePolicy, MemAttribute, MemFlags, MemType},
};
pub use page_format::*;

static PAGETABLE_INIT_DONE: AtomicBool = AtomicBool::new(false);

enum PteType {
    Page,
    PageDirectory,
    Invalid,
}

#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct PageEntry(usize);

// Safty: Only be used before the MMU is enabled
pub unsafe extern "C" fn init_boot_pgtable<P: PageFormat>() -> usize {
    let cpu_id = crate::arch::current_cpu_id();

    unsafe extern "C" {
        static mut BOOT_PAGE_TABLE: u8;
    }

    let phys_addr: usize;
    unsafe {
        // Load the absolute physical address defined by the linker script.
        core::arch::asm!(
            "ldr {addr}, ={table}",
            addr = out(reg) phys_addr,
            table = sym BOOT_PAGE_TABLE,
            options(readonly, nostack, preserves_flags),
        );
    }

    if cpu_id == 0 {
        if crate::arch::current_cpu_id() == 0 {
            let entries_ptr = phys_addr as *mut PageEntry;
            unsafe {
                core::ptr::write_bytes(entries_ptr, 0, 512);
                let entries = core::slice::from_raw_parts_mut(entries_ptr, 512);

                for &base in crate::boards::MMU_L1_DEVICE_BASES {
                    let index = PAGE_FORMAT::INDEX1.read(base as usize);
                    let index = PAGE_FORMAT::INDEX1.read(base as usize);
                    let v = P::get_translate(
                        base as usize,
                        MemAttribute {
                            flags: MemFlags::READ | MemFlags::WRITE | MemFlags::GLOBAL,
                            memory_type: MemType::Device,
                        },
                        true,
                    );
                    entries[index] = PageEntry(v);
                }

                for &base in crate::boards::MMU_L1_NORMAL_BASES {
                    let index = PAGE_FORMAT::INDEX1.read(base as usize);
                    let index = PAGE_FORMAT::INDEX1.read(base as usize);
                    let v = P::get_translate(
                        base as usize,
                        MemAttribute {
                            flags: MemFlags::READ | MemFlags::WRITE | MemFlags::EXECUTE,
                            memory_type: MemType::Normal(CachePolicy::WriteBack),
                        },
                        true,
                    );
                    entries[index] = PageEntry(v);
                }
            }
        }
        PAGETABLE_INIT_DONE.store(true, Ordering::Release);
    } else {
        while !PAGETABLE_INIT_DONE.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
    phys_addr
}

pub trait PageFormat {
    fn get_translate(base: usize, att: MemAttribute, is_block: bool) -> usize;
}

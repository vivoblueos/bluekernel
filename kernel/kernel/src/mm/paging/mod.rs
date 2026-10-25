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
pub use page_format::*;

enum PteType {
    Page,
    PageDirectory,
    Invalid,
}

#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct PageEntry(usize);

pub unsafe extern "C" fn init_boot_pgtable() {
    unsafe extern "C" {
        static mut BOOT_PAGE_TABLE: u8;
    }

    let entries_ptr = core::ptr::addr_of_mut!(BOOT_PAGE_TABLE).cast::<PageEntry>();

    unsafe {
        core::ptr::write_bytes(entries_ptr, 0, 512);
        let entries =
            core::slice::from_raw_parts_mut(entries_ptr, 512);

        for &base in crate::boards::MMU_L1_DEVICE_BASES {
            let index = (base >> 30) as usize;
            
        }

        for &base in crate::boards::MMU_L1_NORMAL_BASES {
            let index = (base >> 30) as usize;
        }
    }
}

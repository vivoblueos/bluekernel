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

//! The kernel's shared virtual address space and permanent page tables.

use super::{
    address_space::{MmapFlags, Vma},
    kernel_phys_to_virt,
    paging::{first_fit::FirstFitFrameAllocator, mapper::PageMapper, FrameAllocator, PageFormat},
    CachePolicy, MemAttribute, MemFlags, MemType, PhysicalAddress,
};
use crate::arch::page::PageEntry;
use spin::Once;

// Eight RAM regions, one heap region, and the board's device windows.
const REGION_COUNT: usize = 9 + crate::boards::KERNEL_DEVICE_REGIONS.len();

static KERNEL_SPACE: Once<KernelAddressSpace> = Once::new();

pub struct KernelAddressSpace {
    regions: [Option<Vma>; REGION_COUNT],
    mapper: PageMapper<&'static FirstFitFrameAllocator, PageEntry>,
}

impl KernelAddressSpace {
    pub fn get() -> Option<&'static Self> {
        KERNEL_SPACE.get()
    }

    pub fn root_address(&self) -> PhysicalAddress {
        self.mapper.root_address()
    }

    pub fn find_region(&self, addr: usize) -> Option<Vma> {
        self.regions
            .iter()
            .flatten()
            .find(|region| region.contains(addr))
            .copied()
    }

    pub fn translate(&self, addr: usize) -> Option<PhysicalAddress> {
        // A root-table walk alone cannot distinguish the TTBR0 and TTBR1
        // halves. Only addresses in the kernel's recorded regions are valid.
        self.find_region(addr)?;
        self.mapper.translate(addr)
    }

    fn map_linear(
        &mut self,
        start: usize,
        end: usize,
        flags: MemFlags,
        memory_type: MemType,
    ) -> Result<(), &'static str> {
        if start == end {
            return Ok(());
        }
        let len = end.checked_sub(start).ok_or("reversed kernel mapping")?;
        let virt = kernel_phys_to_virt(PhysicalAddress::new(start)).into_raw();
        let virt_end = virt.checked_add(len).ok_or("kernel mapping overflow")?;
        self.mapper.map_range(
            virt,
            start,
            len,
            MemAttribute {
                flags: flags | MemFlags::GLOBAL,
                memory_type,
            },
        )?;
        let mut vma_flags = MmapFlags::READ;
        if flags.contains(MemFlags::WRITE) {
            vma_flags = vma_flags | MmapFlags::WRITE;
        }
        if flags.contains(MemFlags::EXECUTE) {
            vma_flags = vma_flags | MmapFlags::EXECUTE;
        }
        self.record_region(virt, virt_end, vma_flags)
    }

    fn record_region(
        &mut self,
        start: usize,
        end: usize,
        flags: MmapFlags,
    ) -> Result<(), &'static str> {
        if self
            .regions
            .iter()
            .flatten()
            .any(|region| start < region.end() && region.start() < end)
        {
            return Err("overlapping kernel address-space regions");
        }
        let region = Vma::new(start, end, flags).map_err(|_| "invalid kernel region")?;
        let slot = self
            .regions
            .iter_mut()
            .find(|region| region.is_none())
            .ok_or("kernel region table is full")?;
        *slot = Some(region);
        Ok(())
    }

    fn map_heap(&mut self, start: usize) -> Result<usize, &'static str> {
        let mut allocator = FirstFitFrameAllocator::instance();
        let max_pages = allocator
            .usage()
            .ok_or("frame allocator is not initialized")?
            .free();
        let max_len = max_pages
            .checked_mul(PageEntry::PAGE_SIZE)
            .ok_or("kernel heap size overflow")?;
        // Allocate table frames first. Every free frame left afterwards belongs
        // to the heap, so mapping the backing pages must need no further frames.
        self.mapper.prepare_range(start, max_len)?;
        let pages = allocator
            .usage()
            .ok_or("frame allocator is not initialized")?
            .free();
        if pages == 0 {
            return Err("no physical memory remains for the kernel heap");
        }
        let len = pages
            .checked_mul(PageEntry::PAGE_SIZE)
            .ok_or("kernel heap size overflow")?;
        let end = start
            .checked_add(len)
            .ok_or("kernel heap address overflow")?;
        // FIXME: Temporarily make the heap executable so the ELF loader tests pass.
        // Replace this workaround with dedicated executable mappings for loaded ELF code.
        self.record_region(
            start,
            end,
            MmapFlags::READ | MmapFlags::WRITE | MmapFlags::EXECUTE,
        )?;
        let first = FrameAllocator::alloc_frame(&mut allocator)
            .ok_or("out of kernel heap frames")?
            .into_raw();
        // Reserve all heap frames before allocating any further page tables.
        // This preserves physical contiguity required by existing DMA buffers.
        for offset in (PageEntry::PAGE_SIZE..len).step_by(PageEntry::PAGE_SIZE) {
            let frame = FrameAllocator::alloc_frame(&mut allocator)
                .ok_or("out of kernel heap frames")?
                .into_raw();
            if first.checked_add(offset) != Some(frame) {
                return Err("kernel heap frames must be physically contiguous");
            }
        }
        self.mapper.map_range(
            start,
            first,
            len,
            MemAttribute {
                flags: MemFlags::READ | MemFlags::WRITE | MemFlags::EXECUTE | MemFlags::GLOBAL,
                memory_type: MemType::Normal(CachePolicy::WriteBack),
            },
        )?;
        Ok(end)
    }

    fn bootstrap() -> Result<Self, &'static str> {
        use super::{kernel_virt_to_phys, VirtualAddress};
        use core::ptr::addr_of;

        unsafe extern "C" {
            static __text_start: u8;
            static __rodata_start: u8;
            static __data_start: u8;
            static __bss_start: u8;
            static __sys_stack_start: u8;
            static __sys_stack_end: u8;
            static __heap_start: u8;
            static _end: u8;
        }
        let physical = |addr: usize| kernel_virt_to_phys(VirtualAddress::new(addr)).into_raw();
        let ram_start = crate::boards::PHYS_DRAM_BASE as usize;
        let ram_end = ram_start
            .checked_add(crate::boards::PHYS_DRAM_SIZE as usize)
            .ok_or("physical RAM range overflow")?;
        // The linker aligns each boundary to a page. The final boot-table page
        // is covered too; the heap is mapped separately outside the direct map.
        let boundaries = [
            ram_start,
            physical(addr_of!(__text_start) as usize),
            physical(addr_of!(__rodata_start) as usize),
            physical(addr_of!(__data_start) as usize),
            physical(addr_of!(__bss_start) as usize),
            physical(addr_of!(__sys_stack_start) as usize),
            physical(addr_of!(__sys_stack_end) as usize),
            physical(addr_of!(_end) as usize),
            ram_end,
        ];
        if boundaries.windows(2).any(|range| range[0] > range[1]) {
            return Err("kernel sections do not fit in physical RAM");
        }
        if boundaries
            .iter()
            .any(|addr| addr & (PageEntry::PAGE_SIZE - 1) != 0)
        {
            return Err("kernel section boundaries must be page aligned");
        }
        let allocator = FirstFitFrameAllocator::instance();
        // SAFETY: boot initialized the physical allocator and the boot direct
        // map. New table frames are disjoint from the reserved kernel image,
        // and these tables are retained permanently by KERNEL_SPACE.
        let mapper = unsafe { PageMapper::create(allocator)? };
        let mut space = Self {
            regions: [None; REGION_COUNT],
            mapper,
        };
        let normal = MemType::Normal(CachePolicy::WriteBack);
        for (index, range) in boundaries.windows(2).enumerate() {
            let flags = match index {
                1 => MemFlags::READ | MemFlags::EXECUTE,
                2 => MemFlags::READ,
                _ => MemFlags::READ | MemFlags::WRITE,
            };
            space.map_linear(range[0], range[1], flags, normal)?;
        }
        for &(base, size) in crate::boards::KERNEL_DEVICE_REGIONS {
            let end = base.checked_add(size).ok_or("device mapping overflow")?;
            space.map_linear(base, end, MemFlags::READ | MemFlags::WRITE, MemType::Device)?;
        }
        let heap_start = addr_of!(__heap_start) as usize;
        let heap_end = space.map_heap(heap_start)?;
        // SAFETY: the new table covers the running kernel and the newly backed
        // heap. No heap allocations were needed to construct the static region
        // metadata or the page tables. Install before writing heap metadata.
        unsafe { crate::arch::mmu::activate_kernel_table(space.root_address()) };
        crate::allocator::init_heap(heap_start as *mut u8, heap_end as *mut u8);
        Ok(space)
    }
}

pub(crate) fn init_kernel_heap() -> Result<(), &'static str> {
    KERNEL_SPACE.try_call_once(KernelAddressSpace::bootstrap)?;
    Ok(())
}

// Copyright (c) 2025 vivo Mobile Communication Co., Ltd.
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

// This code is based on https://github.com/weclaw1/slab_allocator/blob/master/src/slab.rs
// Copyright (c) 2017 Robert Węcławski
// SPDX-LICENSE: MIT

#[cfg(allocator = "slab_dynamic")]
use crate::buddy::{order_of_size, BuddyAllocator, BuddyInitError, MAX_ORDER};
use crate::{
    block::{used_block_hdr_for_allocation_unknown_align, BlockHdr, SIZE_USED},
    tlsf,
};
use blueos_infra::{
    impl_simple_intrusive_adapter,
    list::{
        singly_linked_list::SinglyLinkedList,
        typed_ilist::{List, ListHead},
    },
};
use core::{alloc::Layout, mem, ptr, ptr::NonNull};

// `heap` module (SpinLock wrappers) is kept in kernel/kernel/src/allocator/slab/heap.rs

const MIN_SLAB_SHIFT: usize = 3;
const MAX_SLAB_SHIFT: usize = 10;
const ADDITIONAL_SLAB_COUNT: usize = 2; // Add 96 and 192 to improve memory utilization
const SLAB_ALLOCATOR_COUNT: usize = MAX_SLAB_SHIFT - MIN_SLAB_SHIFT + 1 + ADDITIONAL_SLAB_COUNT;
const SYSTEM_ALLOCATOR_INDEX: usize = SLAB_ALLOCATOR_COUNT;
#[cfg(allocator = "slab_dynamic")]
const DYNAMIC_SLAB_ALLOCATOR_COUNT: usize = SLAB_ALLOCATOR_COUNT + 1;
const PAGE_SHIFT: usize = 12;
const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
const SLAB_MAX_UPSEARCH_SIZE: usize = 16; // When SLAB_32 is full, 18B is not allowed in SLAB_64

#[derive(Copy, Clone)]
pub struct Slab {
    block_size: usize,
    len: usize,
    min_len: usize,
    free_block_list: SinglyLinkedList<usize>,
    #[cfg(debug_slab)]
    start_addr: usize,
    #[cfg(debug_slab)]
    end_addr: usize,
}

impl Slab {
    /// Create an empty heap
    pub const fn new() -> Self {
        Slab {
            block_size: 0,
            len: 0,
            min_len: 0,
            free_block_list: SinglyLinkedList::new(),
        }
    }

    pub unsafe fn init(&mut self, start_addr: usize, count: usize, block_size: usize) {
        self.block_size = block_size;
        for i in (0..count).rev() {
            let new_block = (start_addr + i * block_size) as *mut usize;
            self.free_block_list.push(NonNull::new_unchecked(new_block));
        }

        self.len = count;
        self.min_len = count;
    }

    pub fn allocate(&mut self, _layout: &Layout) -> Option<NonNull<u8>> {
        match self.free_block_list.pop() {
            Some(block) => {
                self.len -= 1;
                self.min_len = core::cmp::min(self.min_len, self.len);
                let ptr = block.as_ptr();
                // clear the magic number
                let magic_ptr = ptr.wrapping_add(1);
                // Safety: magic_ptr is not null.
                unsafe { *magic_ptr = 0 };
                // Safety: ptr is not null.
                Some(unsafe { NonNull::new_unchecked(ptr as *mut u8) })
            }
            None => None, //Err(AllocErr)
        }
    }

    /// Safety: ptr must have been previously allocated by self.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>) {
        // Since ptr was allocated by self, its alignment must be at least
        // the alignment of FreeBlock. Casting a less aligned pointer to
        // &mut FreeBlock would be undefined behavior.
        #[cfg_attr(feature = "clippy", allow(clippy::cast_ptr_alignment))]
        let ptr = ptr.as_ptr() as *mut usize;

        let magic_ptr = ptr.wrapping_add(1);
        if *magic_ptr == 0xdeadbeef {
            panic!("Double free detected")
        }
        self.free_block_list.push(NonNull::new_unchecked(ptr));
        ptr::write(magic_ptr, 0xdeadbeef);
        self.len += 1;
    }
}

pub struct SlabHeap<
    const SLAB_8: usize,
    const SLAB_16: usize,
    const SLAB_32: usize,
    const SLAB_64: usize,
    const SLAB_96: usize,
    const SLAB_128: usize,
    const SLAB_192: usize,
    const SLAB_256: usize,
    const SLAB_512: usize,
    const SLAB_1024: usize,
> {
    pub slab_allocator: [Slab; SLAB_ALLOCATOR_COUNT],
    pub system_allocator: tlsf::TlsfHeap,
    pub slab_begin_addr: usize,
    pub slab_total_size: usize,
    // statistics
    allocated: usize,
    maximum: usize,
    total: usize,
}

impl<
        const SLAB_8: usize,
        const SLAB_16: usize,
        const SLAB_32: usize,
        const SLAB_64: usize,
        const SLAB_96: usize,
        const SLAB_128: usize,
        const SLAB_192: usize,
        const SLAB_256: usize,
        const SLAB_512: usize,
        const SLAB_1024: usize,
    >
    SlabHeap<
        SLAB_8,
        SLAB_16,
        SLAB_32,
        SLAB_64,
        SLAB_96,
        SLAB_128,
        SLAB_192,
        SLAB_256,
        SLAB_512,
        SLAB_1024,
    >
{
    // Constants for slab boundaries
    const SLAB_SIZES: [usize; SLAB_ALLOCATOR_COUNT] = [8, 16, 32, 64, 96, 128, 192, 256, 512, 1024];
    const SLAB_PAGE_COUNT: [usize; SLAB_ALLOCATOR_COUNT] = [
        SLAB_8, SLAB_16, SLAB_32, SLAB_64, SLAB_96, SLAB_128, SLAB_192, SLAB_256, SLAB_512,
        SLAB_1024,
    ];
    const SLAB_PAGE_END: [usize; SLAB_ALLOCATOR_COUNT] = Self::get_slab_page_end();
    const SLAB_COUNT: [usize; SLAB_ALLOCATOR_COUNT] = Self::get_slab_count();

    const fn get_slab_page_end() -> [usize; SLAB_ALLOCATOR_COUNT] {
        let mut result = [0; SLAB_ALLOCATOR_COUNT];
        result[0] = Self::SLAB_PAGE_COUNT[0];
        let mut i = 1;
        while i < SLAB_ALLOCATOR_COUNT {
            result[i] = result[i - 1] + Self::SLAB_PAGE_COUNT[i];
            i += 1;
        }
        result
    }

    const fn get_slab_count() -> [usize; SLAB_ALLOCATOR_COUNT] {
        let mut result = [0; SLAB_ALLOCATOR_COUNT];
        let mut i = 0;
        while i < SLAB_ALLOCATOR_COUNT {
            result[i] = (Self::SLAB_PAGE_COUNT[i] << PAGE_SHIFT) / Self::SLAB_SIZES[i];
            i += 1;
        }
        result
    }

    /// Create an empty heap
    pub const fn new() -> Self {
        Self {
            slab_allocator: [const { Slab::new() }; SLAB_ALLOCATOR_COUNT],
            system_allocator: tlsf::TlsfHeap::new(),
            slab_begin_addr: 0,
            slab_total_size: 0,
            allocated: 0,
            maximum: 0,
            total: 0,
        }
    }

    /// Add a range of memory [start, start+size) to the heap
    pub unsafe fn init(&mut self, start: usize, size: usize) {
        let block = crate::support::nonnull_slice_from_raw_parts(
            NonNull::new_unchecked(start as *mut u8),
            size,
        );
        self.system_allocator.insert_free_block_ptr(block);
        self.total = size;

        // allocate slabs
        self.slab_total_size = (SLAB_8
            + SLAB_16
            + SLAB_32
            + SLAB_64
            + SLAB_96
            + SLAB_128
            + SLAB_192
            + SLAB_256
            + SLAB_512
            + SLAB_1024)
            * PAGE_SIZE;
        debug_assert!(self.slab_total_size < size);
        let slab_layout = Layout::from_size_align(self.slab_total_size, PAGE_SIZE).unwrap();
        let slab_ptr = self.system_allocator.allocate(&slab_layout).unwrap();

        // init slabs
        let mut start_addr = slab_ptr.as_ptr() as usize;
        self.slab_begin_addr = start_addr;
        for i in 0..SLAB_ALLOCATOR_COUNT {
            self.slab_allocator[i].init(start_addr, Self::SLAB_COUNT[i], Self::SLAB_SIZES[i]);
            start_addr += Self::SLAB_PAGE_COUNT[i] * PAGE_SIZE;
        }
    }

    pub fn allocate(&mut self, layout: &Layout) -> Option<NonNull<u8>> {
        let mut ptr = None;
        let mut allocator_index = Self::layout_to_allocator(layout.size(), layout.align());
        while ptr.is_none() {
            if allocator_index < SLAB_ALLOCATOR_COUNT {
                if self.slab_allocator[allocator_index].len > 0
                    && Self::SLAB_SIZES[allocator_index].is_multiple_of(layout.align())
                {
                    ptr = self.slab_allocator[allocator_index].allocate(layout);
                    if ptr.is_some() {
                        self.allocated += Self::SLAB_SIZES[allocator_index];
                    }
                } else if Self::SLAB_SIZES[allocator_index] <= SLAB_MAX_UPSEARCH_SIZE {
                    allocator_index += 1;
                } else {
                    allocator_index = SYSTEM_ALLOCATOR_INDEX;
                }
            } else {
                ptr = self.system_allocator.allocate(layout);
                if ptr.is_some() {
                    // Update allocated size for system allocator
                    self.allocated += unsafe {
                        used_block_hdr_for_allocation_unknown_align(ptr.unwrap())
                            .unwrap()
                            .cast::<BlockHdr>()
                            .as_ref()
                            .size
                            & !SIZE_USED
                    };
                } else {
                    // Log allocation failure for debugging.
                    break;
                }
            }
        }

        // Update maximum usage
        if ptr.is_some() {
            self.maximum = core::cmp::max(self.maximum, self.allocated);
        }

        ptr
    }

    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: &Layout) -> usize {
        let allocator_index = self.ptr_to_allocator(ptr.as_ptr() as usize);
        if allocator_index >= SLAB_ALLOCATOR_COUNT {
            let size = self.system_allocator.deallocate(ptr, layout.align());
            self.allocated = self.allocated.saturating_sub(size);
            size
        } else {
            self.slab_allocator[allocator_index].deallocate(ptr);
            self.allocated = self
                .allocated
                .saturating_sub(Self::SLAB_SIZES[allocator_index]);
            Self::SLAB_SIZES[allocator_index]
        }
    }

    pub unsafe fn deallocate_unknown_align(&mut self, ptr: NonNull<u8>) -> usize {
        let allocator_index = self.ptr_to_allocator(ptr.as_ptr() as usize);
        if allocator_index >= SLAB_ALLOCATOR_COUNT {
            let size = self.system_allocator.deallocate_unknown_align(ptr);
            self.allocated = self.allocated.saturating_sub(size);
            size
        } else {
            self.slab_allocator[allocator_index].deallocate(ptr);
            self.allocated = self
                .allocated
                .saturating_sub(Self::SLAB_SIZES[allocator_index]);
            Self::SLAB_SIZES[allocator_index]
        }
    }

    pub unsafe fn reallocate(
        &mut self,
        ptr: NonNull<u8>,
        new_layout: &Layout,
    ) -> Option<NonNull<u8>> {
        let allocator_index = self.ptr_to_allocator(ptr.as_ptr() as usize);
        if allocator_index >= SLAB_ALLOCATOR_COUNT {
            let old_size = self.system_allocator.size_of_allocation(ptr)?;
            let new_ptr = self.system_allocator.reallocate(ptr, new_layout)?;
            let new_size = self.system_allocator.size_of_allocation(new_ptr)?;
            self.allocated = self.allocated - old_size + new_size;
            self.maximum = core::cmp::max(self.maximum, self.allocated);
            Some(new_ptr)
        } else {
            let block_size = Self::SLAB_SIZES[allocator_index];
            if new_layout.size() <= block_size {
                return Some(ptr);
            }
            let new_ptr = self.allocate(new_layout)?;
            core::ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), block_size);
            self.deallocate(ptr, new_layout);
            Some(new_ptr)
        }
    }

    pub unsafe fn reallocate_unknown_align(
        &mut self,
        ptr: NonNull<u8>,
        new_size: usize,
    ) -> Option<NonNull<u8>> {
        let allocator_index = self.ptr_to_allocator(ptr.as_ptr() as usize);
        if allocator_index >= SLAB_ALLOCATOR_COUNT {
            let old_size = self.system_allocator.size_of_allocation(ptr)?;
            let new_ptr = self
                .system_allocator
                .reallocate_unknown_align(ptr, new_size)?;
            let new_size = self.system_allocator.size_of_allocation(new_ptr)?;
            self.allocated = self.allocated - old_size + new_size;
            self.maximum = core::cmp::max(self.maximum, self.allocated);
            Some(new_ptr)
        } else {
            let block_size = Self::SLAB_SIZES[allocator_index];
            if new_size <= block_size {
                return Some(ptr);
            }
            let new_layout = Layout::from_size_align_unchecked(new_size, mem::size_of::<usize>());
            let new_ptr = self.allocate(&new_layout)?;
            core::ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), block_size);
            self.deallocate(ptr, &new_layout);
            Some(new_ptr)
        }
    }

    // Finds the appropriate allocator based on layout size and alignment
    //
    // This function implements a best-fit strategy for slab allocation:
    // - For sizes > 1024 bytes, use the system allocator
    // - For smaller sizes, use the smallest slab that can accommodate both size and alignment
    fn layout_to_allocator(size: usize, align: usize) -> usize {
        let index = Self::SLAB_SIZES.partition_point(|index| index < &core::cmp::max(size, align));
        if index <= SLAB_ALLOCATOR_COUNT {
            index
        } else {
            SLAB_ALLOCATOR_COUNT
        }
    }

    fn ptr_to_allocator(&mut self, ptr: usize) -> usize {
        if ptr < self.slab_begin_addr {
            return SYSTEM_ALLOCATOR_INDEX;
        }
        let offset = ptr - self.slab_begin_addr;
        let slab_index = offset >> PAGE_SHIFT;

        // Return SYSTEM_ALLOCATOR_INDEX if slab_index is not in SLAB_PAGE_END range
        Self::SLAB_PAGE_END.partition_point(|pos| pos <= &slab_index)
    }

    // Return the number of bytes that maximum used
    pub fn maximum(&self) -> usize {
        self.maximum
    }

    // Return the number of bytes that are actually allocated
    pub fn allocated(&self) -> usize {
        self.allocated
    }

    // Return the total number of bytes in the heap
    pub fn total(&self) -> usize {
        self.total
    }

    pub fn print_slab_stat(&self) {
        log::info!("size   total  free   max    alloc ");
        log::info!("------ ------ ------ ------ ------");
        for i in 0..SLAB_ALLOCATOR_COUNT {
            if Self::SLAB_PAGE_COUNT[i] != 0 {
                log::info!(
                    "{:6} {:6} {:6} {:6} {:6}",
                    Self::SLAB_SIZES[i],
                    Self::SLAB_COUNT[i],
                    self.slab_allocator[i].len,
                    Self::SLAB_COUNT[i] - self.slab_allocator[i].min_len,
                    Self::SLAB_COUNT[i] - self.slab_allocator[i].len
                );
            }
        }
    }

    pub fn size_of_allocation(&self, ptr: NonNull<u8>) -> Option<usize> {
        let raw_ptr = ptr.as_ptr() as usize;
        if raw_ptr < self.slab_begin_addr {
            return self.system_allocator.size_of_allocation(ptr);
        }
        let offset = raw_ptr - self.slab_begin_addr;
        let slab_index = offset >> PAGE_SHIFT;

        let pos = Self::SLAB_PAGE_END.partition_point(|pos| pos <= &slab_index);
        if pos < SLAB_ALLOCATOR_COUNT {
            return Some(Self::SLAB_SIZES[pos]);
        }
        self.system_allocator.size_of_allocation(ptr)
    }

    pub fn get_max_free_block_size(&self) -> usize {
        let max_free = self.system_allocator.get_max_free_block_size();
        if max_free > Self::SLAB_SIZES[SLAB_ALLOCATOR_COUNT - 1] {
            return max_free;
        }
        for i in (0..SLAB_ALLOCATOR_COUNT).rev() {
            if self.slab_allocator[i].len > 0 {
                return core::cmp::max(max_free, Self::SLAB_SIZES[i]);
            }
        }
        max_free
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dynamic Slab Allocator
// ═══════════════════════════════════════════════════════════════════════════

// ── Constants ───────────────────────────────────────────────────────────────

#[cfg(allocator = "slab_dynamic")]
const PAGE_MAGIC: u32 = 0x534C_4142; // "SLAB"

#[cfg(allocator = "slab_dynamic")]
const NULL_PTR: usize = 0;

#[cfg(allocator = "slab_dynamic")]
const MEMORY_PRESSURE_THRESHOLD: usize = 128 * 1024; // 128 KB

#[cfg(allocator = "slab_dynamic")]
const DEFAULT_MAX_TOTAL_PAGES: usize = blueos_kconfig::CONFIG_SLAB_DYNAMIC_MAX_TOTAL_PAGES as usize;

/// Requests up to and including this size stay in dynamic slab. Larger
/// requests are forwarded to buddy.
#[cfg(allocator = "slab_dynamic")]
pub const DYNAMIC_SLAB_MAX_SIZE: usize = 2048;

/// One page transferred from a page provider to the dynamic slab allocator.
///
/// `index` identifies the entry in the external [`PageMetadata`] array.  It
/// must agree with the page address and the mapping supplied to
/// [`DynamicSlabHeap::init_with_metadata`].
#[cfg(allocator = "slab_dynamic")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlabPage {
    pub address: NonNull<u8>,
    pub index: usize,
}

/// Supplies page-sized backing storage to [`DynamicSlabHeap`].
///
/// # Safety
///
/// Implementations must return unique, writable, `PAGE_SIZE`-aligned pages
/// from the address/index range configured for the heap. A released page must
/// be accepted exactly once and must not be made available elsewhere until
/// `release_page` is called.
#[cfg(allocator = "slab_dynamic")]
pub unsafe trait SlabPageProvider {
    fn acquire_page(&mut self) -> Option<SlabPage>;

    /// # Safety
    ///
    /// `page` must be a live page previously returned by this provider and
    /// must no longer contain live slab objects.
    unsafe fn release_page(&mut self, page: SlabPage);
}

#[cfg(allocator = "slab_dynamic")]
#[derive(Clone, Copy)]
enum DynamicAllocationKind {
    Slab(usize),
    Buddy(usize),
}

#[cfg(allocator = "slab_dynamic")]
impl DynamicAllocationKind {
    fn capacity(self) -> usize {
        match self {
            Self::Slab(size) => size,
            Self::Buddy(order) => PAGE_SIZE << order,
        }
    }
}

/// Initialization failures for provider-backed dynamic slab metadata.
#[cfg(allocator = "slab_dynamic")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicSlabInitError {
    ZeroPages,
    MetadataLayoutOverflow,
    MetadataMisaligned,
    MetadataTooSmall { required: usize, provided: usize },
    PageBaseMisaligned,
    Buddy(BuddyInitError),
}

#[cfg(allocator = "slab_dynamic")]
impl From<BuddyInitError> for DynamicSlabInitError {
    fn from(error: BuddyInitError) -> Self {
        Self::Buddy(error)
    }
}

// ── External Metadata Helper Functions ──────────────────────────────────────

/// Map page address to metadata index
#[cfg(allocator = "slab_dynamic")]
#[inline]
fn page_to_meta_index(page_addr: usize, heap_base: usize) -> usize {
    (page_addr - heap_base) / PAGE_SIZE
}

/// Get metadata for a page address
#[cfg(allocator = "slab_dynamic")]
#[inline]
unsafe fn get_page_meta(
    page_addr: usize,
    metadata: *mut PageMetadata,
    heap_base: usize,
) -> &'static mut PageMetadata {
    let idx = page_to_meta_index(page_addr, heap_base);
    &mut *metadata.add(idx)
}

// ── PageMetadata ─────────────────────────────────────────────────────────────

/// Metadata for a slab page, stored externally in SLAB_METADATA array.
/// No longer embedded in the page itself, allowing full page for data blocks.
///
/// Fields:
///   page_magic   — PAGE_MAGIC when page is owned by a DynamicSlab
///   slab_index   — index into SLAB_SIZES[] (0..SLAB_ALLOCATOR_COUNT)
///   free_blocks  — number of free blocks currently in this page
///   total_blocks — total blocks in this page (constant after init)
///   free_head    — address of first free block; 0 = empty
///   next_page    — index of next page in list; MAX_SLAB_PAGES = end
///   prev_page    — index of prev page in list; MAX_SLAB_PAGES = end
#[cfg(allocator = "slab_dynamic")]
#[repr(C)]
struct PageMetadata {
    page_magic: u32,
    slab_index: u8,
    _pad: [u8; 3],
    free_blocks: u32,
    total_blocks: u32,
    free_head: usize,
    next_page: usize,
    prev_page: usize,
}

#[cfg(allocator = "slab_dynamic")]
impl PageMetadata {
    const fn new() -> Self {
        Self {
            page_magic: 0,
            slab_index: 0,
            _pad: [0; 3],
            free_blocks: 0,
            total_blocks: 0,
            free_head: 0,
            next_page: usize::MAX,
            prev_page: usize::MAX,
        }
    }
}

// ── Index-based List Operations ──────────────────────────────────────────────

/// Push page to front of list
#[cfg(allocator = "slab_dynamic")]
unsafe fn list_push_front(
    head_idx: &mut usize,
    page_idx: usize,
    metadata: *mut PageMetadata,
    max_pages: usize,
) {
    let meta = &mut *metadata.add(page_idx);
    meta.next_page = *head_idx;
    meta.prev_page = max_pages;

    if *head_idx != max_pages {
        (*metadata.add(*head_idx)).prev_page = page_idx;
    }
    *head_idx = page_idx;
}

/// Remove page from list
#[cfg(allocator = "slab_dynamic")]
unsafe fn list_remove(
    head_idx: &mut usize,
    page_idx: usize,
    metadata: *mut PageMetadata,
    max_pages: usize,
) {
    let meta = &*metadata.add(page_idx);
    let next = meta.next_page;
    let prev = meta.prev_page;

    if prev != max_pages {
        (*metadata.add(prev)).next_page = next;
    } else {
        *head_idx = next;
    }

    if next != max_pages {
        (*metadata.add(next)).prev_page = prev;
    }
}

/// Pop page from front of list
#[cfg(allocator = "slab_dynamic")]
unsafe fn list_pop_front(
    head_idx: &mut usize,
    metadata: *mut PageMetadata,
    max_pages: usize,
) -> Option<usize> {
    if *head_idx == max_pages {
        return None;
    }
    let page_idx = *head_idx;
    list_remove(head_idx, page_idx, metadata, max_pages);
    Some(page_idx)
}

/// Byte offset from page base where the first allocatable block starts.
/// With external metadata, the entire page is available for data blocks.
#[cfg(allocator = "slab_dynamic")]
fn page_data_offset(_block_size: usize) -> usize {
    0
}

/// Number of allocatable blocks that fit in a single PAGE_SIZE page.
/// With external metadata, we can use the full page.
#[cfg(allocator = "slab_dynamic")]
fn blocks_per_page(block_size: usize) -> usize {
    PAGE_SIZE / block_size
}

/// Check whether `ptr` lives inside a dynamically managed slab page.
///
/// Returns `Some(slab_index)` when the page's magic matches, `None` otherwise.
/// With external metadata, we look up the metadata array.
///
/// # Safety
/// The page-aligned address is always readable in kernel context.
#[cfg(allocator = "slab_dynamic")]
fn ptr_is_slab(
    ptr: usize,
    metadata: *mut PageMetadata,
    heap_base: usize,
    max_pages: usize,
) -> Option<u8> {
    let page_base = ptr & !(PAGE_SIZE - 1);
    assert!(
        heap_base != 0 && page_base >= heap_base,
        "invalid ptr {:#x}, heap_base {:#x} or page_base {:#x}",
        ptr,
        heap_base,
        page_base
    );
    unsafe {
        let page_idx = page_to_meta_index(page_base, heap_base);
        if page_idx >= max_pages {
            return None;
        }
        let meta = &*metadata.add(page_idx);
        if meta.page_magic == PAGE_MAGIC {
            Some(meta.slab_index)
        } else {
            None
        }
    }
}

// ── DynamicSlab ──────────────────────────────────────────────────────────────

#[cfg(allocator = "slab_dynamic")]
struct DynamicSlab {
    block_size: usize,
    page_list_head: usize, // Index into metadata array, max_pages = empty
    total_blocks: usize,
    free_blocks: usize,
}

#[cfg(allocator = "slab_dynamic")]
impl DynamicSlab {
    const fn new() -> Self {
        DynamicSlab {
            block_size: 0,
            page_list_head: usize::MAX,
            total_blocks: 0,
            free_blocks: 0,
        }
    }

    fn set_block_size(&mut self, block_size: usize) {
        self.block_size = block_size;
    }

    /// Format a fresh PAGE_SIZE-aligned page as a slab page and prepend to page list.
    ///
    /// # Safety
    /// `page_addr` must be PAGE_SIZE-aligned and point to PAGE_SIZE writable bytes.
    unsafe fn init_page(
        &mut self,
        page_addr: usize,
        slab_index: u8,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) {
        let offset = page_data_offset(self.block_size);
        let total = blocks_per_page(self.block_size);

        // Build intrusive free list
        let mut prev_free: usize = NULL_PTR;
        for i in (0..total).rev() {
            let block_addr = page_addr + offset + i * self.block_size;
            *(block_addr as *mut usize) = prev_free;
            prev_free = block_addr;
        }

        // Get external metadata
        let meta = get_page_meta(page_addr, metadata, heap_base);
        meta.page_magic = PAGE_MAGIC;
        meta.slab_index = slab_index;
        meta._pad = [0u8; 3];
        meta.free_blocks = total as u32;
        meta.total_blocks = total as u32;
        meta.free_head = if total > 0 {
            page_addr + offset
        } else {
            NULL_PTR
        };
        meta.next_page = max_pages;
        meta.prev_page = max_pages;

        // Add to page list
        let page_idx = page_to_meta_index(page_addr, heap_base);
        if page_idx >= max_pages {
            panic!(
                "init_page: page_idx {} >= max_pages {}, page_addr={:#x}, heap_base={:#x}",
                page_idx, max_pages, page_addr, heap_base
            );
        }
        list_push_front(&mut self.page_list_head, page_idx, metadata, max_pages);
        self.total_blocks += total;
        self.free_blocks += total;
    }

    /// Add an already-initialized page back to this slab (from PagePool).
    /// The page must have been previously initialized for this slab type and be fully free.
    unsafe fn add_page(
        &mut self,
        page_addr: usize,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) {
        let meta = get_page_meta(page_addr, metadata, heap_base);
        debug_assert_eq!(
            meta.free_blocks, meta.total_blocks,
            "Page from pool must be fully free"
        );
        let page_idx = page_to_meta_index(page_addr, heap_base);
        if page_idx >= max_pages {
            panic!(
                "add_page: page_idx {} >= max_pages {}, page_addr={:#x}, heap_base={:#x}",
                page_idx, max_pages, page_addr, heap_base
            );
        }
        list_push_front(&mut self.page_list_head, page_idx, metadata, max_pages);
        self.total_blocks += meta.total_blocks as usize;
        self.free_blocks += meta.total_blocks as usize;
    }

    /// Allocate one block from the first page that has free blocks.
    /// Returns `None` if all pages are full (caller must add a new page first).
    unsafe fn allocate_block(
        &mut self,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) -> Option<NonNull<u8>> {
        let mut page_idx = self.page_list_head;
        while page_idx != max_pages {
            let meta = &*metadata.add(page_idx);
            if meta.free_blocks > 0 {
                let page_addr = heap_base + page_idx * PAGE_SIZE;
                return Some(self.pop_from_page(page_addr, metadata, heap_base));
            }
            page_idx = meta.next_page;
        }
        None
    }

    /// Pop one block from the free list of the given page.
    ///
    /// # Safety
    /// `page_addr` must be an active slab page with `free_blocks > 0`.
    unsafe fn pop_from_page(
        &mut self,
        page_addr: usize,
        metadata: *mut PageMetadata,
        heap_base: usize,
    ) -> NonNull<u8> {
        let meta = get_page_meta(page_addr, metadata, heap_base);
        let block_addr = meta.free_head;
        debug_assert_ne!(block_addr, NULL_PTR, "pop_from_page called on empty page");

        let next_free = *(block_addr as *const usize);
        meta.free_head = next_free;
        meta.free_blocks -= 1;
        self.free_blocks -= 1;

        NonNull::new_unchecked(block_addr as *mut u8)
    }

    /// Push `ptr` back onto its page's free list.
    ///
    /// Returns `(page_addr, page_is_now_fully_free)`.
    ///
    /// # Safety
    /// `ptr` must be a block previously allocated from this `DynamicSlab`.
    unsafe fn free_block(
        &mut self,
        ptr: NonNull<u8>,
        metadata: *mut PageMetadata,
        heap_base: usize,
    ) -> (usize, bool) {
        let ptr_addr = ptr.as_ptr() as usize;
        let page_addr = ptr_addr & !(PAGE_SIZE - 1);
        let meta = get_page_meta(page_addr, metadata, heap_base);

        debug_assert_eq!(meta.page_magic, PAGE_MAGIC);
        debug_assert!(
            meta.free_blocks < meta.total_blocks,
            "double-free detected at {:p}",
            ptr.as_ptr()
        );

        *(ptr.as_ptr() as *mut usize) = meta.free_head;
        meta.free_head = ptr_addr;
        meta.free_blocks += 1;
        self.free_blocks += 1;

        let fully_free = meta.free_blocks == meta.total_blocks;
        (page_addr, fully_free)
    }

    /// Unlink `page_addr` from the intrusive page list and update aggregate counters.
    ///
    /// Clears `page_magic` so `ptr_is_slab` returns `None` for this page afterwards.
    ///
    /// # Safety
    /// `page_addr` must be currently in `self`'s page list.
    unsafe fn remove_page(
        &mut self,
        page_addr: usize,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) {
        let meta = get_page_meta(page_addr, metadata, heap_base);
        let total = meta.total_blocks as usize;

        // Remove from list
        let page_idx = page_to_meta_index(page_addr, heap_base);
        list_remove(&mut self.page_list_head, page_idx, metadata, max_pages);

        self.total_blocks -= total;
        self.free_blocks -= total;
        meta.page_magic = 0;
    }
}

// ── PagePool ─────────────────────────────────────────────────────────────────

/// SLUB-style page pool: caches fully-free slab pages to reduce system pressure.
/// Uses per-slab intrusive linked lists for O(1) same-type page reuse.
#[cfg(allocator = "slab_dynamic")]
struct PagePool {
    lists: [usize; DYNAMIC_SLAB_ALLOCATOR_COUNT], // Head indices, max_pages = empty
    total_pages: usize,
    max_total_pages: usize,
}

#[cfg(allocator = "slab_dynamic")]
impl PagePool {
    const fn new() -> Self {
        PagePool {
            lists: [usize::MAX; DYNAMIC_SLAB_ALLOCATOR_COUNT],
            total_pages: 0,
            max_total_pages: DEFAULT_MAX_TOTAL_PAGES,
        }
    }

    /// Try to release a fully-free page into the pool.
    ///
    /// # Safety
    /// `page_addr` must be PAGE_SIZE-aligned and no longer in any `DynamicSlab`'s list.
    unsafe fn release_page(
        &mut self,
        page_addr: usize,
        slab_index: usize,
        system_allocator: &mut tlsf::TlsfHeap,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) {
        if self.total_pages >= self.max_total_pages {
            // Pool full - deallocate page back to system
            system_allocator.deallocate(NonNull::new_unchecked(page_addr as *mut u8), PAGE_SIZE);
            return;
        }

        let page_idx = page_to_meta_index(page_addr, heap_base);
        assert!(
            page_idx < max_pages,
            "addr {} page_idx {} out of bounds",
            page_addr,
            page_idx
        );
        let meta = get_page_meta(page_addr, metadata, heap_base);
        meta.slab_index = slab_index as u8;
        list_push_front(&mut self.lists[slab_index], page_idx, metadata, max_pages);
        meta.page_magic = PAGE_MAGIC; // Mark as valid slab page for reuse
        self.total_pages += 1;
    }

    /// Cache a page or return it to an external page provider when the cache
    /// is full.
    unsafe fn release_page_to<P: SlabPageProvider>(
        &mut self,
        page_addr: usize,
        slab_index: usize,
        provider: &mut P,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) {
        let page_idx = page_to_meta_index(page_addr, heap_base);
        assert!(page_idx < max_pages, "slab page index out of bounds");

        if self.total_pages >= self.max_total_pages {
            let meta = &mut *metadata.add(page_idx);
            meta.page_magic = 0;
            meta.next_page = max_pages;
            meta.prev_page = max_pages;
            provider.release_page(SlabPage {
                address: NonNull::new_unchecked(page_addr as *mut u8),
                index: page_idx,
            });
            return;
        }

        let meta = get_page_meta(page_addr, metadata, heap_base);
        meta.slab_index = slab_index as u8;
        list_push_front(&mut self.lists[slab_index], page_idx, metadata, max_pages);
        meta.page_magic = PAGE_MAGIC;
        self.total_pages += 1;
    }

    /// Take one page from the pool, preferring same slab_index.
    ///
    /// # Safety
    /// Caller must immediately call `DynamicSlab::init_page` on the returned page.
    /// Returns (page_addr, needs_init): needs_init=true if page is from different slab type
    unsafe fn take_page(
        &mut self,
        slab_index: usize,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) -> Option<(usize, bool)> {
        // Try same slab_index first - no init needed
        if let Some(page_idx) = list_pop_front(&mut self.lists[slab_index], metadata, max_pages) {
            debug_assert!(page_idx < max_pages, "page_idx out of bounds");
            let addr = heap_base.checked_add(page_idx.checked_mul(PAGE_SIZE)?)?;
            self.total_pages -= 1;
            return Some((addr, false));
        }

        // Fallback: take from any other slab - needs reinit
        for i in 0..DYNAMIC_SLAB_ALLOCATOR_COUNT {
            if let Some(page_idx) = list_pop_front(&mut self.lists[i], metadata, max_pages) {
                debug_assert!(page_idx < max_pages, "page_idx out of bounds");
                let addr = heap_base.checked_add(page_idx.checked_mul(PAGE_SIZE)?)?;
                self.total_pages -= 1;
                return Some((addr, true));
            }
        }

        None
    }

    /// Reclaim up to `pages_needed` pages from the pool and return them to system.
    ///
    /// # Safety
    /// `system` must be the same allocator from which the pages were originally obtained.
    unsafe fn reclaim_to_system(
        &mut self,
        system_allocator: &mut tlsf::TlsfHeap,
        pages_needed: usize,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) -> usize {
        let mut reclaimed = 0;

        while reclaimed < pages_needed {
            let mut found = false;
            for i in 0..DYNAMIC_SLAB_ALLOCATOR_COUNT {
                if let Some(page_idx) = list_pop_front(&mut self.lists[i], metadata, max_pages) {
                    debug_assert!(page_idx < max_pages, "page_idx out of bounds");
                    let page_addr = heap_base + page_idx * PAGE_SIZE;
                    unsafe {
                        let meta = &mut *metadata.add(page_idx);
                        meta.page_magic = 0;
                        system_allocator
                            .deallocate(NonNull::new_unchecked(page_addr as *mut u8), PAGE_SIZE);
                    }
                    self.total_pages -= 1;
                    reclaimed += 1;
                    found = true;
                    break;
                }
            }
            if !found {
                break;
            }
        }
        reclaimed
    }

    /// Reclaim up to `pages_needed` cached pages to an external provider.
    unsafe fn reclaim_to_provider<P: SlabPageProvider>(
        &mut self,
        provider: &mut P,
        pages_needed: usize,
        metadata: *mut PageMetadata,
        heap_base: usize,
        max_pages: usize,
    ) -> usize {
        let mut reclaimed = 0;

        while reclaimed < pages_needed {
            let mut page_idx = None;
            for i in 0..DYNAMIC_SLAB_ALLOCATOR_COUNT {
                if let Some(found) = list_pop_front(&mut self.lists[i], metadata, max_pages) {
                    page_idx = Some(found);
                    break;
                }
            }

            let Some(page_idx) = page_idx else {
                break;
            };
            debug_assert!(page_idx < max_pages);
            let page_addr = heap_base + page_idx * PAGE_SIZE;
            let meta = &mut *metadata.add(page_idx);
            meta.page_magic = 0;
            meta.next_page = max_pages;
            meta.prev_page = max_pages;
            provider.release_page(SlabPage {
                address: NonNull::new_unchecked(page_addr as *mut u8),
                index: page_idx,
            });
            self.total_pages -= 1;
            reclaimed += 1;
        }
        reclaimed
    }
}

// ── DynamicSlabHeap ───────────────────────────────────────────────────────────

#[cfg(allocator = "slab_dynamic")]
type SlabStatData = [(usize, usize, usize, usize); DYNAMIC_SLAB_ALLOCATOR_COUNT];

#[cfg(allocator = "slab_dynamic")]
pub struct DynamicSlabHeap {
    slabs: [DynamicSlab; DYNAMIC_SLAB_ALLOCATOR_COUNT],
    page_pool: PagePool,
    system_allocator: tlsf::TlsfHeap,
    buddy: BuddyAllocator,
    buddy_enabled: bool,
    allocated: usize,
    maximum: usize,
    total: usize,
    // External metadata
    metadata: *mut PageMetadata,
    heap_base: usize,
    max_pages: usize,
}

#[cfg(allocator = "slab_dynamic")]
impl DynamicSlabHeap {
    const SLAB_SIZES: [usize; DYNAMIC_SLAB_ALLOCATOR_COUNT] = [
        8,
        16,
        32,
        64,
        96,
        128,
        192,
        256,
        512,
        1024,
        DYNAMIC_SLAB_MAX_SIZE,
    ];

    pub const fn new() -> Self {
        DynamicSlabHeap {
            slabs: [const { DynamicSlab::new() }; DYNAMIC_SLAB_ALLOCATOR_COUNT],
            page_pool: PagePool::new(),
            system_allocator: tlsf::TlsfHeap::new(),
            buddy: BuddyAllocator::new(),
            buddy_enabled: false,
            allocated: 0,
            maximum: 0,
            total: 0,
            metadata: core::ptr::null_mut(),
            heap_base: 0,
            max_pages: 0,
        }
    }

    /// Return the external metadata layout for `page_count` addressable pages.
    pub fn metadata_layout(page_count: usize) -> Result<Layout, DynamicSlabInitError> {
        if page_count == 0 {
            return Err(DynamicSlabInitError::ZeroPages);
        }
        Layout::array::<PageMetadata>(page_count)
            .map_err(|_| DynamicSlabInitError::MetadataLayoutOverflow)
    }

    /// Initialize a dynamic slab whose data pages are supplied externally.
    ///
    /// `page_base` is the virtual address corresponding to metadata index zero.
    /// A page at `page_base + index * PAGE_SIZE` uses metadata entry `index`.
    ///
    /// # Safety
    ///
    /// The metadata buffer must be writable, exclusively owned by this heap,
    /// and live for the heap's lifetime. Pages subsequently returned by the
    /// provider must be writable and follow the configured address/index map.
    pub unsafe fn init_with_metadata(
        &mut self,
        metadata_ptr: NonNull<u8>,
        metadata_len: usize,
        page_base: usize,
        page_count: usize,
    ) -> Result<(), DynamicSlabInitError> {
        let layout = Self::metadata_layout(page_count)?;
        if (metadata_ptr.as_ptr() as usize) & (layout.align() - 1) != 0 {
            return Err(DynamicSlabInitError::MetadataMisaligned);
        }
        if metadata_len < layout.size() {
            return Err(DynamicSlabInitError::MetadataTooSmall {
                required: layout.size(),
                provided: metadata_len,
            });
        }
        if page_base & (PAGE_SIZE - 1) != 0 {
            return Err(DynamicSlabInitError::PageBaseMisaligned);
        }
        let total = page_count
            .checked_mul(PAGE_SIZE)
            .ok_or(DynamicSlabInitError::MetadataLayoutOverflow)?;

        self.metadata = metadata_ptr.as_ptr().cast::<PageMetadata>();
        self.heap_base = page_base;
        self.max_pages = page_count;
        self.allocated = 0;
        self.maximum = 0;
        self.total = total;
        self.buddy_enabled = false;
        self.page_pool.total_pages = 0;

        for i in 0..page_count {
            self.metadata.add(i).write(PageMetadata::new());
        }
        for i in 0..DYNAMIC_SLAB_ALLOCATOR_COUNT {
            self.page_pool.lists[i] = page_count;
            self.slabs[i].page_list_head = page_count;
            self.slabs[i].total_blocks = 0;
            self.slabs[i].free_blocks = 0;
            self.slabs[i].set_block_size(Self::SLAB_SIZES[i]);
        }
        Ok(())
    }

    /// Initialize dynamic slab with buddy as its page and large-allocation
    /// backend.
    ///
    /// Requests whose size or alignment exceeds [`DYNAMIC_SLAB_MAX_SIZE`] are
    /// served directly by buddy. Smaller requests are served by slab classes,
    /// whose pages are also acquired from buddy.
    ///
    /// # Safety
    ///
    /// Both metadata buffers must be writable and exclusively owned for the
    /// lifetime of this heap. `page_base..page_base + page_count * PAGE_SIZE`
    /// must be a writable, page-aligned mapping. `manageable_pages` must cover
    /// only pages not occupied by the kernel or either metadata buffer.
    pub unsafe fn init_with_buddy(
        &mut self,
        slab_metadata: NonNull<u8>,
        slab_metadata_len: usize,
        buddy_metadata: NonNull<u8>,
        buddy_metadata_len: usize,
        page_base: usize,
        page_count: usize,
        manageable_pages: core::ops::Range<usize>,
    ) -> Result<(), DynamicSlabInitError> {
        self.init_with_metadata(slab_metadata, slab_metadata_len, page_base, page_count)?;
        self.buddy.init(
            buddy_metadata,
            buddy_metadata_len,
            page_count,
            manageable_pages,
        )?;
        self.buddy_enabled = true;
        self.prewarm_with_buddy();
        self.update_buddy_peak();
        Ok(())
    }

    /// Initialize the heap with the memory range `[start, start+size)`.
    ///
    /// Unlike `SlabHeap`, no memory is pre-allocated for slabs. All slab pages
    /// are fetched lazily from TLSF on first demand.
    ///
    /// # Safety
    /// `start..start+size` must be valid, exclusively owned, writable memory.
    pub unsafe fn init(&mut self, start: usize, size: usize) {
        self.buddy_enabled = false;
        // Calculate metadata requirements
        self.heap_base = crate::support::align_up_size(start, PAGE_SIZE);
        self.max_pages = size / PAGE_SIZE;

        // Initialize TLSF with full heap
        let block = crate::support::nonnull_slice_from_raw_parts(
            NonNull::new_unchecked(start as *mut u8),
            size,
        );
        self.system_allocator.insert_free_block_ptr(block);
        self.total = size;

        // Allocate metadata array from TLSF
        let metadata_size = self.max_pages * core::mem::size_of::<PageMetadata>();
        let metadata_layout =
            Layout::from_size_align_unchecked(metadata_size, core::mem::align_of::<PageMetadata>());
        let metadata_ptr = self
            .system_allocator
            .allocate(&metadata_layout)
            .expect("Failed to allocate metadata");
        self.metadata = metadata_ptr.as_ptr() as *mut PageMetadata;

        // Initialize all metadata entries
        for i in 0..self.max_pages {
            self.metadata.add(i).write(PageMetadata::new());
        }

        // Initialize page_pool lists with max_pages as empty sentinel
        for i in 0..DYNAMIC_SLAB_ALLOCATOR_COUNT {
            self.page_pool.lists[i] = self.max_pages;
        }

        for i in 0..DYNAMIC_SLAB_ALLOCATOR_COUNT {
            self.slabs[i].page_list_head = self.max_pages;
            self.slabs[i].set_block_size(Self::SLAB_SIZES[i]);
        }
        self.prewarm_critical_slabs();
    }

    unsafe fn prewarm_critical_slabs(&mut self) {
        const PREWARM: &[(usize, usize)] = &[(0, 1), (1, 1), (2, 1), (3, 1), (4, 1), (5, 1)];
        for &(idx, count) in PREWARM {
            for _ in 0..count {
                if let Some((page, _needs_init)) = self.acquire_page(idx) {
                    self.slabs[idx].init_page(
                        page,
                        idx as u8,
                        self.metadata,
                        self.heap_base,
                        self.max_pages,
                    );
                } else {
                    return;
                }
            }
        }
    }

    // ── Allocation ─────────────────────────────────────────────────────────

    /// Return the slab index for the given `(size, align)`.
    /// Result is `DYNAMIC_SLAB_ALLOCATOR_COUNT` when no slab can satisfy the request.
    fn layout_to_slab_index(size: usize, align: usize) -> usize {
        Self::SLAB_SIZES
            .iter()
            .position(|&class_size| class_size >= size && class_size.is_multiple_of(align))
            .unwrap_or(DYNAMIC_SLAB_ALLOCATOR_COUNT)
    }

    /// Return whether this layout has a deterministic dynamic-slab class.
    pub fn supports_layout(layout: &Layout) -> bool {
        Self::layout_to_slab_index(layout.size(), layout.align()) < DYNAMIC_SLAB_ALLOCATOR_COUNT
    }

    /// Return the capacity of the dynamic-slab class selected for `layout`.
    pub fn class_size(layout: &Layout) -> Option<usize> {
        let index = Self::layout_to_slab_index(layout.size(), layout.align());
        if index < DYNAMIC_SLAB_ALLOCATOR_COUNT {
            Some(Self::SLAB_SIZES[index])
        } else {
            None
        }
    }

    /// Allocate from dynamic slab pages supplied by `provider`.
    ///
    /// Layouts without a slab class are rejected rather than falling back to a
    /// second allocator, keeping allocation and deallocation routing identical.
    pub fn allocate_with_provider<P: SlabPageProvider>(
        &mut self,
        layout: &Layout,
        provider: &mut P,
    ) -> Option<NonNull<u8>> {
        let index = Self::layout_to_slab_index(layout.size(), layout.align());
        if index >= DYNAMIC_SLAB_ALLOCATOR_COUNT {
            return None;
        }

        let ptr = unsafe {
            if self.slabs[index].free_blocks == 0 {
                let (page_addr, needs_init) = self.acquire_page_from(index, provider)?;
                if needs_init {
                    self.slabs[index].init_page(
                        page_addr,
                        index as u8,
                        self.metadata,
                        self.heap_base,
                        self.max_pages,
                    );
                } else {
                    self.slabs[index].add_page(
                        page_addr,
                        self.metadata,
                        self.heap_base,
                        self.max_pages,
                    );
                }
            }
            self.slabs[index].allocate_block(self.metadata, self.heap_base, self.max_pages)?
        };

        self.allocated += Self::SLAB_SIZES[index];
        self.maximum = core::cmp::max(self.maximum, self.allocated);
        Some(ptr)
    }

    unsafe fn acquire_page_from<P: SlabPageProvider>(
        &mut self,
        slab_index: usize,
        provider: &mut P,
    ) -> Option<(usize, bool)> {
        if let Some(page) =
            self.page_pool
                .take_page(slab_index, self.metadata, self.heap_base, self.max_pages)
        {
            return Some(page);
        }

        let page = provider.acquire_page()?;
        let page_addr = page.address.as_ptr() as usize;
        if page_addr & (PAGE_SIZE - 1) != 0
            || page.index >= self.max_pages
            || self
                .heap_base
                .checked_add(page.index.checked_mul(PAGE_SIZE)?)?
                != page_addr
        {
            debug_assert!(false, "page provider returned an invalid slab page");
            provider.release_page(page);
            return None;
        }
        debug_assert_eq!((*self.metadata.add(page.index)).page_magic, 0);
        Some((page_addr, true))
    }

    /// Pre-populate the latency-sensitive slab classes from `provider`.
    pub fn prewarm_with_provider<P: SlabPageProvider>(&mut self, provider: &mut P) {
        const PREWARM: &[(usize, usize)] = &[(0, 1), (1, 1), (2, 1), (3, 1), (4, 1), (5, 1)];
        unsafe {
            for &(index, count) in PREWARM {
                for _ in 0..count {
                    let Some((page_addr, _)) = self.acquire_page_from(index, provider) else {
                        return;
                    };
                    self.slabs[index].init_page(
                        page_addr,
                        index as u8,
                        self.metadata,
                        self.heap_base,
                        self.max_pages,
                    );
                }
            }
        }
    }

    fn buddy_orders(layout: &Layout) -> Option<(usize, usize)> {
        let order = order_of_size(layout.size().max(1));
        let align_order = if layout.align() <= PAGE_SIZE {
            0
        } else {
            layout.align().trailing_zeros() as usize - PAGE_SHIFT
        };
        (order <= MAX_ORDER && align_order <= MAX_ORDER).then_some((order, align_order))
    }

    fn pfn_to_pointer(&self, pfn: usize) -> Option<NonNull<u8>> {
        if pfn >= self.max_pages {
            return None;
        }
        let address = self.heap_base.checked_add(pfn.checked_mul(PAGE_SIZE)?)?;
        NonNull::new(address as *mut u8)
    }

    fn pointer_to_pfn(&self, ptr: NonNull<u8>) -> Option<usize> {
        let address = ptr.as_ptr() as usize;
        if address & (PAGE_SIZE - 1) != 0 {
            return None;
        }
        let pfn = address.checked_sub(self.heap_base)? >> PAGE_SHIFT;
        (pfn < self.max_pages).then_some(pfn)
    }

    fn update_buddy_peak(&mut self) {
        if self.buddy_enabled {
            self.maximum = self
                .maximum
                .max(self.buddy.memory_info().used_pages * PAGE_SIZE);
        }
    }

    unsafe fn acquire_page_from_buddy(&mut self, slab_index: usize) -> Option<(usize, bool)> {
        if let Some(page) =
            self.page_pool
                .take_page(slab_index, self.metadata, self.heap_base, self.max_pages)
        {
            return Some(page);
        }

        let frame = self.buddy.alloc_pages(0)?;
        let pfn = frame.pfn();
        match self.pfn_to_pointer(pfn) {
            Some(pointer) => Some((pointer.as_ptr() as usize, true)),
            None => {
                self.buddy.free_pages(frame, 0);
                None
            }
        }
    }

    fn allocate_slab_from_buddy(&mut self, layout: &Layout) -> Option<NonNull<u8>> {
        let index = Self::layout_to_slab_index(layout.size(), layout.align());
        if index >= DYNAMIC_SLAB_ALLOCATOR_COUNT {
            return None;
        }

        unsafe {
            if self.slabs[index].free_blocks == 0 {
                let (page_addr, needs_init) = self.acquire_page_from_buddy(index)?;
                if needs_init {
                    self.slabs[index].init_page(
                        page_addr,
                        index as u8,
                        self.metadata,
                        self.heap_base,
                        self.max_pages,
                    );
                } else {
                    self.slabs[index].add_page(
                        page_addr,
                        self.metadata,
                        self.heap_base,
                        self.max_pages,
                    );
                }
            }
            let pointer =
                self.slabs[index].allocate_block(self.metadata, self.heap_base, self.max_pages)?;
            self.allocated += Self::SLAB_SIZES[index];
            Some(pointer)
        }
    }

    fn prewarm_with_buddy(&mut self) {
        const PREWARM: &[(usize, usize)] = &[(0, 1), (1, 1), (2, 1), (3, 1), (4, 1), (5, 1)];
        unsafe {
            for &(index, count) in PREWARM {
                for _ in 0..count {
                    let Some((page_addr, _)) = self.acquire_page_from_buddy(index) else {
                        return;
                    };
                    self.slabs[index].init_page(
                        page_addr,
                        index as u8,
                        self.metadata,
                        self.heap_base,
                        self.max_pages,
                    );
                }
            }
        }
    }

    fn allocate_from_buddy(&mut self, layout: &Layout) -> Option<NonNull<u8>> {
        let result = if layout.size().max(layout.align()) <= DYNAMIC_SLAB_MAX_SIZE {
            self.allocate_slab_from_buddy(layout)
        } else {
            let (order, align_order) = Self::buddy_orders(layout)?;
            self.allocate_buddy_block(order, align_order)
        };

        if result.is_none() {
            self.reclaim_buddy_page_pool();
            if layout.size().max(layout.align()) > DYNAMIC_SLAB_MAX_SIZE {
                let (order, align_order) = Self::buddy_orders(layout)?;
                return self.allocate_buddy_block(order, align_order);
            }
        }
        result
    }

    fn allocate_buddy_block(&mut self, order: usize, align_order: usize) -> Option<NonNull<u8>> {
        let frame = self.buddy.alloc_pages_aligned(order, align_order)?;
        match self.pfn_to_pointer(frame.pfn()) {
            Some(ptr) => Some(ptr),
            None => {
                unsafe { self.buddy.free_pages(frame, order) };
                None
            }
        }
    }

    pub fn allocate(&mut self, layout: &Layout) -> Option<NonNull<u8>> {
        if self.buddy_enabled {
            let ptr = self.allocate_from_buddy(layout);
            if ptr.is_some() {
                self.update_buddy_peak();
            }
            return ptr;
        }
        let ptr = unsafe { self.do_allocate(layout) };
        if ptr.is_some() {
            self.maximum = core::cmp::max(self.maximum, self.allocated);
        }
        ptr
    }

    unsafe fn do_allocate(&mut self, layout: &Layout) -> Option<NonNull<u8>> {
        let mut idx = Self::layout_to_slab_index(layout.size(), layout.align());

        // Slab path: try from idx upward (upsearch handles alignment mismatches on small sizes).
        while idx < DYNAMIC_SLAB_ALLOCATOR_COUNT {
            if !Self::SLAB_SIZES[idx].is_multiple_of(layout.align()) {
                if Self::SLAB_SIZES[idx] <= SLAB_MAX_UPSEARCH_SIZE {
                    idx += 1;
                    continue;
                }
                break; // fall through to system allocator
            }

            // Ensure the slab has at least one free block; add a new page if needed.
            if self.slabs[idx].free_blocks == 0 {
                if let Some((page_addr, needs_init)) = self.acquire_page(idx) {
                    if needs_init {
                        self.slabs[idx].init_page(
                            page_addr,
                            idx as u8,
                            self.metadata,
                            self.heap_base,
                            self.max_pages,
                        );
                    } else {
                        self.slabs[idx].add_page(
                            page_addr,
                            self.metadata,
                            self.heap_base,
                            self.max_pages,
                        );
                    }
                } else {
                    // Cannot acquire page, fallback to system for this allocation
                    break;
                }
            }

            let ptr = self.slabs[idx]
                .allocate_block(self.metadata, self.heap_base, self.max_pages)
                .unwrap();
            self.allocated += Self::SLAB_SIZES[idx];
            return Some(ptr);
        }

        // System allocator path (size > 2048 or incompatible alignment).
        let ptr = if let Some(ptr) = self.system_allocator.allocate(layout) {
            ptr
        } else {
            // TLSF allocation failed, try reclaiming all pages from pool
            self.reclaim_page_pool();
            self.system_allocator.allocate(layout)?
        };
        self.allocated += used_block_hdr_for_allocation_unknown_align(ptr)
            .unwrap()
            .cast::<BlockHdr>()
            .as_ref()
            .size
            & !SIZE_USED;
        Some(ptr)
    }

    /// Obtain a fresh PAGE_SIZE page: first try the pool, then system.
    /// Returns (page_addr, needs_init): needs_init=false if page is already initialized for this slab.
    unsafe fn acquire_page(&mut self, slab_index: usize) -> Option<(usize, bool)> {
        if let Some((page, needs_init)) =
            self.page_pool
                .take_page(slab_index, self.metadata, self.heap_base, self.max_pages)
        {
            return Some((page, needs_init));
        }
        let page_layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
        if let Some(ptr) = self.system_allocator.allocate(&page_layout) {
            return Some((ptr.as_ptr() as usize, true));
        }
        // system allocation failed, try reclaiming all pages from pool
        self.reclaim_page_pool();
        self.system_allocator
            .allocate(&page_layout)
            .map(|ptr| (ptr.as_ptr() as usize, true))
    }

    // ── Deallocation ───────────────────────────────────────────────────────

    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: &Layout) -> usize {
        if self.buddy_enabled {
            return self.deallocate_from_buddy(ptr, Some(layout));
        }
        let ptr_addr = ptr.as_ptr() as usize;
        if let Some(slab_idx) = ptr_is_slab(ptr_addr, self.metadata, self.heap_base, self.max_pages)
        {
            self.free_slab_block(ptr, slab_idx as usize)
        } else {
            let size = self.system_allocator.deallocate(ptr, layout.align());
            self.allocated -= size;
            size
        }
    }

    pub unsafe fn deallocate_unknown_align(&mut self, ptr: NonNull<u8>) -> usize {
        if self.buddy_enabled {
            return self.deallocate_from_buddy(ptr, None);
        }
        let ptr_addr = ptr.as_ptr() as usize;
        if let Some(slab_idx) = ptr_is_slab(ptr_addr, self.metadata, self.heap_base, self.max_pages)
        {
            self.free_slab_block(ptr, slab_idx as usize)
        } else {
            let size = self.system_allocator.deallocate_unknown_align(ptr);
            self.allocated -= size;
            size
        }
    }

    unsafe fn deallocate_from_buddy(&mut self, ptr: NonNull<u8>, layout: Option<&Layout>) -> usize {
        if let Some(slab_index) = ptr_is_slab(
            ptr.as_ptr() as usize,
            self.metadata,
            self.heap_base,
            self.max_pages,
        ) {
            return self.deallocate_slab_to_buddy(ptr, slab_index as usize);
        }

        let pfn = self
            .pointer_to_pfn(ptr)
            .expect("buddy allocation must start at a page boundary");
        let order = match layout {
            Some(layout) => Self::buddy_orders(layout)
                .map(|orders| orders.0)
                .expect("invalid buddy allocation layout"),
            None => self
                .buddy
                .allocation_order(pfn)
                .expect("pointer is not a live buddy allocation"),
        };
        debug_assert_eq!(self.buddy.allocation_order(pfn), Some(order));
        self.buddy.free_pages_pfn(pfn, order);
        PAGE_SIZE << order
    }

    unsafe fn deallocate_slab_to_buddy(&mut self, ptr: NonNull<u8>, slab_index: usize) -> usize {
        let (page_addr, page_empty) =
            self.slabs[slab_index].free_block(ptr, self.metadata, self.heap_base);
        let block_size = Self::SLAB_SIZES[slab_index];
        self.allocated -= block_size;

        if !page_empty {
            return block_size;
        }

        self.slabs[slab_index].remove_page(
            page_addr,
            self.metadata,
            self.heap_base,
            self.max_pages,
        );
        let page_index = page_to_meta_index(page_addr, self.heap_base);
        if self.page_pool.total_pages < self.page_pool.max_total_pages {
            {
                let meta = get_page_meta(page_addr, self.metadata, self.heap_base);
                meta.slab_index = slab_index as u8;
                meta.page_magic = PAGE_MAGIC;
            }
            list_push_front(
                &mut self.page_pool.lists[slab_index],
                page_index,
                self.metadata,
                self.max_pages,
            );
            self.page_pool.total_pages += 1;
        } else {
            let meta = &mut *self.metadata.add(page_index);
            meta.page_magic = 0;
            meta.next_page = self.max_pages;
            meta.prev_page = self.max_pages;
            self.buddy.free_pages_pfn(page_index, 0);
        }
        block_size
    }

    /// Deallocate a live object from a provider-backed slab.
    ///
    /// Returns `None` when the pointer is not owned by dynamic slab.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live allocation returned by `allocate_with_provider`
    /// when this function returns `Some`.
    pub unsafe fn deallocate_with_provider<P: SlabPageProvider>(
        &mut self,
        ptr: NonNull<u8>,
        provider: &mut P,
    ) -> Option<usize> {
        let slab_index = ptr_is_slab(
            ptr.as_ptr() as usize,
            self.metadata,
            self.heap_base,
            self.max_pages,
        )? as usize;
        let (page_addr, page_empty) =
            self.slabs[slab_index].free_block(ptr, self.metadata, self.heap_base);
        self.allocated -= Self::SLAB_SIZES[slab_index];

        if page_empty {
            self.slabs[slab_index].remove_page(
                page_addr,
                self.metadata,
                self.heap_base,
                self.max_pages,
            );
            self.page_pool.release_page_to(
                page_addr,
                slab_index,
                provider,
                self.metadata,
                self.heap_base,
                self.max_pages,
            );
        }
        Some(Self::SLAB_SIZES[slab_index])
    }

    /// Free a slab block and, if the page becomes fully empty, either pool or
    /// return the page to TLSF.
    unsafe fn free_slab_block(&mut self, ptr: NonNull<u8>, idx: usize) -> usize {
        let (page_addr, page_empty) =
            self.slabs[idx].free_block(ptr, self.metadata, self.heap_base);
        self.allocated -= Self::SLAB_SIZES[idx];

        if page_empty {
            self.slabs[idx].remove_page(page_addr, self.metadata, self.heap_base, self.max_pages);
            self.page_pool.release_page(
                page_addr,
                idx,
                &mut self.system_allocator,
                self.metadata,
                self.heap_base,
                self.max_pages,
            );
        }
        Self::SLAB_SIZES[idx]
    }

    // ── Reallocation ───────────────────────────────────────────────────────

    pub unsafe fn reallocate(
        &mut self,
        ptr: NonNull<u8>,
        new_layout: &Layout,
    ) -> Option<NonNull<u8>> {
        if self.buddy_enabled {
            let old_capacity = self.allocation_kind(ptr)?.capacity();
            return self.reallocate_from_buddy(ptr, new_layout, old_capacity);
        }
        let ptr_addr = ptr.as_ptr() as usize;
        if let Some(slab_idx) = ptr_is_slab(ptr_addr, self.metadata, self.heap_base, self.max_pages)
        {
            let idx = slab_idx as usize;
            if new_layout.size() <= Self::SLAB_SIZES[idx]
                && Self::SLAB_SIZES[idx].is_multiple_of(new_layout.align())
            {
                return Some(ptr); // fits in existing block
            }
            let new_ptr = self.allocate(new_layout)?;
            core::ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), Self::SLAB_SIZES[idx]);
            // Use deallocate_unknown_align: for slab blocks, deallocate() ignores
            // the layout argument (routes via ptr_is_slab), so this is cleaner.
            self.deallocate_unknown_align(ptr);
            Some(new_ptr)
        } else {
            let old_size = used_block_hdr_for_allocation_unknown_align(ptr)
                .unwrap()
                .cast::<BlockHdr>()
                .as_ref()
                .size
                & !SIZE_USED;
            let new_ptr = self.system_allocator.reallocate(ptr, new_layout)?;
            let new_size = used_block_hdr_for_allocation_unknown_align(new_ptr)
                .unwrap()
                .cast::<BlockHdr>()
                .as_ref()
                .size
                & !SIZE_USED;
            self.allocated = self.allocated - old_size + new_size;
            Some(new_ptr)
        }
    }

    pub unsafe fn reallocate_unknown_align(
        &mut self,
        ptr: NonNull<u8>,
        new_size: usize,
    ) -> Option<NonNull<u8>> {
        if self.buddy_enabled {
            let old_capacity = self.allocation_kind(ptr)?.capacity();
            let new_layout =
                Layout::from_size_align(new_size, core::mem::size_of::<usize>()).ok()?;
            return self.reallocate_from_buddy(ptr, &new_layout, old_capacity);
        }
        let ptr_addr = ptr.as_ptr() as usize;
        if let Some(slab_idx) = ptr_is_slab(ptr_addr, self.metadata, self.heap_base, self.max_pages)
        {
            let idx = slab_idx as usize;
            if new_size <= Self::SLAB_SIZES[idx] {
                return Some(ptr);
            }
            let new_layout =
                Layout::from_size_align_unchecked(new_size, core::mem::size_of::<usize>());
            let new_ptr = self.allocate(&new_layout)?;
            core::ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), Self::SLAB_SIZES[idx]);
            self.deallocate_unknown_align(ptr);
            Some(new_ptr)
        } else {
            let old_size = used_block_hdr_for_allocation_unknown_align(ptr)
                .unwrap()
                .cast::<BlockHdr>()
                .as_ref()
                .size
                & !SIZE_USED;
            let new_ptr = self
                .system_allocator
                .reallocate_unknown_align(ptr, new_size)?;
            let new_allocated = used_block_hdr_for_allocation_unknown_align(new_ptr)
                .unwrap()
                .cast::<BlockHdr>()
                .as_ref()
                .size
                & !SIZE_USED;
            self.allocated = self.allocated - old_size + new_allocated;
            Some(new_ptr)
        }
    }

    fn allocation_kind(&self, ptr: NonNull<u8>) -> Option<DynamicAllocationKind> {
        if let Some(size) = self.slab_allocation_size(ptr) {
            return Some(DynamicAllocationKind::Slab(size));
        }
        let pfn = self.pointer_to_pfn(ptr)?;
        self.buddy
            .allocation_order(pfn)
            .map(DynamicAllocationKind::Buddy)
    }

    unsafe fn reallocate_from_buddy(
        &mut self,
        ptr: NonNull<u8>,
        new_layout: &Layout,
        old_capacity: usize,
    ) -> Option<NonNull<u8>> {
        let old_kind = self.allocation_kind(ptr)?;
        let new_is_slab = new_layout.size().max(new_layout.align()) <= DYNAMIC_SLAB_MAX_SIZE;
        let can_reuse = match old_kind {
            DynamicAllocationKind::Slab(class_size) => {
                new_is_slab && Self::class_size(new_layout) == Some(class_size)
            }
            DynamicAllocationKind::Buddy(order) => {
                !new_is_slab
                    && Self::buddy_orders(new_layout).map(|orders| orders.0) == Some(order)
                    && (ptr.as_ptr() as usize) & (new_layout.align() - 1) == 0
            }
        };
        if can_reuse {
            return Some(ptr);
        }

        let new_ptr = self.allocate_from_buddy(new_layout)?;
        self.update_buddy_peak();
        core::ptr::copy_nonoverlapping(
            ptr.as_ptr(),
            new_ptr.as_ptr(),
            old_capacity.min(new_layout.size()),
        );
        self.deallocate_from_buddy(ptr, None);
        Some(new_ptr)
    }

    // ── Statistics ─────────────────────────────────────────────────────────

    pub fn allocated(&self) -> usize {
        if self.buddy_enabled {
            return self.buddy.memory_info().used_pages * PAGE_SIZE;
        }
        self.allocated
    }
    pub fn maximum(&self) -> usize {
        self.maximum
    }
    pub fn total(&self) -> usize {
        if self.buddy_enabled {
            let info = self.buddy.memory_info();
            return (info.total_pages - info.reserved_pages) * PAGE_SIZE;
        }
        self.total
    }

    pub fn size_of_allocation(&self, ptr: NonNull<u8>) -> Option<usize> {
        let ptr_addr = ptr.as_ptr() as usize;
        if let Some(slab_idx) = ptr_is_slab(ptr_addr, self.metadata, self.heap_base, self.max_pages)
        {
            Some(Self::SLAB_SIZES[slab_idx as usize])
        } else if self.buddy_enabled {
            let pfn = self.pointer_to_pfn(ptr)?;
            self.buddy
                .allocation_order(pfn)
                .map(|order| PAGE_SIZE << order)
        } else {
            self.system_allocator.size_of_allocation(ptr)
        }
    }

    /// Return the slab-class capacity when `ptr` belongs to this dynamic slab.
    pub fn slab_allocation_size(&self, ptr: NonNull<u8>) -> Option<usize> {
        let slab_index = ptr_is_slab(
            ptr.as_ptr() as usize,
            self.metadata,
            self.heap_base,
            self.max_pages,
        )?;
        Some(Self::SLAB_SIZES[slab_index as usize])
    }

    pub fn get_max_free_block_size(&self) -> usize {
        if self.buddy_enabled {
            return self.buddy.max_free_block_size();
        }
        let sys_free = self.system_allocator.get_max_free_block_size();
        for i in (0..DYNAMIC_SLAB_ALLOCATOR_COUNT).rev() {
            if self.slabs[i].free_blocks > 0 {
                return core::cmp::max(sys_free, Self::SLAB_SIZES[i]);
            }
        }
        sys_free
    }

    // ── TLSF memory pressure ───────────────────────────────────────────────

    /// Reclaim pages from pool to TLSF when memory pressure is detected.
    /// Called when TLSF allocation fails or by idle thread for proactive reclaim.
    pub fn check_memory_pressure(&mut self) {
        if self.buddy_enabled {
            self.reclaim_buddy_page_pool();
            return;
        }
        let max_free = self.system_allocator.get_max_free_block_size();
        if max_free < MEMORY_PRESSURE_THRESHOLD {
            let pages_needed = (MEMORY_PRESSURE_THRESHOLD - max_free) / PAGE_SIZE + 1;
            unsafe {
                self.page_pool.reclaim_to_system(
                    &mut self.system_allocator,
                    pages_needed,
                    self.metadata,
                    self.heap_base,
                    self.max_pages,
                );
            }
        }
    }

    /// Reclaim all pages from page pool back to TLSF.
    /// Used in tests for accurate memory leak detection.
    pub fn reclaim_page_pool(&mut self) {
        if self.buddy_enabled {
            self.reclaim_buddy_page_pool();
            return;
        }
        unsafe {
            self.page_pool.reclaim_to_system(
                &mut self.system_allocator,
                usize::MAX,
                self.metadata,
                self.heap_base,
                self.max_pages,
            );
        }
    }

    fn reclaim_buddy_page_pool(&mut self) -> usize {
        let mut reclaimed = 0;
        unsafe {
            loop {
                let mut page_index = None;
                for index in 0..DYNAMIC_SLAB_ALLOCATOR_COUNT {
                    if let Some(found) = list_pop_front(
                        &mut self.page_pool.lists[index],
                        self.metadata,
                        self.max_pages,
                    ) {
                        page_index = Some(found);
                        break;
                    }
                }

                let Some(page_index) = page_index else {
                    break;
                };
                let meta = &mut *self.metadata.add(page_index);
                meta.page_magic = 0;
                meta.next_page = self.max_pages;
                meta.prev_page = self.max_pages;
                self.buddy.free_pages_pfn(page_index, 0);
                self.page_pool.total_pages -= 1;
                reclaimed += 1;
            }
        }
        reclaimed
    }

    /// Return all cached empty pages to an external page provider.
    pub fn reclaim_page_pool_to<P: SlabPageProvider>(&mut self, provider: &mut P) -> usize {
        unsafe {
            self.page_pool.reclaim_to_provider(
                provider,
                usize::MAX,
                self.metadata,
                self.heap_base,
                self.max_pages,
            )
        }
    }

    pub fn get_slab_stat(&self) -> (SlabStatData, usize, usize) {
        let mut data = [(0usize, 0usize, 0usize, 0usize); DYNAMIC_SLAB_ALLOCATOR_COUNT];
        for (i, item) in data.iter_mut().enumerate() {
            let bpp = blocks_per_page(Self::SLAB_SIZES[i]);
            let pages = self.slabs[i].total_blocks.div_ceil(bpp);
            *item = (
                Self::SLAB_SIZES[i],
                pages,
                self.slabs[i].total_blocks,
                self.slabs[i].free_blocks,
            );
        }
        (
            data,
            self.page_pool.total_pages,
            self.page_pool.max_total_pages,
        )
    }
}

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

//! Global first-fit allocator backed by a static array of physical frame states.

use super::{FrameAllocator, FrameUsage};
use crate::{
    mm::{memory_bank::MemoryRegion, PhysicalAddress},
    sync::SpinLock,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameState {
    Free,
    Reserved,
    Allocated,
}

/// Initialize frame states from available RAM and overlapping reservations.
/// Only complete pages of RAM become free; any overlap with a reservation
/// reserves the whole page. Gaps outside the described RAM remain reserved.
pub fn init_frame_states(
    base: PhysicalAddress,
    page_size: usize,
    states: &mut [FrameState],
    regions: &[MemoryRegion],
) -> Option<()> {
    let base = base.into_raw();
    if !page_size.is_power_of_two() || base & (page_size - 1) != 0 {
        return None;
    }
    let end = base.checked_add(states.len().checked_mul(page_size)?)?;
    states.fill(FrameState::Reserved);
    // Available banks can overlap reservations; apply reservations last.
    for is_reserved in [false, true] {
        for region in regions.iter().filter(|r| r.is_reserved == is_reserved) {
            let start = region.base.into_raw();
            let region_end = start.checked_add(region.size)?;
            let start = start.max(base);
            let region_end = region_end.min(end);
            if start >= region_end {
                continue;
            }
            let (first, last, state) = if is_reserved {
                (
                    (start - base) / page_size,
                    (region_end - base).div_ceil(page_size),
                    FrameState::Reserved,
                )
            } else {
                (
                    (start - base).div_ceil(page_size),
                    (region_end - base) / page_size,
                    FrameState::Free,
                )
            };
            if first < last {
                states[first..last].fill(state);
            }
        }
    }
    Some(())
}

struct AllocatorState {
    base: usize,
    page_size: usize,
    states: &'static mut [FrameState],
    // Allocation is monotonic: pages below this index can never become free.
    first_free: usize,
}

/// A single shared allocator, accessed through [`Self::instance`].
///
/// The caller supplies a static state array at initialization. Pages containing
/// the kernel, metadata, or other live objects must not be marked free.
pub struct FirstFitFrameAllocator {
    state: SpinLock<Option<AllocatorState>>,
}

static FIRST_FIT: FirstFitFrameAllocator = FirstFitFrameAllocator {
    state: SpinLock::new(None),
};

impl FirstFitFrameAllocator {
    pub fn instance() -> &'static Self {
        &FIRST_FIT
    }

    /// Initialize the singleton for `[base, base + N * page_size)`.
    ///
    /// Returns `None` if already initialized, the page size is not a nonzero
    /// power of two, the base is misaligned, or the physical range overflows.
    ///
    /// # Safety
    /// Free pages must be valid RAM exclusively owned by this allocator and
    /// must not overlap any live objects, including the allocator itself.
    /// The RAM must remain valid for the lifetime of all returned frames.
    pub unsafe fn init<const N: usize>(
        &self,
        base: PhysicalAddress,
        page_size: usize,
        states: &'static mut [FrameState; N],
    ) -> Option<()> {
        let base = base.into_raw();
        if !page_size.is_power_of_two() || base & (page_size - 1) != 0 {
            return None;
        }
        let size = N.checked_mul(page_size)?;
        base.checked_add(size)?;
        let mut state = self.state.irqsave_lock();
        if state.is_some() {
            return None;
        }
        *state = Some(AllocatorState {
            base,
            page_size,
            states,
            first_free: 0,
        });
        Some(())
    }

    pub fn usage(&self) -> Option<FrameUsage> {
        let state = self.state.irqsave_lock();
        let state = state.as_ref()?;
        let free = state.states[state.first_free..]
            .iter()
            .filter(|state| **state == FrameState::Free)
            .count();
        Some(FrameUsage::new(
            state.states.len() - free,
            state.states.len(),
        ))
    }

    /// Return the lowest-address free frame, or `None` before initialization
    /// or when all frames are reserved or allocated.
    pub fn alloc_frame(&self) -> Option<PhysicalAddress> {
        let mut state = self.state.irqsave_lock();
        let state = state.as_mut()?;
        let start = state.first_free;
        let Some(offset) = state.states[start..]
            .iter()
            .position(|state| *state == FrameState::Free)
        else {
            state.first_free = state.states.len();
            return None;
        };
        let index = start + offset;
        state.states[index] = FrameState::Allocated;
        state.first_free = index + 1;
        // Initialization checked the entire physical range for overflow.
        Some(PhysicalAddress::new(state.base + index * state.page_size))
    }
}

// SAFETY: All references access the same state under an IRQ-saving lock. Init
// requires exclusive ownership of free pages and succeeds only once. Allocated
// pages are marked before returning, so concurrent calls cannot overlap.
unsafe impl FrameAllocator for &FirstFitFrameAllocator {
    fn alloc_frame(&mut self) -> Option<PhysicalAddress> {
        FirstFitFrameAllocator::alloc_frame(self)
    }

    fn usage(&self) -> FrameUsage {
        FirstFitFrameAllocator::usage(self).expect("FirstFitFrameAllocator didn't be initialized.")
    }
}

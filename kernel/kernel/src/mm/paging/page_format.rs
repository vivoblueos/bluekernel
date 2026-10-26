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

pub trait PageLevel {
    const INDEX_SHIFT: u8;
    const INDEX_BITS: u8;
}

pub fn page_index<L: PageLevel>(va: usize) -> usize {
    let mask = (1usize << L::INDEX_BITS) - 1;
    ((va >> L::INDEX_SHIFT) & mask) as usize
}

// #[derive(Copy, Clone)]
// pub struct PageFormat<const LEVEL_COUNT: usize> {
//     pub va_bits: u8,
//     pub max_pa_bits: u8,
//     pub levels: [PageLevel; LEVEL_COUNT],
// }

// impl<const LEVEL_COUNT: usize> PageFormat<LEVEL_COUNT> {
//     const fn level_count(&self) -> usize {
//         self.levels.len()
//     }

    // Safty: the validation of the va and pa is not checked here, 
    //        it should be checked before calling this function.
//     #[inline(always)]
//     pub fn index(&self, va: u64, level: usize) -> usize {
//         let spec = &self.levels[level];
//         let mask = (1u64 << spec.index_bits) - 1;
//         ((va >> spec.index_shift) & mask) as usize
//     }
// }

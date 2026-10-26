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

use crate::mm::paging::PageLevel;

pub struct Level0;
pub struct Level1;
pub struct Level2;
pub struct Level3;

impl PageLevel for Level0 {
    const INDEX_SHIFT: u8 = 39;
    const INDEX_BITS: u8 = 9;
}

impl PageLevel for Level1 {
    const INDEX_SHIFT: u8 = 30;
    const INDEX_BITS: u8 = 9;
}

impl PageLevel for Level2 {
    const INDEX_SHIFT: u8 = 21;
    const INDEX_BITS: u8 = 9;
}

impl PageLevel for Level3 {
    const INDEX_SHIFT: u8 = 12;
    const INDEX_BITS: u8 = 9;
}

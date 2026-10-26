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

use super::*;
use blueos_test_macro::test;

fn normal(flags: MemFlags) -> MemAttribute {
    MemAttribute {
        flags,
        memory_type: MemType::Normal(CachePolicy::WriteBack),
    }
}

#[test]
fn test_descriptor_process_permissions() {
    use MemFlags as F;
    // Expected AP[2:1], PXN and UXN for code, data and read-only mappings.
    let cases = [
        (F::READ | F::EXECUTE | F::USER, 0b11, 1, 0),
        (F::READ | F::WRITE | F::USER, 0b01, 1, 1),
        (F::READ | F::USER, 0b11, 1, 1),
        (F::READ | F::WRITE, 0b00, 1, 1),
        (F::READ | F::EXECUTE, 0b10, 0, 1),
        (F::READ | F::WRITE | F::EXECUTE, 0b00, 0, 1),
    ];
    for (flags, ap, pxn, uxn) in cases {
        for kind in [DescriptorKind::Block, DescriptorKind::Page] {
            let entry = set_descriptor(0x4000_0000, normal(flags), kind).unwrap();
            assert_eq!((entry >> 6) & 0b11, ap);
            assert_eq!((entry >> 53) & 1, pxn);
            assert_eq!((entry >> 54) & 1, uxn);
            assert_eq!(entry & 1, 1);
            assert_eq!((entry >> 10) & 1, 1);
        }
    }
}

#[test]
fn test_descriptor_translation_scope() {
    let local = set_descriptor(0, normal(MemFlags::READ), DescriptorKind::Page).unwrap();
    let global = set_descriptor(
        0,
        normal(MemFlags::READ | MemFlags::GLOBAL),
        DescriptorKind::Page,
    )
    .unwrap();
    assert_eq!((local >> 11) & 1, 1);
    assert_eq!((global >> 11) & 1, 0);
    assert_eq!(local ^ global, 1 << 11);
}

#[test]
fn test_descriptor_no_access() {
    for flags in [
        MemFlags::empty(),
        MemFlags::USER,
        MemFlags::GLOBAL,
        MemFlags::USER | MemFlags::GLOBAL,
    ] {
        for kind in [DescriptorKind::Block, DescriptorKind::Page] {
            assert_eq!(set_descriptor(0x4000_0000, normal(flags), kind), Ok(0));
        }
    }
}

#[test]
fn test_descriptor_rejects_unrepresentable_permissions() {
    for flags in [
        MemFlags::WRITE,
        MemFlags::EXECUTE,
        MemFlags::WRITE | MemFlags::EXECUTE,
    ] {
        for user in [MemFlags::empty(), MemFlags::USER] {
            for kind in [DescriptorKind::Block, DescriptorKind::Page] {
                assert!(set_descriptor(0, normal(flags | user), kind).is_err());
            }
        }
    }
    let executable_device = MemAttribute {
        flags: MemFlags::READ | MemFlags::EXECUTE,
        memory_type: MemType::Device,
    };
    assert!(set_descriptor(0, executable_device, DescriptorKind::Page).is_err());
}

#[test]
fn test_descriptor_memory_types() {
    let cases = [
        (MemType::Device, 0, 0),
        (MemType::Normal(CachePolicy::WriteBack), 1, 0b11),
        (MemType::Normal(CachePolicy::Uncached), 2, 0b11),
        (MemType::Normal(CachePolicy::WriteCombining), 2, 0b11),
        (MemType::Normal(CachePolicy::WriteThrough), 3, 0b11),
    ];
    for (memory_type, index, shareability) in cases {
        let entry = set_descriptor(
            0,
            MemAttribute {
                flags: MemFlags::READ | MemFlags::WRITE,
                memory_type,
            },
            DescriptorKind::Page,
        )
        .unwrap();
        assert_eq!((entry >> 2) & 0b111, index);
        assert_eq!((entry >> 8) & 0b11, shareability);
        assert_eq!((entry >> 53) & 0b11, 0b11);
    }
}

#[test]
fn test_descriptor_address_and_kind() {
    let attr = normal(MemFlags::READ | MemFlags::USER);
    let block = set_descriptor(0x4000_0000, attr, DescriptorKind::Block).unwrap();
    let page = set_descriptor(0x4000_0000, attr, DescriptorKind::Page).unwrap();
    assert_eq!(block & 0b11, 0b01);
    assert_eq!(page & 0b11, 0b11);
    assert_eq!(block ^ page, 1 << 1);
    assert_eq!(page & 0x0000_ffff_ffff_f000, 0x4000_0000);

    // Address bits outside the output address field cannot alter permissions.
    for kind in [DescriptorKind::Block, DescriptorKind::Page] {
        assert_eq!(
            set_descriptor(0xffff_0000_4000_0000, attr, kind),
            set_descriptor(0x4000_0000, attr, kind),
        );
    }
}

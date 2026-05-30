//! FieldAddr construction + type-mismatch detection tests.

use agent_core::mem_value::ValType;
use agent_core::spine::{FieldAddr, FieldInfo, MemAddr, ReadWrite};

#[test]
fn field_addr_construction_carries_addr_and_type() {
    let addr: MemAddr<ReadWrite> = unsafe { MemAddr::from_raw_writable(0x1000) };
    let fa = FieldAddr::new(addr, ValType::U32);
    assert_eq!(fa.addr().as_u64(), 0x1000);
    assert_eq!(fa.val_type(), ValType::U32);
}

#[test]
fn field_addr_is_copy_and_eq() {
    let addr: MemAddr<ReadWrite> = unsafe { MemAddr::from_raw_writable(0x2000) };
    let a = FieldAddr::new(addr, ValType::F32);
    let b = a;
    assert_eq!(a, b);
}

#[test]
fn field_info_is_copy_and_struct_fields_accessible() {
    let fi = FieldInfo {
        name_ptr: 0xdeadbeef,
        offset:   0x10,
        val_type: ValType::U64,
        token:    0x04000001,
    };
    let copy = fi;
    assert_eq!(copy.name_ptr, 0xdeadbeef);
    assert_eq!(copy.offset, 0x10);
    assert_eq!(copy.val_type, ValType::U64);
    assert_eq!(copy.token, 0x04000001);
}

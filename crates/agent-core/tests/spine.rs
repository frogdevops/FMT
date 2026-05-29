use agent_core::mem_value::status;
use agent_core::spine::MemError;

#[test]
fn mem_error_maps_to_existing_status_codes() {
    assert_eq!(i32::from(MemError::Unreadable),   status::ERR_UNREADABLE);
    assert_eq!(i32::from(MemError::Unwritable),   status::ERR_UNWRITABLE);
    assert_eq!(i32::from(MemError::BadType),      status::ERR_BAD_TYPE);
    assert_eq!(i32::from(MemError::BufTooSmall),  status::ERR_BUF_TOO_SMALL);
    assert_eq!(i32::from(MemError::Denied),       status::ERR_DENIED);
}

use agent_core::spine::{MemAddr, ReadOnly, ReadWrite};

#[test]
fn mem_addr_is_pointer_sized() {
    assert_eq!(std::mem::size_of::<MemAddr<ReadOnly>>(), 8);
    assert_eq!(std::mem::size_of::<MemAddr<ReadWrite>>(), 8);
}

#[test]
fn from_raw_round_trips_readonly() {
    let a = MemAddr::from_raw(0x1234_5678_DEAD_BEEF);
    assert_eq!(a.as_u64(), 0x1234_5678_DEAD_BEEF);
}

#[test]
fn from_raw_writable_round_trips() {
    // SAFETY: test only — no real memory at this address.
    let a = unsafe { MemAddr::from_raw_writable(0xAAAA_BBBB_CCCC_DDDD) };
    assert_eq!(a.as_u64(), 0xAAAA_BBBB_CCCC_DDDD);
}

#[test]
fn writable_downgrades_to_readonly() {
    let w = unsafe { MemAddr::from_raw_writable(0x42) };
    let r: MemAddr<ReadOnly> = w.as_readonly();
    assert_eq!(r.as_u64(), 0x42);
}

#[test]
fn readonly_upgrade_round_trips() {
    let r = MemAddr::from_raw(0x99);
    let w: MemAddr<ReadWrite> = unsafe { r.mark_writable() };
    assert_eq!(w.as_u64(), 0x99);
}

use agent_core::spine::{FrameSeq, Instance, KlassPtr, MethodPtr, SocketHandle};

#[test]
fn handles_are_pointer_sized() {
    assert_eq!(std::mem::size_of::<KlassPtr>(),     8);
    assert_eq!(std::mem::size_of::<MethodPtr>(),    8);
    assert_eq!(std::mem::size_of::<Instance>(),     8);
    assert_eq!(std::mem::size_of::<FrameSeq>(),     8);
    assert_eq!(std::mem::size_of::<SocketHandle>(), 8);
}

#[test]
fn handle_round_trips() {
    assert_eq!(KlassPtr::from_raw(0xAAA).as_u64(),     0xAAA);
    assert_eq!(MethodPtr::from_raw(0xBBB).as_u64(),    0xBBB);
    assert_eq!(Instance::from_raw(0xCCC).as_u64(),     0xCCC);
    assert_eq!(FrameSeq::from_raw(7).as_u64(),         7);
    assert_eq!(SocketHandle::from_raw(0xDDD).as_u64(), 0xDDD);
}

use agent_core::mem_value::ValType;
use agent_core::spine::{InvokeError, MemValue};

#[test]
fn invoke_error_maps_to_distinct_status_range() {
    // -100..-106 per the spec; all distinct, none collide with MemError (-1..-5).
    let codes = [
        i32::from(InvokeError::NotFound),
        i32::from(InvokeError::ArgCountMismatch { expected: 0, got: 0 }),
        i32::from(InvokeError::ArgTypeMismatch { idx: 0, expected: ValType::U8, got: ValType::U8 }),
        i32::from(InvokeError::NullInstance),
        i32::from(InvokeError::MarshalFailed { idx: 0, reason: "" }),
        i32::from(InvokeError::ManagedException(String::new())),
        i32::from(InvokeError::InternalFailure("")),
    ];
    for c in codes {
        assert!(c >= -106 && c <= -100, "invoke status {} outside -100..-106", c);
    }
    // No overlap with MemError range.
    for c in codes {
        assert!(c < status::ERR_UNREADABLE && c > -200, "invoke status {} overlaps mem/hook range", c);
    }
}

#[test]
fn mem_value_u32_round_trip() {
    let v: u32 = 0xDEAD_BEEF;
    let buf = v.to_le_bytes_buf();
    assert_eq!(buf.len(), 4);
    let back: u32 = u32::from_le_bytes_spine(&buf).unwrap();
    assert_eq!(back, v);
    assert_eq!(<u32 as MemValue>::VAL_TYPE, ValType::U32);
}

#[test]
fn mem_value_i64_round_trip() {
    let v: i64 = -42;
    let buf = v.to_le_bytes_buf();
    assert_eq!(buf.len(), 8);
    assert_eq!(i64::from_le_bytes_spine(&buf), Some(v));
    assert_eq!(<i64 as MemValue>::VAL_TYPE, ValType::I64);
}

#[test]
fn mem_value_f32_round_trip() {
    let v: f32 = 3.14159;
    let buf = v.to_le_bytes_buf();
    assert_eq!(buf.len(), 4);
    assert_eq!(f32::from_le_bytes_spine(&buf), Some(v));
}

#[test]
fn mem_value_rejects_short_buffer() {
    let buf = [0u8; 2];
    assert!(u32::from_le_bytes_spine(&buf).is_none());
}

#[test]
fn mem_value_all_numerics_have_a_val_type() {
    assert_eq!(<u8  as MemValue>::VAL_TYPE, ValType::U8);
    assert_eq!(<u16 as MemValue>::VAL_TYPE, ValType::U16);
    assert_eq!(<u64 as MemValue>::VAL_TYPE, ValType::U64);
    assert_eq!(<i8  as MemValue>::VAL_TYPE, ValType::I8);
    assert_eq!(<i16 as MemValue>::VAL_TYPE, ValType::I16);
    assert_eq!(<i32 as MemValue>::VAL_TYPE, ValType::I32);
    assert_eq!(<f64 as MemValue>::VAL_TYPE, ValType::F64);
}

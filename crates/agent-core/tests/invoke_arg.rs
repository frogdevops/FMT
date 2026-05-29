use agent_core::mem_value::Value;
use agent_core::spine::{Instance, InvokeArg};

fn round_trip(v: InvokeArg) {
    let bytes = v.encode();
    let (back, consumed) = InvokeArg::decode(&bytes).expect("decode failed");
    assert_eq!(consumed, bytes.len(), "consumed != encoded length");
    assert_eq!(back, v, "round-trip mismatch");
}

#[test] fn rt_prim_u32() { round_trip(InvokeArg::Prim(Value::U32(0xDEAD_BEEF))); }
#[test] fn rt_prim_f64() { round_trip(InvokeArg::Prim(Value::F64(3.14159))); }
#[test] fn rt_instance() { round_trip(InvokeArg::Instance(Instance::from_raw(0xAAAA_BBBB))); }
#[test] fn rt_string()   { round_trip(InvokeArg::String("hello world".into())); }
#[test] fn rt_struct()   { round_trip(InvokeArg::Struct(vec![1, 2, 3, 4, 5, 6, 7, 8])); }
#[test] fn rt_null()     { round_trip(InvokeArg::Null); }

#[test]
fn rt_nested_array() {
    round_trip(InvokeArg::Array(vec![
        InvokeArg::Prim(Value::I32(-1)),
        InvokeArg::String("x".into()),
        InvokeArg::Array(vec![InvokeArg::Null, InvokeArg::Instance(Instance::from_raw(7))]),
    ]));
}

#[test]
fn decode_rejects_short_buffer() {
    let bytes = [13u8, 0xFF, 0, 0, 0]; // tag=String, len=255, but body is empty
    assert!(InvokeArg::decode(&bytes).is_none());
}

#[test]
fn decode_rejects_unknown_tag() {
    let bytes = [99u8];
    assert!(InvokeArg::decode(&bytes).is_none());
}

//! `InvokeArg` — the marshalled value vocabulary for il2cpp invoke + hook ops.
//! Extends `mem_value::Value` with kinds the mem domain doesn't need (Instance,
//! String, Struct, Array, Null). The variants map 1:1 to wire-tag bytes 0..16.

use crate::mem_value::Value;
use crate::spine::handles::Instance;

#[derive(Debug, Clone, PartialEq)]
pub enum InvokeArg {
    Prim(Value),               // tags 0..11 reuse the mem ABI
    Instance(Instance),        // tag 12
    String(String),            // tag 13 — UTF-8 on the wire
    Struct(Vec<u8>),           // tag 14
    Array(Vec<InvokeArg>),     // tag 15
    Null,                      // tag 16
}

/// Wire tag bytes — distinct from the `ValType` enum to make tag-space allocation
/// explicit. Tags 0..11 numerically match ValType for backwards compat.
pub mod tag {
    pub const INSTANCE: u8 = 12;
    pub const STRING:   u8 = 13;
    pub const STRUCT:   u8 = 14;
    pub const ARRAY:    u8 = 15;
    pub const NULL:     u8 = 16;
}

impl InvokeArg {
    /// Encode as `[u8 tag, payload...]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            InvokeArg::Prim(v) => {
                out.push(v.val_type() as u8);
                out.extend(v.encode());
            }
            InvokeArg::Instance(h) => {
                out.push(tag::INSTANCE);
                out.extend(&h.as_u64().to_le_bytes());
            }
            InvokeArg::String(s) => {
                out.push(tag::STRING);
                out.extend(&(s.len() as u32).to_le_bytes());
                out.extend(s.as_bytes());
            }
            InvokeArg::Struct(bytes) => {
                out.push(tag::STRUCT);
                out.extend(&(bytes.len() as u32).to_le_bytes());
                out.extend(bytes);
            }
            InvokeArg::Array(elems) => {
                out.push(tag::ARRAY);
                out.extend(&(elems.len() as u32).to_le_bytes());
                for e in elems { out.extend(e.encode()); }
            }
            InvokeArg::Null => {
                out.push(tag::NULL);
            }
        }
        out
    }

    /// Decode from `[u8 tag, payload...]`. Returns (InvokeArg, bytes_consumed)
    /// or None on short buffer / unknown tag.
    pub fn decode(bytes: &[u8]) -> Option<(InvokeArg, usize)> {
        if bytes.is_empty() { return None; }
        let tag = bytes[0];
        match tag {
            0..=11 => {
                // Primitive — delegate to ValType + Value::decode
                let vt = crate::mem_value::ValType::from_tag(tag)?;
                let width = vt.fixed_width()?;
                if bytes.len() < 1 + width { return None; }
                let v = Value::decode(vt, &bytes[1..1 + width])?;
                Some((InvokeArg::Prim(v), 1 + width))
            }
            tag::INSTANCE => {
                if bytes.len() < 9 { return None; }
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&bytes[1..9]);
                Some((InvokeArg::Instance(Instance::from_raw(u64::from_le_bytes(buf))), 9))
            }
            tag::STRING => {
                if bytes.len() < 5 { return None; }
                let len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                if bytes.len() < 5 + len { return None; }
                let s = std::str::from_utf8(&bytes[5..5 + len]).ok()?.to_owned();
                Some((InvokeArg::String(s), 5 + len))
            }
            tag::STRUCT => {
                if bytes.len() < 5 { return None; }
                let len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                if bytes.len() < 5 + len { return None; }
                Some((InvokeArg::Struct(bytes[5..5 + len].to_vec()), 5 + len))
            }
            tag::ARRAY => {
                if bytes.len() < 5 { return None; }
                let count = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                let mut elems = Vec::with_capacity(count);
                let mut consumed = 5usize;
                for _ in 0..count {
                    let (e, n) = InvokeArg::decode(&bytes[consumed..])?;
                    elems.push(e);
                    consumed += n;
                }
                Some((InvokeArg::Array(elems), consumed))
            }
            tag::NULL => Some((InvokeArg::Null, 1)),
            _ => None,
        }
    }
}

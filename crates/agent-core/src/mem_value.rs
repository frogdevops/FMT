//! Pure typed value model for the external `mem` API. The closed type set is the
//! anti-garbage gate: every read/write declares its type. Host-testable; the wire
//! encoding here is shared by the WASM host and the future frontend transport.

/// Closed set of value types. `u8` tag for the ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ValType {
    U8 = 0, U16 = 1, U32 = 2, U64 = 3,
    I8 = 4, I16 = 5, I32 = 6, I64 = 7,
    F32 = 8, F64 = 9,
    Bytes = 10,
    Cstr = 11,
}

impl ValType {
    /// Fixed byte width, or None for variable-length (Bytes/Cstr).
    pub fn fixed_width(self) -> Option<usize> {
        Some(match self {
            ValType::U8 | ValType::I8 => 1,
            ValType::U16 | ValType::I16 => 2,
            ValType::U32 | ValType::I32 | ValType::F32 => 4,
            ValType::U64 | ValType::I64 | ValType::F64 => 8,
            ValType::Bytes | ValType::Cstr => return None,
        })
    }

    pub fn from_tag(tag: u8) -> Option<ValType> {
        Some(match tag {
            0 => ValType::U8, 1 => ValType::U16, 2 => ValType::U32, 3 => ValType::U64,
            4 => ValType::I8, 5 => ValType::I16, 6 => ValType::I32, 7 => ValType::I64,
            8 => ValType::F32, 9 => ValType::F64,
            10 => ValType::Bytes, 11 => ValType::Cstr,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U8(u8), U16(u16), U32(u32), U64(u64),
    I8(i8), I16(i16), I32(i32), I64(i64),
    F32(f32), F64(f64),
    Bytes(Vec<u8>), Cstr(String),
}

impl Value {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Value::U8(v) => v.to_le_bytes().to_vec(),
            Value::U16(v) => v.to_le_bytes().to_vec(),
            Value::U32(v) => v.to_le_bytes().to_vec(),
            Value::U64(v) => v.to_le_bytes().to_vec(),
            Value::I8(v) => v.to_le_bytes().to_vec(),
            Value::I16(v) => v.to_le_bytes().to_vec(),
            Value::I32(v) => v.to_le_bytes().to_vec(),
            Value::I64(v) => v.to_le_bytes().to_vec(),
            Value::F32(v) => v.to_le_bytes().to_vec(),
            Value::F64(v) => v.to_le_bytes().to_vec(),
            Value::Bytes(b) => b.clone(),
            Value::Cstr(s) => s.as_bytes().to_vec(),
        }
    }

    pub fn decode(ty: ValType, bytes: &[u8]) -> Option<Value> {
        if let Some(w) = ty.fixed_width() {
            if bytes.len() != w {
                return None;
            }
        }
        Some(match ty {
            ValType::U8 => Value::U8(bytes[0]),
            ValType::I8 => Value::I8(bytes[0] as i8),
            ValType::U16 => Value::U16(u16::from_le_bytes([bytes[0], bytes[1]])),
            ValType::I16 => Value::I16(i16::from_le_bytes([bytes[0], bytes[1]])),
            ValType::U32 => Value::U32(u32::from_le_bytes(bytes[..4].try_into().ok()?)),
            ValType::I32 => Value::I32(i32::from_le_bytes(bytes[..4].try_into().ok()?)),
            ValType::F32 => Value::F32(f32::from_le_bytes(bytes[..4].try_into().ok()?)),
            ValType::U64 => Value::U64(u64::from_le_bytes(bytes[..8].try_into().ok()?)),
            ValType::I64 => Value::I64(i64::from_le_bytes(bytes[..8].try_into().ok()?)),
            ValType::F64 => Value::F64(f64::from_le_bytes(bytes[..8].try_into().ok()?)),
            ValType::Bytes => Value::Bytes(bytes.to_vec()),
            ValType::Cstr => {
                let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                Value::Cstr(String::from_utf8_lossy(&bytes[..end]).into_owned())
            }
        })
    }

    pub fn val_type(&self) -> ValType {
        match self {
            Value::U8(_) => ValType::U8, Value::U16(_) => ValType::U16,
            Value::U32(_) => ValType::U32, Value::U64(_) => ValType::U64,
            Value::I8(_) => ValType::I8, Value::I16(_) => ValType::I16,
            Value::I32(_) => ValType::I32, Value::I64(_) => ValType::I64,
            Value::F32(_) => ValType::F32, Value::F64(_) => ValType::F64,
            Value::Bytes(_) => ValType::Bytes, Value::Cstr(_) => ValType::Cstr,
        }
    }
}

/// Status codes shared by the WASM host (and future frontend) ABI.
pub mod status {
    pub const OK: i32 = 0;
    pub const ERR_UNREADABLE: i32 = -1;
    pub const ERR_UNWRITABLE: i32 = -2;
    pub const ERR_BAD_TYPE: i32 = -3;
    pub const ERR_BUF_TOO_SMALL: i32 = -4;
    pub const ERR_DENIED: i32 = -5;
    pub const CHANGED: i32 = 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_widths_are_correct() {
        assert_eq!(ValType::U8.fixed_width(), Some(1));
        assert_eq!(ValType::U32.fixed_width(), Some(4));
        assert_eq!(ValType::F64.fixed_width(), Some(8));
        assert_eq!(ValType::Bytes.fixed_width(), None);
        assert_eq!(ValType::Cstr.fixed_width(), None);
    }

    #[test]
    fn from_tag_round_trips_and_rejects_garbage() {
        assert_eq!(ValType::from_tag(2), Some(ValType::U32));
        assert_eq!(ValType::from_tag(11), Some(ValType::Cstr));
        assert_eq!(ValType::from_tag(99), None);
    }

    #[test]
    fn fixed_value_encode_decode_round_trips() {
        let v = Value::U32(0xDEADBEEF);
        let bytes = v.encode();
        assert_eq!(bytes, 0xDEADBEEFu32.to_le_bytes());
        assert_eq!(Value::decode(ValType::U32, &bytes), Some(Value::U32(0xDEADBEEF)));
    }

    #[test]
    fn float_round_trips() {
        let v = Value::F32(1.5);
        assert_eq!(Value::decode(ValType::F32, &v.encode()), Some(Value::F32(1.5)));
    }

    #[test]
    fn decode_rejects_wrong_length_for_fixed_type() {
        assert_eq!(Value::decode(ValType::U32, &[1, 2, 3]), None);
    }

    #[test]
    fn bytes_and_cstr_decode() {
        assert_eq!(Value::decode(ValType::Bytes, &[1, 2, 3]), Some(Value::Bytes(vec![1, 2, 3])));
        assert_eq!(Value::decode(ValType::Cstr, b"hi\0junk"), Some(Value::Cstr("hi".into())));
    }

    #[test]
    fn val_type_reports_back() {
        assert_eq!(Value::I64(-1).val_type(), ValType::I64);
    }
}

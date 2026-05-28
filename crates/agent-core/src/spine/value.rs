//! `MemValue`: the pure byte↔T conversion trait shared by every typed `mem::read`
//! / `mem::write` call site. Holding this in `agent-core` keeps it free of FFI
//! and host-testable on Linux. The agent-side `read<T>` / `write<T>` wrappers
//! (in `crates/agent/src/external/api.rs`) consume the trait and do the actual
//! validated memory IO.

use crate::mem_value::ValType;

/// A value that can be read from / written to process memory.
///
/// Variable-length values (`Bytes`, `Cstr`) are not `MemValue` impls — they
/// need a length argument that the trait shape can't carry. They live as free
/// functions on the agent side (`read_bytes_t`, `read_cstr_t`).
pub trait MemValue: Sized + Copy {
    const VAL_TYPE: ValType;

    /// Decode a value from a little-endian byte slice. Returns `None` if the
    /// slice is shorter than the type's width.
    fn from_le_bytes_spine(bytes: &[u8]) -> Option<Self>;

    /// Encode the value as a little-endian byte vector. Length is always
    /// `Self::VAL_TYPE.fixed_width().unwrap()`.
    fn to_le_bytes_buf(self) -> Vec<u8>;
}

macro_rules! impl_mem_value_numeric {
    ($t:ty, $vt:expr, $width:expr) => {
        impl MemValue for $t {
            const VAL_TYPE: ValType = $vt;
            #[inline]
            fn from_le_bytes_spine(bytes: &[u8]) -> Option<Self> {
                if bytes.len() < $width { return None; }
                let mut buf = [0u8; $width];
                buf.copy_from_slice(&bytes[..$width]);
                Some(<$t>::from_le_bytes(buf))
            }
            #[inline]
            fn to_le_bytes_buf(self) -> Vec<u8> {
                self.to_le_bytes().to_vec()
            }
        }
    };
}

impl_mem_value_numeric!(u8,  ValType::U8,  1);
impl_mem_value_numeric!(u16, ValType::U16, 2);
impl_mem_value_numeric!(u32, ValType::U32, 4);
impl_mem_value_numeric!(u64, ValType::U64, 8);
impl_mem_value_numeric!(i8,  ValType::I8,  1);
impl_mem_value_numeric!(i16, ValType::I16, 2);
impl_mem_value_numeric!(i32, ValType::I32, 4);
impl_mem_value_numeric!(i64, ValType::I64, 8);
impl_mem_value_numeric!(f32, ValType::F32, 4);
impl_mem_value_numeric!(f64, ValType::F64, 8);

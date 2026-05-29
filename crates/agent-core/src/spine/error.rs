//! Typed error model for the spine. Round-trips to the existing status codes
//! used at the WASM-host ABI boundary.

use crate::mem_value::status;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    Unreadable,
    Unwritable,
    BadType,
    BufTooSmall,
    Denied,
    Changed,
}

impl From<MemError> for i32 {
    fn from(e: MemError) -> i32 {
        match e {
            MemError::Unreadable  => status::ERR_UNREADABLE,
            MemError::Unwritable  => status::ERR_UNWRITABLE,
            MemError::BadType     => status::ERR_BAD_TYPE,
            MemError::BufTooSmall => status::ERR_BUF_TOO_SMALL,
            MemError::Denied      => status::ERR_DENIED,
            MemError::Changed     => status::CHANGED,
        }
    }
}

use crate::mem_value::ValType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvokeError {
    NotFound,
    ArgCountMismatch { expected: u8, got: u8 },
    ArgTypeMismatch { idx: u8, expected: ValType, got: ValType },
    NullInstance,
    MarshalFailed { idx: u8, reason: &'static str },
    ManagedException(String),
    InternalFailure(&'static str),
}

impl From<InvokeError> for i32 {
    fn from(e: InvokeError) -> i32 {
        match e {
            InvokeError::NotFound               => -100,
            InvokeError::ArgCountMismatch { .. } => -101,
            InvokeError::ArgTypeMismatch { .. }  => -102,
            InvokeError::NullInstance            => -103,
            InvokeError::MarshalFailed { .. }    => -104,
            InvokeError::ManagedException(_)     => -105,
            InvokeError::InternalFailure(_)      => -106,
        }
    }
}

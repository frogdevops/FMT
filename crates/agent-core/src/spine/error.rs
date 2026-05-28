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

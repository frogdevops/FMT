//! Trait-architecture spine: typed handles + capability markers + MemValue +
//! MemError. The structural backbone that lets the three Spec-2 domains
//! (mem / il2cpp / proto) compose by type rather than by raw u64 handoff.
//! See docs/superpowers/specs/2026-05-28-trait-spine-design.md.

pub mod addr;
pub mod error;
pub mod handles;
pub mod invoke_arg;
pub mod value;

pub use addr::{MemAddr, ReadOnly, ReadWrite};
pub use error::{HookError, InvokeError, MemError};
pub use handles::{FrameSeq, HookHandle, Instance, KlassPtr, MethodPtr, SocketHandle};
pub use invoke_arg::InvokeArg;
pub use value::MemValue;

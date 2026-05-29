//! Managed-method hook runtime. Per-method thunks emit machine code that jumps
//! to a universal shim; the shim captures arg registers into RegArgs and calls
//! into Rust. See docs/superpowers/specs/2026-05-29-invoke-hook-design.md
//! Sections 1, 3, 4c-f.
//!
//! INVARIANT (asserted in install/remove/dispatch):
//!     thunk_slot_N.embedded_id == N
//!     HOOK_SLOTS[N] holds the HookCtx for that method
//!     REENTRY[N] guards that method
//!     HookHandle::from_raw(N) is the script-visible ticket
//!   — ONE NUMBER from script to asm.

pub mod regargs;
pub mod shim;
pub mod replay;
pub mod thunks;
pub mod registry;
pub mod dispatcher;
pub mod api;

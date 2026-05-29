//! Layout-stable POD that the universal shim writes during reg capture and
//! the Rust dispatcher reads. Field offsets must match the shim asm in
//! `shim.rs` byte-for-byte — every change here is a change there.

use core::ffi::c_void;

#[repr(C)]
pub struct RegArgs {
    /// From R10 (set by the thunk).                   Offset 0
    pub method_id:  u64,
    /// RCX, RDX, R8, R9.                              Offsets 8..40
    pub int_args:   [u64; 4],
    /// XMM0..XMM3 (low 64 bits).                      Offsets 40..72
    pub float_args: [f64; 4],
    /// Pointer to caller's stack args (arg 5+).       Offset 72
    pub stack_args: *const u64,
    /// Loaded back into RAX on shim return.           Offset 80
    pub ret_int:    u64,
    /// Loaded back into XMM0 on shim return.          Offset 88
    pub ret_float:  f64,
}

// Compile-time guarantee that the shim's hardcoded offsets stay correct.
const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(RegArgs, method_id)  == 0);
    assert!(offset_of!(RegArgs, int_args)   == 8);
    assert!(offset_of!(RegArgs, float_args) == 40);
    assert!(offset_of!(RegArgs, stack_args) == 72);
    assert!(offset_of!(RegArgs, ret_int)    == 80);
    assert!(offset_of!(RegArgs, ret_float)  == 88);
    assert!(core::mem::size_of::<RegArgs>() == 96);
};

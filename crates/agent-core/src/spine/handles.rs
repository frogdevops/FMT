//! Domain-specific handle newtypes. Each wraps a `u64` with no runtime cost
//! (`#[repr(transparent)]`) and prevents accidental cross-domain confusion at
//! compile time (e.g. a `KlassPtr` cannot be passed where a `MemAddr` is
//! expected). None of these carry capability markers — there is no read/write
//! distinction on a klass, method, or frame sequence number.

macro_rules! handle_newtype {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(transparent)]
        pub struct $name(u64);

        impl $name {
            #[inline]
            pub fn from_raw(v: u64) -> Self { Self(v) }
            #[inline]
            pub fn as_u64(self) -> u64 { self.0 }
        }
    };
}

handle_newtype!(KlassPtr,     "An `Il2CppClass*` — the il2cpp class handle.");
handle_newtype!(MethodPtr,    "A `MethodInfo*` — the il2cpp method handle.");
handle_newtype!(Instance,     "An object instance pointer.");
handle_newtype!(FrameSeq,     "A bookmark into the protocol frame ring.");
handle_newtype!(SocketHandle, "A tracked WinSock socket (proto.send / inject).");
handle_newtype!(HookHandle,   "An active method-hook registration handle.");

use crate::mem_value::ValType;
use crate::spine::addr::{MemAddr, ReadWrite};

/// An il2cpp instance-field address with its known type. Distinct from
/// `MemAddr<ReadWrite>` because il2cpp field writes may need value-type
/// boxing semantics that raw memory writes don't. The type system carries
/// the field's `ValType` from `field_addr_t` construction through any
/// downstream `Write<T>` callsite, where `Write<T> for FieldAddr` can
/// verify `T::VAL_TYPE == self.val_type` at write time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldAddr {
    pub addr:     MemAddr<ReadWrite>,
    pub val_type: ValType,
}

impl FieldAddr {
    #[inline]
    pub fn new(addr: MemAddr<ReadWrite>, val_type: ValType) -> Self {
        Self { addr, val_type }
    }

    #[inline]
    pub fn addr(self) -> MemAddr<ReadWrite> { self.addr }

    #[inline]
    pub fn val_type(self) -> ValType { self.val_type }
}

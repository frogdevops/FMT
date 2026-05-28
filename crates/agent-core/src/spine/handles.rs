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

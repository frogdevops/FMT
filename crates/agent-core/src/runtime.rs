/// Raw, flat data read from the runtime — plain Rust, no FFI types,
/// so the dump pipeline can be exercised with a fake in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawField {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawClass {
    pub namespace: String,
    pub name: String,
    pub fields: Vec<RawField>,
}

/// Abstraction over the il2cpp runtime. The real implementation (in the
/// `agent` crate) calls the il2cpp C API; tests use a fake.
pub trait Il2CppRuntime {
    fn enumerate_classes(&self) -> Vec<RawClass>;
}

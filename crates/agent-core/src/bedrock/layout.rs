//! THE bedrock contract. Capabilities consume `Layout`; nothing else carries
//! offsets. Every field is a `Fact` — there is no raw `usize` a consumer could
//! read as a silent fallback. (Comments here describe what each fact IS used for
//! by mechanism — never its numeric value; values live in each Fact's Provenance.)

use crate::bedrock::Fact;

#[derive(Debug, Clone)]
pub struct Layout {
    /// Base address of the class metadata table in memory.
    pub table_base: Fact<usize>,
    /// Number of class entries in the metadata table.
    pub table_count: Fact<usize>,
    /// Stride between class pointer slots in the table (element spacing).
    pub class_table_step: Fact<usize>,
    /// Offset within a klass to its namespace cstring pointer.
    pub klass_namespace: Fact<usize>,
    /// Offset within a klass to its FieldInfo array pointer.
    pub klass_fields: Fact<usize>,
    /// Offset within a klass to its MethodInfo array pointer.
    pub klass_methods: Fact<usize>,
    /// Offset within a klass to its static fields data pointer.
    pub klass_static_fields: Fact<usize>,
    /// Offset within a klass to its TypeDefinition pointer.
    pub klass_type_def: Fact<usize>,
    /// Offset within a klass to its GenericClass pointer (for generic instantiations).
    pub klass_generic_class: Fact<usize>,
    /// Byte offset within a klass that contains the valuetype flag.
    pub klass_valuetype_off: Fact<usize>,
    /// Bit position within the valuetype byte that signals a value type.
    pub klass_valuetype_bit: Fact<u8>,
    /// Byte offset into the type's byval_arg chunk to read the type code.
    pub type_discrim_read_at: Fact<usize>,
    /// Bit shift to apply after reading to isolate the type code byte.
    pub discrim_shift: Fact<u8>,
    /// Offset within a MethodInfo to its native code pointer slot.
    pub method_pointer_off: Fact<usize>,
    /// Offset within a MethodInfo to its declaring-class back-pointer.
    pub method_klass_off: Fact<usize>,
    /// Offset within a MethodInfo to its name cstring pointer.
    pub method_name_off: Fact<usize>,
    /// Offset within a MethodInfo to its parameter count field.
    pub method_param_count_off: Fact<usize>,
    /// Offset within a MethodInfo to its return type pointer.
    pub method_return_type_off: Fact<usize>,
    /// Offset within a MethodInfo to its parameter array pointer.
    pub method_parameters_off: Fact<usize>,
    /// Offset within a MethodInfo to its flags field.
    pub method_flags_off: Fact<usize>,
    /// Size in bytes of a single ParameterInfo entry.
    pub param_info_size: Fact<usize>,
    /// Offset within a ParameterInfo to its type pointer.
    pub param_info_type_off: Fact<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::UnresolvedReason;
    #[test]
    fn unresolved_default_is_constructible() {
        let f: Fact<usize> = Fact::Unresolved { reason: UnresolvedReason::NoWitness };
        assert!(!f.is_resolved());
    }
}

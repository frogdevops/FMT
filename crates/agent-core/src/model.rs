#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpedField {
    pub name: String,
    pub type_name: String,
    /// Index into the runtime Il2CppType[] array (from metadata field definition).
    /// Populated only when the field came from a metadata parse.
    pub type_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpedClass {
    pub namespace: String,
    pub name: String,
    pub fields: Vec<DumpedField>,
    pub methods: Vec<String>,
    /// Index of this type's `Il2CppType` in the codegen types array
    /// (Il2CppMetadataRegistration.types). Extracted from the type
    /// definition's `byvalTypeIndex` field.
    pub type_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dump {
    pub classes: Vec<DumpedClass>,
}

impl Dump {
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    pub fn total_fields(&self) -> usize {
        self.classes.iter().map(|c| c.fields.len()).sum()
    }

    pub fn total_methods(&self) -> usize {
        self.classes.iter().map(|c| c.methods.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_methods() {
        let dump = Dump {
            classes: vec![DumpedClass {
                namespace: "Game".into(),
                name: "Player".into(),
                fields: vec![],
                methods: vec!["Update".into(), "Start".into()],
                type_index: 0,
            }],
        };
        assert_eq!(dump.total_methods(), 2);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpedField {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpedClass {
    pub namespace: String,
    pub name: String,
    pub fields: Vec<DumpedField>,
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
}

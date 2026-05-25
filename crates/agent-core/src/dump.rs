use crate::model::{Dump, DumpedClass, DumpedField};
use crate::runtime::Il2CppRuntime;

pub fn build_dump(rt: &dyn Il2CppRuntime) -> Dump {
    let mut classes: Vec<DumpedClass> = rt
        .enumerate_classes()
        .into_iter()
        .filter(|c| !c.name.starts_with('<')) // skip compiler-generated types
        .map(|c| DumpedClass {
            namespace: c.namespace,
            name: c.name,
            fields: c
                .fields
                .into_iter()
                .map(|f| DumpedField { name: f.name, type_name: f.type_name, type_index: None })
                .collect(),
            methods: Vec::new(),
            type_index: 0,
        })
        .collect();

    classes.sort_by(|a, b| {
        (a.namespace.as_str(), a.name.as_str()).cmp(&(b.namespace.as_str(), b.name.as_str()))
    });

    Dump { classes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DumpedClass, DumpedField};
    use crate::runtime::{Il2CppRuntime, RawClass, RawField};

    struct FakeRuntime {
        classes: Vec<RawClass>,
    }

    impl Il2CppRuntime for FakeRuntime {
        fn enumerate_classes(&self) -> Vec<RawClass> {
            self.classes.clone()
        }
    }

    #[test]
    fn builds_sorted_dump_skipping_compiler_generated() {
        let rt = FakeRuntime {
            classes: vec![
                RawClass { namespace: "Game".into(), name: "Player".into(), fields: vec![
                    RawField { name: "health".into(), type_name: "System.Int32".into() },
                ]},
                RawClass { namespace: String::new(), name: "<PrivateImplementationDetails>".into(), fields: vec![] },
                RawClass { namespace: "Game".into(), name: "Enemy".into(), fields: vec![] },
            ],
        };

        let dump = build_dump(&rt);

        // Compiler-generated `<...>` class is filtered out; rest sorted by (namespace, name).
        assert_eq!(dump.classes, vec![
            DumpedClass { namespace: "Game".into(), name: "Enemy".into(), fields: vec![], methods: vec![], type_index: 0 },
            DumpedClass { namespace: "Game".into(), name: "Player".into(), fields: vec![
                DumpedField { name: "health".into(), type_name: "System.Int32".into(), type_index: None },
            ], methods: vec![], type_index: 0 },
        ]);
    }
}

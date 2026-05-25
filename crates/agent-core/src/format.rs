use crate::model::Dump;

pub fn format_dump(dump: &Dump) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Unity internals dump\n# classes: {}, fields: {}\n\n",
        dump.class_count(),
        dump.total_fields()
    ));
    for class in &dump.classes {
        let full = if class.namespace.is_empty() {
            class.name.clone()
        } else {
            format!("{}.{}", class.namespace, class.name)
        };
        out.push_str(&format!("class {} {{\n", full));
        for field in &class.fields {
            out.push_str(&format!("    {} {};\n", field.type_name, field.name));
        }
        out.push_str("}\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dump, DumpedClass, DumpedField};

    #[test]
    fn formats_classes_and_fields() {
        let dump = Dump {
        classes: vec![DumpedClass {
            namespace: "Game".into(),
            name: "Player".into(),
            fields: vec![
                DumpedField { name: "health".into(), type_name: "System.Int32".into(), type_index: None },
                DumpedField { name: "mana".into(), type_name: "System.Single".into(), type_index: None },
            ],
            methods: vec![],
            type_index: 0,
        }],
        };

        let text = format_dump(&dump);

        let expected = "\
# Unity internals dump
# classes: 1, fields: 2

class Game.Player {
    System.Int32 health;
    System.Single mana;
}

";
        assert_eq!(text, expected);
    }

    #[test]
    fn omits_namespace_when_empty() {
        let dump = Dump {
        classes: vec![DumpedClass {
            namespace: "System".into(),
            name: "Object".into(),
            fields: vec![],
            methods: vec![],
            type_index: 0,
        }],
        };

        assert!(format_dump(&dump).contains("class System.Object {\n}"));
    }
}

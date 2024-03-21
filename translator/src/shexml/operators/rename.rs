use std::collections::HashMap;

use shexml_interpreter::{
    ExpressionStmt, FieldType, Iterator,
};

pub fn translate_rename_pairs_map(
    iterators_map: &HashMap<String, Iterator>,
    expr_stmt: &ExpressionStmt,
) -> HashMap<String, String> {
    let mut rename_pairs = HashMap::new();
    if let shexml_interpreter::ExpressionStmtEnum::Basic { reference } =
        &expr_stmt.expr_enum
    {
        let iter_ident = &reference.iterator_ident;
        let expr_ident = &expr_stmt.ident;

        if let Some(field) = &reference.field {
            let from = format!("{}.{}", iter_ident, field);
            let to = format!("{}.{}", expr_ident, field);

            rename_pairs.insert(from, to);
        } else if let Some(iterator) = iterators_map.get(iter_ident) {
            let normal_fields = iterator
                .fields
                .iter()
                .filter(|field| field.field_type == FieldType::Normal);

            normal_fields.for_each(|field| {
                let from = format!("{}.{}", iter_ident, field.ident);
                let to = format!("{}.{}", expr_ident, field.ident);
                rename_pairs.insert(from, to);
            })
        }
    }

    rename_pairs
}
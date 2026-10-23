//! Published JSON Schemas, generated from the public contract types.

use schemars::{Schema, generate::SchemaSettings};

/// The descriptor schema, including omission of effect-dependent principal defaults.
pub fn operation_schema() -> Schema {
    let mut schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<crate::Operation>();
    schema.insert("$id".into(), "https://raw.githubusercontent.com/thirtytwobits/clapham/main/crates/clapham/schemas/operation.schema.json".into());
    schema
}

/// The refusal schema, preserving contributor codes and the fixed categories.
pub fn refusal_schema() -> Schema {
    let mut schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<crate::Refusal>();
    schema.insert("$id".into(), "https://raw.githubusercontent.com/thirtytwobits/clapham/main/crates/clapham/schemas/refusal.schema.json".into());
    schema
}

//! Published JSON Schemas, generated from the public contract types.

use schemars::{Schema, generate::SchemaSettings};

/// The descriptor schema, including omission of effect-dependent auth defaults.
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

/// The catalogue schema: a descriptor array, or one descriptor for a named lookup.
pub fn catalogue_schema() -> Schema {
    let mut schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<crate::Catalogue>();
    schema.insert("$id".into(), "https://raw.githubusercontent.com/thirtytwobits/clapham/main/crates/clapham/schemas/catalogue.schema.json".into());
    schema
}

/// Declarative configuration, sharing the programmatic composition model.
pub fn configuration_schema() -> Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<crate::Configuration>()
}

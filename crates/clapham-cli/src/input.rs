use std::collections::BTreeSet;
use std::io::Read;

use clap::ArgMatches;
use clapham::{Operation, Refusal};
use serde_json::{Map, Value};

use crate::{
    configuration,
    tree::{self, Node},
    usage,
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Kind {
    String,
    Integer,
    Number,
    Boolean,
    Json,
}

impl Kind {
    pub(crate) fn value_name(self) -> &'static str {
        match self {
            Self::String => "STRING",
            Self::Integer => "INTEGER",
            Self::Number => "NUMBER",
            Self::Boolean => "true|false",
            Self::Json => "JSON",
        }
    }

    pub(crate) fn numeric(self) -> bool {
        matches!(self, Self::Integer | Self::Number)
    }

    fn parse(self, text: &str) -> Result<Value, Refusal> {
        if matches!(self, Self::String) {
            return Ok(Value::String(text.into()));
        }
        let value: Value = serde_json::from_str(text).map_err(|error| usage(error.to_string()))?;
        let valid = match self {
            Self::String => unreachable!(),
            Self::Integer => {
                value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n| n.fract() == 0.0)
            }
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Json => true,
        };
        if valid {
            Ok(value)
        } else {
            Err(usage(format!("expected {}", self.value_name())))
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Field {
    pub(crate) property: String,
    pub(crate) id: String,
    pub(crate) flag: String,
    pub(crate) help: String,
    pub(crate) kind: Kind,
    pub(crate) required: bool,
    pub(crate) position: Option<usize>,
}

pub(crate) fn fields(operation: &Operation) -> Result<Vec<Field>, Refusal> {
    let root = Value::Object(operation.input.clone());
    let properties = operation.input.get("properties").and_then(Value::as_object);
    let required = operation.input.get("required").and_then(Value::as_array);
    let mut positional = BTreeSet::new();
    let mut optional_seen = false;
    for property in &operation.positional {
        if !properties.is_some_and(|properties| properties.contains_key(property))
            || !positional.insert(property)
        {
            return Err(configuration(format!(
                "{} has an unknown or repeated positional property {property}",
                operation.name
            )));
        }
        if required.is_some_and(|names| names.iter().any(|name| name == property)) {
            if optional_seen {
                return Err(configuration(format!(
                    "{} has a required positional property {property} after an optional one",
                    operation.name
                )));
            }
        } else {
            optional_seen = true;
        }
    }
    let mut fields = Vec::new();
    for (property, schema) in properties.into_iter().flatten() {
        let resolved = resolve(schema, &root);
        let schema_type = schema
            .get("type")
            .or_else(|| resolved.get("type"))
            .and_then(Value::as_str);
        let kind = match schema_type {
            Some("string") => Kind::String,
            Some("integer") => Kind::Integer,
            Some("number") => Kind::Number,
            Some("boolean") => Kind::Boolean,
            _ => match resolved.get("enum").and_then(Value::as_array) {
                Some(values) if !values.is_empty() && values.iter().all(Value::is_string) => {
                    Kind::String
                }
                Some(values) if !values.is_empty() && values.iter().all(Value::is_boolean) => {
                    Kind::Boolean
                }
                Some(values) if !values.is_empty() && values.iter().all(Value::is_number) => {
                    Kind::Number
                }
                _ => Kind::Json,
            },
        };
        let mut help = schema
            .get("description")
            .or_else(|| resolved.get("description"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if let Some(choices) = resolved.get("enum") {
            help.push_str(&format!(" [values: {choices}]"));
        }
        let position = operation
            .positional
            .iter()
            .position(|name| name == property)
            .map(|index| index + 1);
        fields.push(Field {
            property: property.clone(),
            id: format!("input:{property}"),
            flag: if matches!(kind, Kind::Json) {
                format!("{property}-json")
            } else {
                property.clone()
            },
            help,
            kind,
            required: required.is_some_and(|names| names.iter().any(|name| name == property)),
            position,
        });
    }
    tree::check_flags(&fields)?;
    Ok(fields)
}

// Follow local JSON Pointer references without recursion or network access. A
// cyclic or complex schema remains expressible through a JSON-valued argument.
fn resolve<'a>(mut schema: &'a Value, root: &'a Value) -> &'a Value {
    let mut seen = BTreeSet::new();
    while let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if !seen.insert(reference) {
            break;
        }
        let Some(target) = reference
            .strip_prefix('#')
            .and_then(|pointer| root.pointer(pointer))
        else {
            break;
        };
        schema = target;
    }
    schema
}

pub(crate) fn bind(
    node: &Node,
    matches: &ArgMatches,
    mut stdin: impl Read,
    limit: usize,
) -> Result<Value, Refusal> {
    if let Some(document) = matches.get_one::<String>(tree::JSON) {
        let mut bytes = Vec::new();
        let document = if document == "-" {
            stdin
                .by_ref()
                .take((limit as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|error| usage(format!("cannot read JSON input: {error}")))?;
            if bytes.len() > limit {
                return Err(usage("JSON input exceeds bounds.message_bytes"));
            }
            std::str::from_utf8(&bytes).map_err(|error| usage(error.to_string()))?
        } else {
            document
        };
        if document.len() > limit {
            return Err(usage("JSON input exceeds bounds.message_bytes"));
        }
        return serde_json::from_str(document).map_err(|error| usage(error.to_string()));
    }
    let mut input = Map::new();
    for field in &node.fields {
        if let Some(value) = matches.get_one::<String>(&field.id) {
            let value = field
                .kind
                .parse(value)
                .map_err(|error| usage(format!("{}: {}", field.property, error.message)))?;
            input.insert(field.property.clone(), value);
        }
    }
    Ok(Value::Object(input))
}

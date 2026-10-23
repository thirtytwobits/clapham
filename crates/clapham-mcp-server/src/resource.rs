use clapham::{Effect, Operation, Refusal};
use percent_encoding::percent_decode_str;
use serde_json::{Map, Value};

#[derive(Debug)]
pub(crate) struct Template {
    literals: Vec<String>,
    variables: Vec<String>,
}

impl Template {
    pub(crate) fn parse(op: &Operation) -> Result<Self, Refusal> {
        let invalid = || {
            super::configuration(format!(
                "{}: resources require a query and unambiguous simple string URI-template variables",
                op.name
            ))
        };
        if op.effect != Effect::Query {
            return Err(invalid());
        }
        let mut rest = op.resource.as_deref().ok_or_else(invalid)?;
        let mut literals = Vec::new();
        let mut variables = Vec::new();
        while let Some((literal, expression)) = rest.split_once('{') {
            if literal.contains('}')
                || (!variables.is_empty()
                    && (literal.is_empty()
                        || literal.starts_with(|c: char| {
                            c.is_ascii_alphanumeric() || "_.~-%".contains(c)
                        })))
            {
                return Err(invalid());
            }
            let (variable, remaining) = expression.split_once('}').ok_or_else(invalid)?;
            if variable.is_empty()
                || !variable
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                || variables.iter().any(|v| v == variable)
            {
                return Err(invalid());
            }
            let property = op.input.get("properties").and_then(|v| v.get(variable));
            if property.and_then(|v| v.get("type")).and_then(Value::as_str) != Some("string") {
                return Err(invalid());
            }
            literals.push(literal.into());
            variables.push(variable.into());
            rest = remaining;
        }
        if rest.contains('}')
            || (!variables.is_empty()
                && rest.starts_with(|c: char| c.is_ascii_alphanumeric() || "_.~-%".contains(c)))
        {
            return Err(invalid());
        }
        literals.push(rest.into());
        Ok(Self {
            literals,
            variables,
        })
    }

    // Variable names do not distinguish the set of URIs a template can match.
    pub(crate) fn shape(&self) -> &[String] {
        &self.literals
    }

    pub(crate) fn bind(&self, uri: &str) -> Option<Map<String, Value>> {
        let mut rest = uri.strip_prefix(&self.literals[0])?;
        let mut input = Map::new();
        for (index, variable) in self.variables.iter().enumerate() {
            let next = &self.literals[index + 1];
            let (encoded, remaining) = if next.is_empty() {
                (rest, "")
            } else {
                rest.split_once(next)?
            };
            // Simple expansion only emits unreserved characters and percent escapes.
            let bytes = encoded.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'%' if i + 2 < bytes.len()
                        && bytes[i + 1].is_ascii_hexdigit()
                        && bytes[i + 2].is_ascii_hexdigit() =>
                    {
                        i += 3
                    }
                    b if b.is_ascii_alphanumeric() || b"-._~".contains(&b) => i += 1,
                    _ => return None,
                }
            }
            let decoded = percent_decode_str(encoded).decode_utf8().ok()?.into_owned();
            input.insert(variable.clone(), Value::String(decoded));
            rest = remaining;
        }
        if rest.is_empty() { Some(input) } else { None }
    }
}

use clapham::{Effect, Operation, Refusal};
use percent_encoding::percent_decode_str;
use serde_json::{Map, Value};

/// An RFC 6570 template of simple path expressions and an optional trailing form-style query
/// expression, `{?a,b}` or `{?a,rest*}`.
#[derive(Debug)]
pub(crate) struct Template {
    literals: Vec<String>,
    variables: Vec<String>,
    query: Option<Query>,
}

/// Optional string properties bound by name, and an optional object property bound from every
/// other parameter.
#[derive(Debug)]
struct Query {
    names: Vec<String>,
    explode: Option<String>,
}

impl Template {
    pub(crate) fn parse(op: &Operation, uri: &str) -> Result<Self, Refusal> {
        let invalid = || {
            super::configuration(format!(
                "{}: resources require a query and unambiguous URI-template variables naming \
                 string path properties, optional string query properties or an optional \
                 string-valued object",
                op.name
            ))
        };
        if op.effect != Effect::Query {
            return Err(invalid());
        }
        let properties = op.input.get("properties");
        let required = |name: &str| {
            op.input
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|required| required.iter().any(|v| v == name))
        };
        let mut rest = uri;
        let mut literals: Vec<String> = Vec::new();
        let mut variables: Vec<String> = Vec::new();
        let mut query = None;
        while let Some((literal, expression)) = rest.split_once('{') {
            let (expression, remaining) = expression.split_once('}').ok_or_else(invalid)?;
            let is_query = expression.starts_with('?');
            if literal.contains('}')
                || (!variables.is_empty()
                    && ((literal.is_empty() && !is_query)
                        || literal.starts_with(|c: char| {
                            c.is_ascii_alphanumeric() || "_.~-%".contains(c)
                        })))
            {
                return Err(invalid());
            }
            literals.push(literal.into());
            rest = remaining;
            if is_query {
                // A form-style query expression ends the template, so the literal before it ends
                // the path. The path must not contain `?`.
                if !rest.is_empty() || literals.iter().any(|literal| literal.contains('?')) {
                    return Err(invalid());
                }
                let mut parsed = Query {
                    names: Vec::new(),
                    explode: None,
                };
                for item in expression[1..].split(',') {
                    let (name, explode) = match item.strip_suffix('*') {
                        Some(name) => (name, true),
                        None => (item, false),
                    };
                    if !simple_name(name)
                        || variables.iter().any(|v| v == name)
                        || parsed.names.iter().any(|v| v == name)
                        || parsed.explode.as_deref() == Some(name)
                        || required(name)
                    {
                        return Err(invalid());
                    }
                    let property = properties.and_then(|v| v.get(name));
                    if explode {
                        if parsed.explode.is_some() || !property.is_some_and(string_map) {
                            return Err(invalid());
                        }
                        parsed.explode = Some(name.into());
                    } else {
                        if !property.is_some_and(|schema| admits(schema, "string")) {
                            return Err(invalid());
                        }
                        parsed.names.push(name.into());
                    }
                }
                query = Some(parsed);
                break;
            }
            if !simple_name(expression) || variables.iter().any(|v| v == expression) {
                return Err(invalid());
            }
            let property = properties.and_then(|v| v.get(expression));
            if property.and_then(|v| v.get("type")).and_then(Value::as_str) != Some("string") {
                return Err(invalid());
            }
            variables.push(expression.into());
        }
        if query.is_none() {
            if rest.contains('}')
                || (!variables.is_empty()
                    && rest.starts_with(|c: char| c.is_ascii_alphanumeric() || "_.~-%".contains(c)))
            {
                return Err(invalid());
            }
            literals.push(rest.into());
        }
        Ok(Self {
            literals,
            variables,
            query,
        })
    }

    // Variable names do not distinguish the set of URIs a template can match. Every query
    // variable is optional, so a query template also matches each URI of its path alone.
    pub(crate) fn shape(&self) -> &[String] {
        &self.literals
    }

    pub(crate) fn bind(&self, uri: &str) -> Option<Map<String, Value>> {
        let (path, parameters) = match &self.query {
            Some(query) => match uri.split_once('?') {
                Some((path, parameters)) => (path, Some((query, parameters))),
                None => (uri, None),
            },
            None => (uri, None),
        };
        let mut rest = path.strip_prefix(&self.literals[0])?;
        let mut input = Map::new();
        for (index, variable) in self.variables.iter().enumerate() {
            let next = &self.literals[index + 1];
            let (encoded, remaining) = if next.is_empty() {
                (rest, "")
            } else {
                rest.split_once(next)?
            };
            input.insert(variable.clone(), Value::String(decode(encoded)?));
            rest = remaining;
        }
        if !rest.is_empty() {
            return None;
        }
        if let Some((query, parameters)) = parameters {
            // Form-style expansion emits `?` only before a parameter, and `name=` for an empty value.
            let mut exploded = Map::new();
            for parameter in parameters.split('&') {
                let (name, value) = parameter.split_once('=')?;
                let (name, value) = (decode(name)?, Value::String(decode(value)?));
                if name.is_empty() {
                    return None;
                }
                let previous = if query.names.contains(&name) {
                    input.insert(name, value)
                } else if query.explode.is_some() {
                    exploded.insert(name, value)
                } else {
                    return None;
                };
                if previous.is_some() {
                    return None;
                }
            }
            if let Some(explode) = &query.explode {
                // An empty associative array is undefined and expands to nothing.
                if !exploded.is_empty() {
                    input.insert(explode.clone(), Value::Object(exploded));
                }
            }
        }
        Some(input)
    }
}

fn simple_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether a schema's `type` is `ty` or a list including it, such as an optional field's.
fn admits(schema: &Value, ty: &str) -> bool {
    match schema.get("type") {
        Some(Value::String(t)) => t == ty,
        Some(Value::Array(types)) => types.iter().any(|t| t == ty),
        _ => false,
    }
}

/// An object whose declared and additional properties are all strings.
fn string_map(schema: &Value) -> bool {
    admits(schema, "object")
        && schema.get("patternProperties").is_none()
        && schema
            .get("additionalProperties")
            .is_some_and(|additional| admits(additional, "string"))
        && schema
            .get("properties")
            .and_then(Value::as_object)
            .is_none_or(|properties| properties.values().all(|p| admits(p, "string")))
}

/// Simple and form-style expansion emit only unreserved characters and percent escapes.
fn decode(encoded: &str) -> Option<String> {
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
    Some(percent_decode_str(encoded).decode_utf8().ok()?.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn operation(input: Value) -> Operation {
        serde_json::from_value(json!({
            "name":"items.find", "description":"Find an item.", "input":input,
            "effect":"query", "locality":"workspace", "idempotent":true, "open_world":false
        }))
        .unwrap()
    }

    fn items() -> Operation {
        operation(json!({
            "type":"object",
            "properties":{
                "list_id":{"type":"string"},
                "id":{"type":"string"},
                "text":{"type":"string"},
                "state":{"type":["string","null"]},
                "labels":{"type":"object","additionalProperties":{"type":"string"}},
                "count":{"type":"integer"},
                "nested":{"type":"object","additionalProperties":{"type":"object"}}
            },
            "required":["list_id","id"]
        }))
    }

    fn bind(uri_template: &str, uri: &str) -> Option<Value> {
        Template::parse(&items(), uri_template)
            .unwrap()
            .bind(uri)
            .map(Value::Object)
    }

    #[test]
    fn path_templates_bind_as_before() {
        assert_eq!(
            bind("items://{list_id}/{id}", "items://tasks/a%20b"),
            Some(json!({"list_id":"tasks","id":"a b"}))
        );
        assert_eq!(
            bind("items://{list_id}/{id}", "items://tasks/a?text=x"),
            None
        );
        assert_eq!(bind("items://{list_id}/{id}", "items://tasks"), None);
    }

    #[test]
    fn query_variables_bind_optional_strings_in_any_order() {
        let template = "items://{list_id}/{id}{?text,state}";
        assert_eq!(
            bind(template, "items://tasks/a"),
            Some(json!({"list_id":"tasks","id":"a"}))
        );
        assert_eq!(
            bind(template, "items://tasks/a?state=open&text=50%25"),
            Some(json!({"list_id":"tasks","id":"a","state":"open","text":"50%"}))
        );
        assert_eq!(
            bind(template, "items://tasks/a?text="),
            Some(json!({"list_id":"tasks","id":"a","text":""}))
        );
    }

    #[test]
    fn exploded_query_binds_remaining_parameters_into_an_object() {
        let template = "items://{list_id}/{id}{?text,labels*}";
        assert_eq!(
            bind(template, "items://tasks/a?colour=red&text=x&size%20class=l"),
            Some(json!({
                "list_id":"tasks","id":"a","text":"x",
                "labels":{"colour":"red","size class":"l"}
            }))
        );
        assert_eq!(
            bind("items://{list_id}/{id}{?labels*}", "items://tasks/a"),
            Some(json!({"list_id":"tasks","id":"a"}))
        );
    }

    #[test]
    fn unbound_repeated_or_malformed_parameters_do_not_match() {
        let template = "items://{list_id}/{id}{?text}";
        for uri in [
            "items://tasks/a?state=open",
            "items://tasks/a?text=x&text=y",
            "items://tasks/a?",
            "items://tasks/a?text",
            "items://tasks/a?text=x&",
            "items://tasks/a?text=a+b",
            "items://tasks/a?text=%ZZ",
            "items://tasks/a?=x",
        ] {
            assert_eq!(bind(template, uri), None, "{uri}");
        }
        assert_eq!(
            bind(
                "items://{list_id}/{id}{?labels*}",
                "items://tasks/a?red=1&red=2"
            ),
            None
        );
    }

    #[test]
    fn query_variables_must_name_optional_strings_or_a_string_map() {
        for template in [
            "items://{list_id}{?id}",
            "items://{list_id}/{id}{?count}",
            "items://{list_id}/{id}{?absent}",
            "items://{list_id}/{id}{?labels}",
            "items://{list_id}/{id}{?text*}",
            "items://{list_id}/{id}{?nested*}",
            "items://{list_id}/{id}{?labels*,state*}",
            "items://{list_id}/{id}{?text,text}",
            "items://{list_id}/{id}{?list_id}",
            "items://{list_id}/{id}{?}",
            "items://{list_id}/{id}{?text:3}",
            "items://{list_id}/{id}{?text}/more",
            "items://{list_id}/{id}{?text}{?state}",
            "items://{list_id}?x/{id}{?text}",
            "items://{list_id}/{id*}",
            "items://{list_id}/{?id}",
        ] {
            assert!(Template::parse(&items(), template).is_err(), "{template}");
        }
        assert!(Template::parse(&items(), "items://{list_id}/{id}{?text,state,labels*}").is_ok());
        assert_eq!(
            bind("items://all{?text}", "items://all?text=x"),
            Some(json!({"text":"x"}))
        );
    }

    #[test]
    fn query_templates_share_the_shape_of_their_path() {
        let path = Template::parse(&items(), "items://{list_id}/{id}").unwrap();
        let query = Template::parse(&items(), "items://{list_id}/{id}{?text}").unwrap();
        assert_eq!(path.shape(), query.shape());
    }
}

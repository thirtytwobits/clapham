use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;

use clapham::{Category, Evidence, Operation, OperationName, Outcome, Refusal};
use serde_json::{Value, json};

use crate::{Cli, Format, OutputOptions, configuration};

/// The version-one JSON frame used for both ordered records and the final result.
/// `kind` is the complete operation name; `data` retains the operation's JSON value.
pub fn envelope_schema() -> schemars::Schema {
    schemars::json_schema!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "title":"CLI output envelope",
        "type":"object", "properties":{
            "v":{"const":1}, "kind":schemars::schema_for!(OperationName), "data":true
        }, "required":["v","kind","data"], "additionalProperties":false
    })
}

/// Write exactly one compact refusal line to stderr, returning its category's exit code.
/// A failed stderr write cannot be reported on that same stream; the original exit
/// code is still returned. The refusal's fields are serialised unchanged.
pub fn write_refusal(refusal: &Refusal, mut stderr: impl Write) -> u8 {
    let mut line = serde_json::to_vec(refusal).expect("refusals contain JSON values");
    line.push(b'\n');
    let _ = stderr.write_all(&line).and_then(|()| stderr.flush());
    refusal.category.exit_code()
}

type Renderer = dyn Fn(&Operation, &Value, bool) -> String + Send + Sync;

#[derive(Clone, Default)]
pub(crate) struct Renderers(BTreeMap<String, Arc<Renderer>>);

impl fmt::Debug for Renderers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.0.keys()).finish()
    }
}

impl Cli {
    /// Register a human renderer for one operation. Its arguments are the descriptor,
    /// the record or result data, and whether this is a record. JSON output is unaffected.
    /// Unknown names refuse as `cli_configuration`; registering again replaces a renderer.
    pub fn with_renderer<F>(mut self, name: &str, renderer: F) -> Result<Self, Refusal>
    where
        F: Fn(&Operation, &Value, bool) -> String + Send + Sync + 'static,
    {
        if self.set.get(name).is_none() {
            return Err(configuration("a renderer must name a composed operation"));
        }
        self.renderers.0.insert(name.into(), Arc::new(renderer));
        Ok(self)
    }
}

pub(crate) fn result_data(outcome: Outcome) -> Value {
    // Ordinary JSON results keep their declared shape. Preserve the complete
    // outcome when it carries additional content, an error or non-default evidence,
    // or distinguishes an absent structured result from an explicit JSON null.
    if outcome.content.is_none() && !outcome.is_error && outcome.evidence == Evidence::Completed {
        if let Some(result) = outcome.result.as_ref() {
            return result.clone();
        }
    }
    serde_json::to_value(outcome).expect("outcomes contain JSON values")
}

pub(crate) struct Presentation {
    pub(crate) operation: Operation,
    pub(crate) options: OutputOptions,
    pub(crate) terminal: bool,
    pub(crate) renderers: Renderers,
}

impl Presentation {
    pub(crate) fn render(&self, data: Value, record: bool) -> Vec<u8> {
        let format = match self.options.format {
            Format::Auto if self.terminal => Format::Human,
            Format::Auto => Format::Ndjson,
            format => format,
        };
        let mut text = if format == Format::Human {
            match self.renderers.0.get(self.operation.name.as_str()) {
                Some(renderer) => renderer(&self.operation, &data, record),
                None => human(
                    &data,
                    if record {
                        None
                    } else {
                        self.operation
                            .output
                            .as_ref()
                            .map(|schema| schema.as_value())
                    },
                ),
            }
        } else {
            let frame = if self.options.raw {
                data
            } else {
                json!({"v":1,"kind":self.operation.name,"data":data})
            };
            if format == Format::Json && self.terminal && !self.operation.streams {
                serde_json::to_string_pretty(&frame)
            } else {
                serde_json::to_string(&frame)
            }
            .expect("frames contain JSON values")
        };
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.into_bytes()
    }
}

pub(crate) fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(bytes)?;
    writer.flush()
}

pub(crate) fn output_failed(error: io::Error, evidence: Evidence) -> Refusal {
    Refusal::new("output_failed", Category::Unavailable, error.to_string()).with_evidence(evidence)
}

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

fn label(key: &str, schema: Option<&Value>) -> String {
    let label = schema
        .and_then(|schema| schema.get("properties"))
        .and_then(|properties| properties.get(key))
        .and_then(|schema| schema.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(key);
    clean(label)
}

fn clean(text: &str) -> String {
    text.chars()
        .flat_map(|character| {
            if character.is_control() {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
}

fn cell(value: &Value) -> String {
    match value {
        Value::String(text) => clean(text),
        _ => value.to_string(),
    }
}

fn human(data: &Value, root: Option<&Value>) -> String {
    let schema = root.map(|root| resolve(root, root));
    if let Value::Array(rows) = data {
        let items = schema
            .and_then(|schema| schema.get("items"))
            .map(|items| resolve(items, root.expect("a schema has a root")));
        if rows.iter().all(Value::is_object)
            && (!rows.is_empty()
                || items.is_some_and(|items| {
                    items.get("type").and_then(Value::as_str) == Some("object")
                }))
        {
            let mut columns = BTreeSet::new();
            if let Some(properties) = items
                .and_then(|items| items.get("properties"))
                .and_then(Value::as_object)
            {
                columns.extend(properties.keys().cloned());
            }
            for row in rows {
                columns.extend(row.as_object().expect("rows are objects").keys().cloned());
            }
            if !columns.is_empty() {
                let columns: Vec<_> = columns.into_iter().collect();
                let headers: Vec<_> = columns.iter().map(|key| label(key, items)).collect();
                let values: Vec<Vec<_>> = rows
                    .iter()
                    .map(|row| {
                        columns
                            .iter()
                            .map(|key| row.get(key).map(cell).unwrap_or_default())
                            .collect()
                    })
                    .collect();
                let widths: Vec<_> = headers
                    .iter()
                    .enumerate()
                    .map(|(index, header)| {
                        values
                            .iter()
                            .map(|row| row[index].chars().count())
                            .max()
                            .unwrap_or(0)
                            .max(header.chars().count())
                    })
                    .collect();
                let line = |row: &[String]| {
                    row.iter()
                        .zip(&widths)
                        .map(|(value, width)| {
                            format!("{value}{}", " ".repeat(width - value.chars().count()))
                        })
                        .collect::<Vec<_>>()
                        .join("  ")
                        .trim_end()
                        .to_owned()
                };
                let mut lines = vec![
                    line(&headers),
                    widths
                        .iter()
                        .map(|width| "-".repeat(*width))
                        .collect::<Vec<_>>()
                        .join("  "),
                ];
                lines.extend(values.iter().map(|row| line(row)));
                return lines.join("\n");
            }
        }
    }
    if let Value::Object(fields) = data {
        return fields
            .iter()
            .map(|(key, value)| format!("{}: {}", label(key, schema), cell(value)))
            .collect::<Vec<_>>()
            .join("\n");
    }
    cell(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_tables_use_referenced_schema_columns_titles_and_escape_control_characters() {
        let schema = json!({"type":"array","items":{"$ref":"#/$defs/Item"},"$defs":{"Item":{
            "type":"object","properties":{"count":{"type":"integer","title":"Item count"},"text":{"type":"string"}}
        }}});
        let text = human(
            &json!([{"count":2,"text":"Read\n\u{1b}[31m"}]),
            Some(&schema),
        );
        assert!(text.starts_with("Item count"));
        assert!(text.contains(r"Read\n\u{1b}[31m"));
        assert!(!text.contains('\u{1b}'));
        assert!(human(&json!([]), Some(&schema)).starts_with("Item count"));
        assert_eq!(
            human(
                &json!({"count":2}),
                Some(&json!({"type":"object","properties":{"count":{"title":"Item count"}}}))
            ),
            "Item count: 2"
        );
    }

    #[test]
    fn absent_result_and_explicit_null_keep_distinct_output_data() {
        assert_eq!(result_data(Outcome::new(Value::Null)), Value::Null);
        let absent = Outcome {
            result: None,
            content: None,
            is_error: false,
            evidence: Evidence::Completed,
        };
        assert_eq!(
            result_data(absent),
            json!({"is_error":false,"evidence":"completed"})
        );
    }
}

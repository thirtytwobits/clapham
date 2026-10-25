use std::io::{self, Write};
use std::sync::Arc;

use clapham::{Category, Effect, Evidence, Operation, Outcome, Refusal};
use rmcp::{ErrorData, model::*};
use serde::Serialize;
use serde_json::{Map, Value, json};

pub(crate) fn tool(op: &Operation, schema_bytes: usize) -> Result<Tool, ErrorData> {
    let mut tool = Tool::new(
        op.name.to_string(),
        op.description.clone(),
        Arc::new(op.input.clone()),
    );
    tool.title = op.title.clone();
    tool.annotations = Some(ToolAnnotations::from_raw(
        None,
        Some(op.effect == Effect::Query),
        Some(op.effect == Effect::Destructive),
        Some(op.effect == Effect::Query || op.idempotent),
        Some(op.open_world),
    ));
    if op.effect == Effect::Administrative {
        tool.meta = Some(
            json!({crate::EFFECT_META_KEY:"administrative"})
                .as_object()
                .unwrap()
                .clone()
                .into(),
        );
    }
    if let Some(schema) = &op.output {
        // rmcp's Tool stores an object schema. Boolean schemas have exact object
        // equivalents; this preserves their validation semantics without loss.
        let schema = match schema.as_value() {
            Value::Bool(true) => Map::new(),
            Value::Bool(false) => json!({"not":{}}).as_object().unwrap().clone(),
            Value::Object(object) => object.clone(),
            _ => unreachable!("JSON Schema has an object or boolean root"),
        };
        if size(&schema, schema_bytes).is_none() {
            return Err(ErrorData::internal_error(
                "output schema exceeds connection bounds",
                None,
            ));
        }
        tool.output_schema = Some(Arc::new(schema));
    }
    Ok(tool)
}

pub(crate) fn refusal(refusal: Refusal) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(refusal.message.clone())]);
    result.structured_content = Some(serde_json::to_value(refusal).expect("refusals serialize"));
    result
}

pub(crate) fn outcome(outcome: Outcome) -> Result<CallToolResult, Refusal> {
    let mut result = CallToolResult::success(
        outcome
            .content
            .unwrap_or_default()
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<ContentBlock>, _>>()
            .map_err(|error| {
                Refusal::new("invalid_result", Category::Internal, error.to_string())
                    .with_evidence(outcome.evidence)
            })?,
    );
    result.structured_content = outcome.result;
    result.is_error = Some(outcome.is_error);
    result.meta = Some(
        serde_json::json!({"io.github.thirtytwobits.clapham/evidence": outcome.evidence})
            .as_object()
            .expect("metadata object")
            .clone()
            .into(),
    );
    Ok(result)
}

pub(crate) fn size<T: Serialize + ?Sized>(value: &T, limit: usize) -> Option<usize> {
    struct Counter {
        size: usize,
        limit: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.size = self
                .size
                .checked_add(bytes.len())
                .filter(|n| *n <= self.limit)
                .ok_or_else(|| io::Error::other("message exceeds bounds"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { size: 0, limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.size)
}

pub(crate) fn too_large() -> Refusal {
    Refusal::new(
        "result_too_large",
        Category::Usage,
        "the encoded result exceeds bounds.message_bytes",
    )
    .with_evidence(Evidence::Uncertain)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_effect_maps_to_native_rmcp_annotations_and_metadata() {
        for effect in [
            Effect::Query,
            Effect::Mutation,
            Effect::Administrative,
            Effect::Destructive,
        ] {
            for idempotent in [false, true] {
                for open_world in [false, true] {
                    let op:Operation=serde_json::from_value(json!({"name":"lists.count","description":"Count lists.","input":{"type":"object"},"effect":effect,"locality":"workspace","idempotent":idempotent,"open_world":open_world})).unwrap();
                    let tool = serde_json::to_value(tool(&op, 8192).unwrap()).unwrap();
                    assert_eq!(tool["annotations"]["readOnlyHint"], effect == Effect::Query);
                    assert_eq!(
                        tool["annotations"]["destructiveHint"],
                        effect == Effect::Destructive
                    );
                    assert_eq!(
                        tool["annotations"]["idempotentHint"],
                        effect == Effect::Query || idempotent
                    );
                    assert_eq!(tool["annotations"]["openWorldHint"], open_world);
                    assert_eq!(
                        tool["_meta"][crate::EFFECT_META_KEY].as_str(),
                        if effect == Effect::Administrative {
                            Some("administrative")
                        } else {
                            None
                        }
                    );
                }
            }
        }
    }
}

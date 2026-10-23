use std::io::{self, Write};
use std::sync::Arc;

use clapham::{
    Category, ContentBlock as CoreContent, Evidence, Operation, Outcome, Refusal,
    ResourceContents as CoreResource,
};
use rmcp::{ErrorData, model::*};
use serde::Serialize;
use serde_json::{Map, Value, json};

pub(crate) fn tool(op: &Operation, schema_bytes: usize) -> Result<Tool, ErrorData> {
    let hints = op.annotations();
    let mut tool = Tool::new(
        op.name.to_string(),
        op.description.clone(),
        Arc::new(op.input.clone()),
    );
    tool.title = op.title.clone();
    tool.annotations = Some(ToolAnnotations::from_raw(
        None,
        Some(hints.read_only_hint),
        Some(hints.destructive_hint),
        Some(hints.idempotent_hint),
        Some(hints.open_world_hint),
    ));
    if !hints.meta.is_empty() {
        tool.meta = Some(hints.meta.into_iter().collect::<Map<_, _>>().into());
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

pub(crate) fn outcome(outcome: Outcome) -> CallToolResult {
    let mut result = CallToolResult::success(
        outcome
            .content
            .unwrap_or_default()
            .into_iter()
            .map(content)
            .collect(),
    );
    result.structured_content = outcome.result;
    result.is_error = Some(outcome.is_error);
    result
}

fn content(block: CoreContent) -> ContentBlock {
    match block {
        CoreContent::Text { text } => ContentBlock::text(text),
        CoreContent::Image { url, mime_type } => media(url, mime_type, true),
        CoreContent::Audio { url, mime_type } => media(url, mime_type, false),
        CoreContent::ResourceLink {
            uri,
            name,
            mime_type,
        } => {
            let mut resource = Resource::new(uri, name);
            resource.mime_type = mime_type;
            ContentBlock::resource_link(resource)
        }
        CoreContent::Resource {
            uri,
            mime_type,
            contents,
        } => ContentBlock::resource(match contents {
            CoreResource::Text(text) => ResourceContents::TextResourceContents {
                uri,
                mime_type,
                text,
                meta: None,
            },
            CoreResource::Base64(blob) => ResourceContents::BlobResourceContents {
                uri,
                mime_type,
                blob,
                meta: None,
            },
        }),
    }
}

fn media(url: String, mime_type: String, image: bool) -> ContentBlock {
    if let Some(data) = url.strip_prefix(&format!("data:{mime_type};base64,")) {
        return if image {
            ContentBlock::image(data.to_owned(), mime_type)
        } else {
            ContentBlock::audio(data.to_owned(), mime_type)
        };
    }
    // MCP image/audio blocks require inline data. Keep references as references;
    // never fetch them, invent bytes, or label a URL as base64.
    ContentBlock::resource_link(Resource::new(url.clone(), url).with_mime_type(mime_type))
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

//! Copyright (c) Scott A Dixon
//! Bounded wire admission shared by both transports.
use crate::McpError;
#[cfg(any(feature = "stdio", feature = "http"))]
use rmcp::model::{CustomResult, ServerJsonRpcMessage, ServerResult};
use serde::Serialize;
use std::io::Write;

pub(crate) fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, McpError> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("MCP message limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut buffer, value).map_err(|_| McpError::Limit)?;
    Ok(buffer.bytes)
}

#[cfg(any(feature = "stdio", feature = "http"))]
pub(crate) fn decode(bytes: &[u8]) -> Result<ServerJsonRpcMessage, McpError> {
    let raw: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| McpError::Protocol)?;
    let mut message: ServerJsonRpcMessage =
        serde_json::from_value(raw.clone()).map_err(|_| McpError::Protocol)?;
    // Retain every catalogue record, including records that fail typed SDK deserialisation.
    // Admission belongs to the registry, and provenance must retain the server's fields.
    if let ServerJsonRpcMessage::Response(response) = &mut message
        && raw["result"].get("tools").is_some()
    {
        response.result = ServerResult::CustomResult(CustomResult(raw["result"].clone()));
    }
    Ok(message)
}

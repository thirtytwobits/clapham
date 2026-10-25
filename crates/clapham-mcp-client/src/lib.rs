//! Copyright (c) Scott A Dixon
#![doc = include_str!("../README.md")]
mod binding;
mod call;
mod connection;
#[cfg(feature = "http")]
mod http;
mod records;
#[cfg(feature = "stdio")]
mod stdio;
mod wire;

pub use binding::{McpCatalogue, McpDiagnostic, McpSelection, McpToolBinding};
pub use connection::{McpConfig, McpConnection, McpError};
#[cfg(feature = "http")]
pub use http::McpHttpOptions;

//! Copyright (c) Scott A Dixon
#![doc = include_str!("../README.md")]
// The shared test clock names this crate as its integration tests do.
#[cfg(test)]
extern crate self as clapham_mcp_client;
mod binding;
mod call;
mod clock;
mod connection;
#[cfg(feature = "http")]
mod http;
mod operations;
mod records;
#[cfg(feature = "stdio")]
mod stdio;
#[cfg(test)]
#[path = "../tests/support/clock.rs"]
mod test_clock;
mod wire;

pub use binding::{McpCatalogue, McpDiagnostic, McpSelection, McpToolBinding};
pub use clock::{Clock, Sleep, TokioClock};
pub use connection::{McpConfig, McpConnection, McpError};
#[cfg(feature = "http")]
pub use http::McpHttpOptions;

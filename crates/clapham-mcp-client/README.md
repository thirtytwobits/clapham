# clapham-mcp-client

Host-owned, bounded MCP 2026-07-28 connections that bind remote tools as Clapham operations.
Requires Rust 1.88. The `stdio` and `http` features are enabled by default and can be selected
independently. No transport is included when default features are disabled.

Create an `McpConfig` with the connection's namespace and a non-secret credential-scope
revision. Set its shared `clapham::Bounds` and operation deadline as needed. The host approves
the endpoint and credentials or creates the process streams and handles process shutdown.

Discover, select and compose:

```no_run
# async fn bind(connection: clapham_mcp_client::McpConnection) -> Result<(), Box<dyn std::error::Error>> {
use clapham::{Bounds, ComposeOptions, ExecutionMode, compose_with_options};
use clapham_mcp_client::McpSelection;

let catalogue = connection.discover().await?;
let binding = catalogue.bind(vec![McpSelection::new("lists.get")])?;
let set = compose_with_options(
    [binding.contribution()],
    Bounds::default(),
    ComposeOptions { execution: ExecutionMode::Daemon, ..Default::default() },
)?;
// With connection namespace "remote", callers see "remote.lists.get".
# let _ = set;
# Ok(())
# }
```

Each selection can assign a local alias and explicit principal kinds. Remote hints determine
query, destructive or mutation effects; remote metadata cannot grant administrative authority
or principal access. The host gate still controls admission. The contribution retains its
connection and binding revision. Fresh discovery and tool-list notifications fence stale calls.

Results use `Outcome`, ordered content and `Evidence`; adapter errors map to bounded declared
`Refusal` codes. A tool-reported `isError` remains an outcome. Dropping an invocation requests
cancellation, and uncertain effects are never replayed automatically.

See `docs/mcp.md` in the repository for the complete client contract, refusal mapping and
transport limits. The crate's `NOTICE` and `LICENSE` retain copyright and licence terms;
repository provenance records the extraction source and paths.

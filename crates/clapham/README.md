# clapham

The contract for operations, descriptors, refusals, principals, composition, selection,
gates and calls.

`Operation` is the serialisable descriptor. Its name uses 1 to 128 ASCII letters,
digits, `_`, `-` and `.`, and splits at the first dot. `description` is required;
`input` declares an object root. `output` is an optional JSON Schema. Omitted
`principals` defaults from the effect; an explicit empty list admits nobody.
`idempotent` and `open_world` are explicit, required hints.

`Effect::annotations` maps the class and those hints to MCP. Queries are read-only
and idempotent; mutations and administrative operations are non-destructive;
destructive operations carry the destructive hint. An administrative class is
recorded under `io.github.thirtytwobits.clapham/effect` in the tool's `_meta`.
The returned `Annotations::meta` belongs on the tool itself, alongside its MCP
annotations. `Operation::annotations` supplies its declared hints to this mapping.

`Principal` carries `id` and `kind`. `Refusal` preserves the contributor's `code`,
with a fixed `Category`, message, optional JSON details and retryability flag.
`Category::exit_code` supplies the command-line exit code.

The published schemas are [operation.schema.json](schemas/operation.schema.json)
and [refusal.schema.json](schemas/refusal.schema.json). Generate them with
`cargo xtask schemas`; `cargo xtask schemas --check` refuses stale output and runs
inside `cargo xtask check`. Composition and execution follow in later issues.

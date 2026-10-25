# Composition reference

`Configuration` is the shared JSON/Rust model. Its OAuth issuer/audience, sources and operation
exposures are serialisable; executable provider and policy registrations are supplied by the
host. `from_json` returns an editable configuration. Rust builders can add registrations,
sources, exposures, aliases, scope requirements, resource policies and CLI settings before
`compose` or `compose_with_options` validates the result atomically.

A source refers to a registered provider, or to an endpoint and host credential reference for
an established remote connection. Sources cannot claim the same registration twice. Each
provider owns its operation namespace. Aliases are validated complete names; handlers receive
the original provider name. Different sources cannot claim one exposed namespace.

Every `require.scopes` entry is mandatory. Configured scope/policy restrictions are added to
provider declarations; selection and aliasing cannot erase them. Policies are resolved by
name and checked against the operation's generated input schema. Provider/policy registrations,
source names and public names must be unique. Policy implementation code remains in Rust.

The common composer validates names, descriptions, schemas and budgets. Schema references
resolve offline within their document. Invalid bounds, excess schema/catalogue size and
operation count fail composition. Typed input conversion is checked before admission, and
successful output is validated after dispatch. Rust construction does not bypass these checks.

The composed set is immutable and cheaply clonable. Selections are bound to that set and its
clones. Canonical revisions include descriptor permission requirements and relevant remote
binding revisions. Revisions exclude handler implementations, caller identity and host policy.

See [the application guide](composing.md), [contract](contract.md) and
[configuration tests](../crates/clapham/tests/redesign.rs).

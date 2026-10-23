# Clapham

The operations contract every piece of the [London Project](https://github.com/thirtytwobits/london)
contributes to, and the CLI, MCP server and MCP client an application assembles from it.

An operation an operator runs from a shell, an agent calls over MCP and a flow calls as a tool is
one operation, described once. Each piece contributes its operations under its own namespace; the
application composes them into one set, decides through its gate which principal may run what,
and presents selections of the set through its front ends.

Clapham builds on no other piece of the London Project. Its design is
[thirtytwobits/london#2](https://github.com/thirtytwobits/london/issues/2).

## Developing

Changes reach `main` through pull requests.

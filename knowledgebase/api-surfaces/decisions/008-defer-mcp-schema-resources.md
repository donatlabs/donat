---
type: decision
status: accepted
date: 2026-07-23
features:
  - "[[api-surfaces]]"
---

# Defer MCP schema resources until they are a complete protocol surface

## Context

The first explicit MCP publication metadata draft accepted
`resources.schema.enabled`, and the Petshop MCP example enabled it. The MCP
server, however, only implements tool discovery and invocation: it does not
advertise a resources capability or handle `resources/list` and
`resources/read`. Accepting the flag therefore creates an advertised-looking
configuration with no usable client surface.

## Decision

Reject `resources.schema.enabled: true` while loading `mcp.yaml`. Metadata
validation reports that MCP schema resources are not supported, so a server
cannot start with a misleading partial resource configuration.

The schema-resource shape remains reserved in the metadata types for a future
implementation. That implementation must add the MCP capabilities,
role-scoped discovery, read dispatch, exact resource URIs and conformance
fixtures in the same change before the loader accepts the flag.

## Alternatives

| Option | Why Not |
|---|---|
| Silently ignore the flag | An operator believes a resource is published when no client can discover or read it. |
| Advertise the capability without handlers | Clients discover a protocol feature that fails on use. |
| Implement a provisional schema resource now | It would require a new role-scoped MCP contract and conformance coverage beyond this publication-layer correction. |

## Consequences

Operators get a fail-fast metadata error instead of a broken MCP resource.
The Petshop MCP example publishes only the supported tool contract. Adding
schema resources later is an intentional protocol feature, not a metadata-only
toggle.

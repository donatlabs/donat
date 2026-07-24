---
type: decision
status: accepted
date: 2026-07-22
features:
  - "[[api-surfaces]]"
---

# Explicit MCP publication metadata and proxy-owned network admission

## Context

The original MCP surface derived six generic CRUD tools from every tracked
table. That is useful for a local exploratory client but makes the public
agent contract broad, poorly described, and incompatible with a GraphQL
allowlist. It also rejected non-loopback `Host` and `Origin` headers, which
made ordinary reverse-proxy deployments impossible even though GraphQL uses
the same server and authentication model.

## Decision

An optional top-level `mcp.yaml` is the MCP publication and presentation
layer. When present, it is an allowlist: only named saved-query tools and
explicit table CRUD operations are listed or callable. Each entry declares
its description and allowed roles. Execution still goes through GraphQL; MCP
metadata grants neither data permission nor a bypass. Saved queries therefore
remain compatible with the global GraphQL allowlist.

When `mcp.yaml` is absent, retain the existing generic tools for backwards
compatibility. This mode is intended for local exploration and is not the
recommended remote contract.

Donat no longer performs MCP-specific loopback `Host`/`Origin` admission.
TLS termination, permitted origins/hosts, forwarding headers and rate limits
are deployment-edge concerns (normally nginx), matching `/v1/graphql`.
Authentication continues to use the existing trusted headers, JWT, or auth
hook; this decision does not add OAuth or an admin role.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep generated CRUD for all remote MCP clients | Publishes a wide, undocumented agent surface and cannot coexist usefully with the allowlist. |
| Put MCP fields beside every GraphQL metadata object | Couples an agent presentation layer to GraphQL configuration and makes curated cross-domain tools awkward. |
| Add a Donat Origin/Host allowlist | Duplicates and conflicts with reverse-proxy policy; GraphQL has no equivalent transport-only restriction. |
| Add OAuth resource-server support now | Existing header/JWT/auth-hook integrations already authenticate callers; OAuth is a separate product decision. |

## Consequences

Remote MCP operators must configure their proxy correctly. A deployment gains
a small, role-scoped, human-described tool contract while retaining the
single GraphQL permission and execution pipeline. The older generic mode
remains available only when no `mcp.yaml` is configured.

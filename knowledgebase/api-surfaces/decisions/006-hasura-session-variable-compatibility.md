---
type: decision
status: accepted
date: 2026-06-30
features:
  - "[[api-surfaces]]"
---

# Hasura session-variable compatibility without admin bypass

## Context

Operators migrating from Hasura often already have JWTs and permission
metadata that use `x-hasura-*` session variables, especially
`x-hasura-allowed-roles`, `x-hasura-default-role`, and row-filter variables
such as `X-Hasura-User-Id`. Before this change, donat only recognized the
Donat namespace (`x-donat-*`) on JWT claims, trusted request headers, and
metadata session-variable references.

The hard constraint is the no-admin-role invariant: compatibility must not
reintroduce Hasura's admin-role bypass or any alternate admin secret.

## Decision

Accept `x-hasura-*` as a compatibility alias for session-variable names while
keeping the authorization model unchanged. JWT role selection reads Donat
claims first and falls back to Hasura claims. Trusted request headers accept
`X-Hasura-Role` only as an explicit role header, with `X-Donat-Role` taking
precedence if both are present. Permission filters and presets treat
`X-Hasura-*` as session-variable references exactly like `X-Donat-*`.

Do not add `X-Hasura-Admin-Secret` support. It is neither a trusted-request
secret nor a permission bypass. Where session headers are validated or
forwarded, `x-hasura-*` is handled with the same safety rules as `x-donat-*`.

## Alternatives

| Option | Why Not |
|--------|---------|
| Require migrations to rewrite all Hasura metadata to `X-Donat-*` | Makes existing Hasura projects harder to run and defeats the point of compatibility. |
| Add Hasura admin-secret semantics too | Violates the no-admin-role/no-bypass invariant. |
| Store only Donat-normalized variables internally | Breaks metadata that explicitly references `X-Hasura-*` row-filter variables. |

## Consequences

Existing Hasura-style JWTs and permission filters can run without metadata
rewrites. The engine carries both namespaces where role variables are
normalized, which is a small amount of duplication but keeps downstream
actions, remote schema presets, REST, and MCP behavior consistent. The
security posture stays fail-closed: every data access still requires an
explicit role and a matching permission.

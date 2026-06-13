---
type: decision
status: accepted
date: 2026-06-13
features:
  - "[[api-surfaces]]"
---

# Each API surface (GraphQL / REST / MCP) is gated by an enabled-apis flag

## Context

The engine now serves three transports over the same data plane: GraphQL
(`/v1/graphql` + relay + ws), REST (`/api/rest`), and MCP (`/mcp`). Operators
need to expose only the surfaces they want — e.g. a deployment that only wants
GraphQL, or one that wants GraphQL + REST but not the MCP CRUD surface over
HTTP. This must be deploy-time configuration (consistent with the engine's
no-runtime-admin posture): nothing toggles a surface at runtime.

## Decision

A single list flag selects which surfaces are mounted, mirroring Donat/Hasura
v2's `HASURA_GRAPHQL_ENABLED_APIS`:

- Env: `DONAT_GRAPHQL_ENABLED_APIS` — a comma-separated list.
- CLI: `--enabled-apis` (same value; CLI wins over env).
- Tokens (case-insensitive, trimmed): `graphql`, `rest`, `mcp`.
  - `graphql` gates `/v1/graphql`, `/v1alpha1/graphql`, `/v1/relay`,
    `/v1beta1/relay`, and their websocket upgrades (subscriptions/relay-ws).
  - `rest` gates `/api/rest/<*path>`.
  - `mcp` gates `/mcp` (POST + the GET-405 stub).
- `/healthz` and `/v1/version` are always mounted (liveness/version are not
  data APIs).

A surface not in the set is **not registered** in the axum router, so requests
to it get a plain 404 — there is no half-on state and no per-request gate to
bypass.

**Default = `graphql,rest,mcp` (all on).** This keeps existing GraphQL
deployments working unchanged and makes the new surfaces available out of the
box; an operator narrows the set to restrict exposure (e.g.
`DONAT_GRAPHQL_ENABLED_APIS=graphql` turns REST and MCP off).

Unknown tokens (e.g. Hasura's `metadata`/`config`/`pgdump`, which this engine
does not implement) are **warned about and ignored** rather than fatal, so a
metadata/config file carried over from Hasura still boots with its recognized
surfaces.

## Alternatives

| Option | Why Not |
|--------|---------|
| Three separate boolean flags (`--enable-rest`, …) | More surface area and the "what's the default of each" question three times; the list is the v2-idiomatic single knob and reads as data ("these APIs are on"). |
| Default to `graphql` only (REST/MCP opt-in) | Safer-by-default, but surprising: the surfaces exist and most users adding them want them on. Kept as an easy one-line change if a stricter default is preferred; the flag makes either posture trivial. |
| Error on unknown tokens | Breaks configs copied from Hasura that list `metadata`/`config`. Warn-and-ignore is more compatible and still visible in logs. |
| Runtime toggle (admin API) | Violates the no-runtime-admin-API rule. Configuration is deploy-time only. |

## Consequences

- **Gain:** operators expose exactly the surfaces they want with one v2-style
  knob; disabling a surface removes its routes entirely (no attack surface, no
  per-request cost). GraphQL-only, GraphQL+REST, and all-on are all one env var
  apart.
- **Pay:** the default is permissive (all on); a security-conscious operator
  must set the var to restrict. Documented prominently in the READMEs.
- **Boundary:** the flag only mounts/omits routes; per-surface auth and
  permissions are unchanged (every mounted surface still requires a role — no
  admin bypass anywhere).

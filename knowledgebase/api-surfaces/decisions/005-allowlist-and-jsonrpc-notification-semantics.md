---
type: decision
status: accepted
date: 2026-06-14
features:
  - "[[api-surfaces]]"
---

# Allowlist interaction & JSON-RPC notification semantics for REST/MCP

## Context

A code review of the REST + MCP surfaces raised three items that look like
bugs but are, on inspection, intended behaviour. This ADR records the
reasoning so they are not "fixed" into regressions later. The relevant
invariant is the project's fail-closed, no-bypass stance (see the BLOCKING
RULE in `CLAUDE.md`): surfaces reuse `gql::execute_full` and never weaken a
security gate to make a surface more convenient.

1. **Query allowlist vs REST/MCP.** When `DONAT_GRAPHQL_ENABLE_ALLOWLIST` is
   set, `execute_full` rejects any operation whose normalized form is not in
   an allowlisted query collection. Both REST and MCP route through
   `execute_full`, so the gate applies to them too. The review observed that
   (a) MCP CRUD tools build *ad-hoc* operations that are never in any
   collection, so every MCP data call returns "query is not allowed" when the
   allowlist is on; and (b) a REST endpoint whose backing collection is not
   itself listed in `metadata.allowlist` is likewise rejected.
2. **JSON-RPC requests without `id`.** `/mcp` treats a request lacking `id` as
   a notification and answers with an empty `200`, before role resolution — so
   a `tools/call` sent without an `id` is a silent no-op rather than executed
   or denied.
3. **REST 405 vs 404.** A path that structurally matches a `:param` template
   but has no endpoint for the method returns `405`. The review flagged this
   as leaking endpoint shape for unrelated paths.

## Decision

**(1) The allowlist gate applies uniformly; it is not bypassed for REST or
MCP.** This is fail-closed and correct:

- For **REST**, the allowlist is a deliberate operator restriction to a vetted
  set of saved queries. REST endpoints are saved queries, but the allowlist is
  keyed on collections explicitly listed in `metadata.allowlist`, not on every
  collection that happens to exist. An endpoint whose collection is not
  allowlisted is rejected — the operator who turned on the allowlist opted into
  exactly that. Bypassing it for REST would let any `rest_endpoint` evade the
  allowlist, defeating its purpose.
- For **MCP**, ad-hoc CRUD operations cannot be pre-registered, so enabling the
  allowlist effectively disables the MCP data tools. That is the *safe*
  outcome: the allowlist exists to restrict execution to vetted operations, and
  letting MCP run arbitrary generated CRUD would defeat it. The two features
  are mutually exclusive by design; the allowlist wins.

We document this incompatibility rather than add a bypass.

**(2) `id`-less requests are notifications per JSON-RPC 2.0** ("the Server MUST
NOT reply to a Notification"). A spec-conformant MCP client always includes an
`id` on `initialize`/`tools/list`/`tools/call`; only true notifications
(e.g. `notifications/initialized`) omit it. Acknowledging with an empty `200`
is correct, and the empty-body short-circuit before auth is harmless because no
work is performed. We keep it.

**(3) `405` for a `:param`-matching path is correct REST behaviour.**
`match_template` requires *literal* segments to be equal, so `pet/:id` only
matches paths under the literal `pet/` prefix — not arbitrary two-segment
paths. A request to such a path that exists for a different method is a genuine
method mismatch (`405`); a path matching no template is `404`. This is locked
in by the `select_endpoint_405_*` / `select_endpoint_404_*` unit tests added
with the routing-precedence fix.

## Alternatives

| Option | Why Not |
|--------|---------|
| Make REST/MCP bypass the allowlist | Defeats the allowlist's purpose (fail-open); violates the no-bypass stance. |
| Auto-allowlist every collection referenced by a `rest_endpoint` | Surprising and still operator-surprising; the operator chose the allowlist set deliberately. Fail-closed is safer. |
| Execute `id`-less `tools/call` anyway | Violates JSON-RPC 2.0 notification semantics; would surface responses clients are told not to expect. |
| Return `404` instead of `405` for `:param`-matching method mismatches | Loses the legitimate 405 signal for real method mismatches; the path *does* match a defined route shape. |

## Consequences

- Operators enabling the query allowlist must know it turns off MCP data tools
  and that each REST endpoint's collection must be allowlisted; this is now
  documented here and should surface in operator docs.
- No code change: the reviewed behaviours are intentional. The routing fix
  (ADR-adjacent, commit on `fix/code-review-findings`) added unit tests that
  pin the 404/405 distinction so a future refactor cannot silently regress it.

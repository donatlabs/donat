# donat Knowledge Base

> Living documentation for design explorations and decisions that are not yet
> (or not only) code. Engine internals and conformance status live in PLAN.md
> and tests/donat/COVERAGE.md; this base holds ideas, research, and ADRs.

## Domains

### [[embedded-sdk/_index|Embedded SDK & Native Hooks]]
Embedding the engine into host-language applications (Go, Node.js) with
native-function hooks (`pre_insert` / `post_insert` / post-commit) instead of
Donat-style webhooks. 6 design notes, 1 research report, 5 decisions.
**Status: idea, deferred until core conformance is done.**

### [[api-surfaces/_index|API Surfaces — REST & MCP]]
Serving the per-role data plane over Donat v2 RESTified endpoints
(`rest_endpoints` → saved GraphQL queries) and an MCP server (streamable HTTP,
generic CRUD tools). Both translate to GraphQL and reuse the execution
pipeline. 1 design note, 2 decisions. **Status: in progress (June 2026).**

## Cross-cutting

- [[security-audit|Security & dependency audit]] — SQL-gen injection review, ranked findings (internal-microservice threat model), library assessment (2026-06-13)
- [research-metadata-architecture.json](research-metadata-architecture.json) — deep-research: declarative/GitOps metadata loading vs runtime admin API; recommends completing filesystem-boot + production-disabling the admin/run_sql surface (2026-06-13)

## Templates

- [[_templates/feature-dossier|Feature Dossier Template]]
- [[_templates/decision|Decision (ADR) Template]]

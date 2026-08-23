# donat Knowledge Base

> Living documentation for design explorations and decisions that are not yet
> (or not only) code. Engine internals and milestones live in PLAN.md, and the
> conformance contract is the `crates/conformance` suite itself; this base
> holds ideas, research, and ADRs.

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

### [[operations/_index|Operations — deploying and running the engine]]
The engine as a process: what bounds a request, how it reaches its database,
how it drains on `SIGTERM`, and what an operator may inspect without a
permission bypass. 5 decisions. **Status: in progress (August 2026).**

### [[platform/research-what-a-platform-needs|Platform direction]]
What separates an engine you deploy from a platform a business is built on,
written after a week of the engine being used to build one. 1 research note,
1 decision — the admin panel, which is an ordinary role rendered outside the
engine rather than a surface the engine grows (`apps/ui`).
**Status: draft, August 2026.**

### [[platform/research-multitenancy-elsewhere|Multitenancy elsewhere]]
What Hasura, Supabase/PostgREST and Nile do about tenants, written to check the
premise of [[declarative-saas/decisions/097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]]
rather than assert it. Two things worth taking: per-tenant resource limits,
which nothing here bounds, and RLS as a second fence for the tenant alone.
1 research note. **Status: draft, August 2026.**

## Cross-cutting

- [[security-audit|Security & dependency audit]] — SQL-gen injection review, ranked findings (internal-microservice threat model), library assessment (2026-06-13)
- [research-metadata-architecture.json](research-metadata-architecture.json) — deep-research: declarative/GitOps metadata loading vs runtime admin API; recommends completing filesystem-boot + production-disabling the admin/run_sql surface (2026-06-13)

## Templates

- [[_templates/feature-dossier|Feature Dossier Template]]
- [[_templates/decision|Decision (ADR) Template]]

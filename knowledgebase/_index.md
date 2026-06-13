# dist-api Knowledge Base

> Living documentation for design explorations and decisions that are not yet
> (or not only) code. Engine internals and conformance status live in PLAN.md
> and tests/hasura/COVERAGE.md; this base holds ideas, research, and ADRs.

## Domains

### [[embedded-sdk/_index|Embedded SDK & Native Hooks]]
Embedding the engine into host-language applications (Go, Node.js) with
native-function hooks (`pre_insert` / `post_insert` / post-commit) instead of
Hasura-style webhooks. 6 design notes, 1 research report, 5 decisions.
**Status: idea, deferred until core conformance is done.**

## Cross-cutting

- [[security-audit|Security & dependency audit]] — SQL-gen injection review, ranked findings (internal-microservice threat model), library assessment (2026-06-13)
- [research-metadata-architecture.json](research-metadata-architecture.json) — deep-research: declarative/GitOps metadata loading vs runtime admin API; recommends completing filesystem-boot + production-disabling the admin/run_sql surface (2026-06-13)

## Templates

- [[_templates/feature-dossier|Feature Dossier Template]]
- [[_templates/decision|Decision (ADR) Template]]

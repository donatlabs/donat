# Declarative SaaS runtime

Design notes and decisions for the layer above Donat's Hasura-compatible data
plane: business commands, rules, durable processes, and compiled connectors.

## Decisions

- [[decisions/001-declarative-saas-runtime-and-porting-policy]]
- [[decisions/002-durable-process-operational-contracts]]
- [[decisions/004-command-literal-db-scalar-validation]]
- [[decisions/005-executable-command-resolved-ir]]
- [[decisions/006-command-claim-election-separate-from-canonical-result-journal]]
- [[decisions/007-command-cte-gates-and-batch-rule-items]]
- [[decisions/008-source-and-role-qualified-command-identity]]
- [[decisions/009-durable-process-source-local-compilation-and-journal-contracts]]
- [[decisions/010-static-community-connector-factory-and-runtime-boundaries]]
- [[decisions/011-version-independent-rust-boundary-lexer]]
- [[decisions/012-canonical-catalog-projections-and-persisted-header-capabilities]]
- [[decisions/013-petshop-first-executable-requirements]]
- [[decisions/014-command-relational-batches]]
- [[decisions/015-petshop-modular-pressure-suite]]
- [[decisions/016-bounded-command-aggregate-types]]
- [[decisions/017-bounded-command-current-row-namespace]]
- [[decisions/018-bounded-command-argument-rows]]
- [[decisions/019-command-only-table-permissions]]
- [[decisions/020-command-unconditional-unique-identity]]
- [[decisions/021-pinned-source-local-process-start-consumption]]
- [[decisions/022-closed-deterministic-process-transitions]]
- [[decisions/023-durable-wait-linearization]]
- [[decisions/024-bounded-fanout-item-journal]]
- [[decisions/025-verified-inbound-delivery-and-wait-correlation]]
- [[decisions/026-connector-egress-is-a-network-concern]]
- [[decisions/027-process-entry-point-commands]]
- [[decisions/028-effect-contracts-compare-resolved-fields]]
- [[decisions/029-connector-response-contract-is-the-activity-output-schema]]
- [[decisions/030-command-arguments-of-named-types-compile-to-their-declared-representation]]
- [[decisions/031-a-durable-retry-must-name-its-cause]]
- [[decisions/032-permission-validators-declare-presence]]
- [[decisions/033-files-are-a-column-and-their-urls-are-signed-in-sql]]
- [[decisions/034-a-declaration-the-runtime-ignores-is-a-defect]]
- [[decisions/035-an-idempotency-scope-may-read-a-lookup-never-a-write]]
- [[decisions/036-the-transition-queue-is-a-work-queue-not-a-line]]

## Reference governance

- [[reference-porting-register]]

-- Durable first-executor election for declarative-command idempotency.
--
-- V3 is the canonical completed-invocation journal: it retains the input
-- fingerprint and complete result used for replay. PostgreSQL does not make a
-- row inserted by one data-modifying CTE visible to a later CTE update in the
-- same statement, so V4 keeps the short-lived election state separately.
-- SQLgen atomically inserts or updates this row and only the elected claim may
-- execute domain writes. No request input, raw scope, role, result, or SQL is
-- stored here.

create table if not exists donat.command_invocation_claims (
    command_name text        not null,
    scope_hash   bytea       not null,
    key          text        not null,
    -- SQLgen writes only `first` for a new/reclaimed key or `replay` for an
    -- active existing key. This is an internal election marker, not a public
    -- invocation lifecycle.
    claim_state  text        not null check (claim_state in ('first', 'replay')),
    expires_at   timestamptz not null,
    created_at   timestamptz not null default now(),
    primary key (command_name, scope_hash, key)
);

-- Retention cleanup treats the claim and V3 journal as one identity. An
-- expired claim is also safely reclaimable by the command statement before a
-- cleanup job runs, so this index is a cleanup aid rather than a correctness
-- dependency.
create index if not exists command_invocation_claims_expires_at_idx
    on donat.command_invocation_claims (expires_at);

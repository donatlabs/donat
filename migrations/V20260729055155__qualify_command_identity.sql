-- Qualify declarative-command idempotency by source and explicit role.
--
-- V3/V4 stored only command_name, which is source-local and therefore cannot
-- safely identify a completed invocation when two source-local commands share
-- a name or when their permissions are disjoint by role. Existing rows do not
-- contain enough information to infer either owner. This deploy-time migration
-- preserves them in an explicit legacy namespace; the command statement fails
-- closed if a new qualified execution encounters a matching legacy key.

alter table donat.command_invocations
    add column command_identity text;

update donat.command_invocations
set command_identity =
    'legacy-unqualified:' || encode(convert_to(command_name, 'UTF8'), 'hex');

alter table donat.command_invocations
    alter column command_identity set not null,
    drop constraint command_invocations_pkey,
    add constraint command_invocations_pkey
        primary key (command_identity, scope_hash, key),
    add constraint command_invocations_identity_nonempty
        check (command_identity <> '');

alter table donat.command_invocation_claims
    add column command_identity text;

update donat.command_invocation_claims
set command_identity =
    'legacy-unqualified:' || encode(convert_to(command_name, 'UTF8'), 'hex');

alter table donat.command_invocation_claims
    alter column command_identity set not null,
    drop constraint command_invocation_claims_pkey,
    add constraint command_invocation_claims_pkey
        primary key (command_identity, scope_hash, key),
    add constraint command_invocation_claims_identity_nonempty
        check (command_identity <> '');

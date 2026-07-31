-- Source-local durable Process definitions, journals, activity leases, and
-- provider ingress. This migration is deploy-time-only; serving validates
-- these objects read-only and never creates or alters them.

create or replace function donat.check_violation(msg text)
returns json
language plpgsql
as $$
begin
    raise exception using message = msg, errcode = '23514';
end;
$$;

-- A command execution generation is distinct from its reusable idempotency
-- key. Exact replay retains this UUID; an expired-key execution replaces it.
alter table donat.command_invocations
    add column invocation_id uuid;

update donat.command_invocations
set invocation_id = gen_random_uuid();

alter table donat.command_invocations
    alter column invocation_id set not null,
    add constraint command_invocations_invocation_id_key
        unique (invocation_id);

create table donat.process_definition_versions (
    source_name text not null,
    process_name text not null,
    revision text not null,
    canonical_definition jsonb not null
        check (pg_column_size(canonical_definition) <= 262144),
    dependency_descriptors jsonb not null
        check (pg_column_size(dependency_descriptors) <= 262144),
    runtime_abi integer not null check (runtime_abi > 0),
    status text not null check (status in ('active', 'retired')),
    deployed_at timestamptz not null default now(),
    retired_at timestamptz,
    primary key (source_name, process_name, revision)
);

create unique index process_definition_versions_active_key
    on donat.process_definition_versions (source_name, process_name)
    where status = 'active';

create index process_definition_versions_status_idx
    on donat.process_definition_versions (source_name, status, process_name);

create table donat.process_start_requests (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    process_name text not null,
    revision text not null,
    input_json jsonb not null
        check (pg_column_size(input_json) <= 262144),
    command_invocation_id uuid not null,
    effect_position integer not null check (effect_position >= 0),
    idempotency_key text not null,
    status text not null
        check (status in ('pending', 'consumed', 'duplicate', 'failed')),
    instance_id uuid,
    created_at timestamptz not null default now(),
    consumed_at timestamptz,
    primary key (source_name, id),
    unique (source_name, command_invocation_id, effect_position),
    foreign key (source_name, process_name, revision)
        references donat.process_definition_versions
            (source_name, process_name, revision)
);

create index process_start_requests_due_idx
    on donat.process_start_requests (source_name, status, created_at, id)
    where status = 'pending';

create table donat.process_instances (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    process_name text not null,
    revision text not null,
    source_request_id uuid not null,
    start_idempotency_key text not null,
    status text not null
        check (status in ('running', 'terminal', 'failed', 'cancelled')),
    current_state text not null,
    input_json jsonb not null
        check (pg_column_size(input_json) <= 262144),
    state_json jsonb not null
        check (pg_column_size(state_json) <= 262144),
    version bigint not null default 0 check (version >= 0),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (source_name, id),
    unique (source_name, process_name, start_idempotency_key),
    unique (source_name, source_request_id),
    foreign key (source_name, source_request_id)
        references donat.process_start_requests (source_name, id),
    foreign key (source_name, process_name, revision)
        references donat.process_definition_versions
            (source_name, process_name, revision)
);

create index process_instances_correlation_idx
    on donat.process_instances
        (source_name, process_name, current_state, status, id);

create table donat.process_events (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    instance_id uuid not null,
    process_name text not null,
    revision text not null,
    kind text not null check (kind in (
        'start',
        'signal',
        'timer',
        'activity_succeeded',
        'activity_failed',
        'retry_exhausted',
        'command_rejected',
        'cancellation'
    )),
    payload_json jsonb not null
        check (pg_column_size(payload_json) <= 262144),
    idempotency_key text,
    available_at timestamptz not null default now(),
    status text not null check (status in ('pending', 'consumed', 'failed')),
    attempts integer not null default 0 check (attempts >= 0),
    created_at timestamptz not null default now(),
    consumed_at timestamptz,
    primary key (source_name, id),
    foreign key (source_name, instance_id)
        references donat.process_instances (source_name, id),
    foreign key (source_name, process_name, revision)
        references donat.process_definition_versions
            (source_name, process_name, revision)
);

create unique index process_events_idempotency_key
    on donat.process_events
        (source_name, process_name, revision, kind, idempotency_key)
    where idempotency_key is not null;

create index process_events_due_idx
    on donat.process_events (source_name, status, available_at, id)
    where status = 'pending';

create table donat.process_signal_requests (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    process_name text not null,
    process_revision text not null,
    signal_name text not null,
    correlation_json jsonb not null
        check (pg_column_size(correlation_json) <= 262144),
    payload_json jsonb not null
        check (pg_column_size(payload_json) <= 262144),
    command_invocation_id uuid not null,
    effect_position integer not null check (effect_position >= 0),
    idempotency_key text not null,
    status text not null check (status in (
        'pending',
        'consumed',
        'duplicate',
        'unmatched',
        'ambiguous',
        'guard_false',
        'unexpected_state',
        'failed'
    )),
    created_at timestamptz not null default now(),
    consumed_at timestamptz,
    primary key (source_name, id),
    unique (source_name, command_invocation_id, effect_position),
    foreign key (source_name, process_name, process_revision)
        references donat.process_definition_versions
            (source_name, process_name, revision)
);

create index process_signal_requests_due_idx
    on donat.process_signal_requests (source_name, status, created_at, id)
    where status = 'pending';

create table donat.process_activity_jobs (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    instance_id uuid not null,
    enqueued_from_event_id uuid not null,
    state_name text not null,
    logical_activity_id text not null,
    connector_instance text not null,
    operation text not null,
    serialization_key_hash bytea,
    input_json jsonb not null
        check (pg_column_size(input_json) <= 262144),
    result_json jsonb
        check (pg_column_size(result_json) <= 262144),
    request_fingerprint text not null,
    status text not null check (status in (
        'scheduled', 'running', 'succeeded', 'failed', 'cancelled'
    )),
    attempts integer not null default 0 check (attempts >= 0),
    lease_generation bigint not null default 0 check (lease_generation >= 0),
    available_at timestamptz not null default now(),
    schedule_to_start_deadline timestamptz not null,
    start_to_close_deadline timestamptz,
    lease_token uuid,
    lease_expires_at timestamptz,
    last_error_json jsonb
        check (pg_column_size(last_error_json) <= 262144),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (source_name, id),
    unique (source_name, instance_id, logical_activity_id),
    foreign key (source_name, instance_id)
        references donat.process_instances (source_name, id),
    foreign key (source_name, enqueued_from_event_id)
        references donat.process_events (source_name, id)
);

create index process_activity_jobs_due_idx
    on donat.process_activity_jobs (source_name, status, available_at, id)
    where status = 'scheduled';

create index process_activity_jobs_lease_idx
    on donat.process_activity_jobs
        (source_name, status, lease_expires_at, id)
    where status = 'running';

create table donat.process_activity_provider_steps (
    source_name text not null,
    activity_job_id uuid not null,
    logical_activity_id text not null,
    compiled_step_id text not null,
    idempotency_key text not null,
    first_provider_attempt_at timestamptz not null,
    maximum_send_deadline_at timestamptz not null,
    usable_window_expires_at timestamptz not null,
    created_at timestamptz not null default now(),
    primary key (source_name, logical_activity_id, compiled_step_id),
    unique (source_name, activity_job_id, compiled_step_id),
    foreign key (source_name, activity_job_id)
        references donat.process_activity_jobs (source_name, id)
);

create index process_activity_provider_steps_window_idx
    on donat.process_activity_provider_steps
        (source_name, usable_window_expires_at);

create table donat.process_transition_logs (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    instance_id uuid not null,
    event_id uuid,
    activity_job_id uuid,
    activity_attempt integer,
    activity_lease_generation bigint,
    from_state text,
    to_state text,
    outcome text not null,
    definition_revision text not null,
    command_result_json jsonb
        check (pg_column_size(command_result_json) <= 262144),
    before_state_hash bytea,
    after_state_hash bytea,
    redacted_context jsonb not null
        check (pg_column_size(redacted_context) <= 262144),
    created_at timestamptz not null default now(),
    primary key (source_name, id),
    foreign key (source_name, instance_id)
        references donat.process_instances (source_name, id),
    foreign key (source_name, event_id)
        references donat.process_events (source_name, id),
    foreign key (source_name, activity_job_id)
        references donat.process_activity_jobs (source_name, id)
);

create unique index process_transition_logs_event_key
    on donat.process_transition_logs (source_name, instance_id, event_id)
    where event_id is not null;

create unique index process_transition_logs_activity_outcome_key
    on donat.process_transition_logs (
        source_name,
        activity_job_id,
        activity_attempt,
        activity_lease_generation,
        outcome
    )
    where activity_job_id is not null
      and activity_attempt is not null
      and activity_lease_generation is not null;

create table donat.process_capacity_reservations (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    activity_job_id uuid not null,
    connector_instance text not null,
    operation text not null,
    serialization_key_hash bytea,
    lease_token uuid not null,
    reserved_at timestamptz not null,
    expires_at timestamptz not null,
    released_at timestamptz,
    primary key (source_name, id),
    unique (source_name, activity_job_id, lease_token),
    foreign key (source_name, activity_job_id)
        references donat.process_activity_jobs (source_name, id)
);

create index process_capacity_reservations_expiry_idx
    on donat.process_capacity_reservations
        (source_name, connector_instance, operation, expires_at);

create index process_capacity_reservations_serialization_idx
    on donat.process_capacity_reservations (
        source_name,
        connector_instance,
        operation,
        serialization_key_hash,
        expires_at
    );

create table donat.process_capacity_buckets (
    source_name text not null,
    connector_instance text not null,
    operation text not null,
    available_tokens numeric(38, 18) not null
        check (available_tokens >= 0),
    last_refill_at timestamptz not null,
    policy_fingerprint text not null,
    primary key (source_name, connector_instance, operation)
);

create table donat.process_inbound_deliveries (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    connector_instance text not null,
    provider_event_id text,
    payload_digest bytea not null,
    signature_status text not null check (signature_status in (
        'verified', 'missing', 'invalid', 'expired', 'malformed', 'unsupported'
    )),
    outcome text not null check (outcome in (
        'accepted',
        'duplicate',
        'unmatched',
        'ambiguous',
        'guard_false',
        'unexpected_state',
        'invalid_signature'
    )),
    instance_id uuid,
    process_event_id uuid,
    redacted_metadata jsonb not null
        check (pg_column_size(redacted_metadata) <= 262144),
    received_at timestamptz not null default now(),
    primary key (source_name, id),
    foreign key (source_name, instance_id)
        references donat.process_instances (source_name, id),
    foreign key (source_name, process_event_id)
        references donat.process_events (source_name, id),
    check (
        (
            outcome = 'accepted'
            and instance_id is not null
            and process_event_id is not null
        )
        or
        (
            outcome <> 'accepted'
            and instance_id is null
            and process_event_id is null
        )
    )
);

create index process_inbound_deliveries_instance_idx
    on donat.process_inbound_deliveries
        (source_name, instance_id, received_at)
    where instance_id is not null;

create index process_inbound_deliveries_received_idx
    on donat.process_inbound_deliveries
        (source_name, received_at, id);

create table donat.process_inbound_events (
    source_name text not null,
    id uuid not null default gen_random_uuid(),
    connector_instance text not null,
    provider_event_id text not null,
    first_delivery_id uuid not null,
    payload_digest bytea not null,
    verified_at timestamptz not null,
    primary key (source_name, id),
    unique (source_name, connector_instance, provider_event_id),
    foreign key (source_name, first_delivery_id)
        references donat.process_inbound_deliveries (source_name, id)
        deferrable initially deferred
);

-- Durable, source-local expansion for bounded Process for_each states.
-- The item journal is finite by compiled metadata, retains original order,
-- and links request items to the existing fenced activity worker.

alter table donat.process_events
    drop constraint process_events_kind_check,
    add constraint process_events_kind_check check (kind in (
        'start',
        'continue',
        'signal',
        'timer',
        'fanout_item',
        'activity_succeeded',
        'activity_failed',
        'retry_exhausted',
        'command_rejected',
        'cancellation'
    ));

create table donat.process_fanout_items (
    source_name text not null,
    instance_id uuid not null,
    state_name text not null,
    entry_event_id uuid not null,
    ordinal integer not null check (ordinal >= 0 and ordinal < 256),
    item_key text not null,
    item_key_identity text not null,
    item_json jsonb not null
        check (
            jsonb_typeof(item_json) = 'object'
            and pg_column_size(item_json) <= 262144
        ),
    status text not null check (status in (
        'pending', 'scheduled', 'succeeded', 'failed'
    )),
    activity_job_id uuid,
    result_json jsonb
        check (
            result_json is null
            or (
                jsonb_typeof(result_json) = 'object'
                and pg_column_size(result_json) <= 262144
            )
        ),
    failure_json jsonb
        check (
            failure_json is null
            or (
                jsonb_typeof(failure_json) = 'object'
                and pg_column_size(failure_json) <= 262144
            )
        ),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (source_name, instance_id, state_name, ordinal),
    unique (source_name, instance_id, state_name, item_key_identity),
    unique (source_name, activity_job_id),
    foreign key (source_name, instance_id)
        references donat.process_instances (source_name, id),
    foreign key (source_name, entry_event_id)
        references donat.process_events (source_name, id),
    foreign key (source_name, activity_job_id)
        references donat.process_activity_jobs (source_name, id),
    check (
        (status in ('pending', 'scheduled')
            and result_json is null
            and failure_json is null)
        or
        (status = 'succeeded'
            and result_json is not null
            and failure_json is null)
        or
        (status = 'failed'
            and result_json is null
            and failure_json is not null)
    )
);

create index process_fanout_items_progress_idx
    on donat.process_fanout_items (
        source_name,
        instance_id,
        state_name,
        status,
        ordinal
    );

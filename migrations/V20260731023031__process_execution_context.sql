-- Closed caller context and explicit internal continuation events for the
-- Process state executor. Existing pre-V7 rows remain readable; a caller
-- command state fails closed if its historical start has no caller context.

alter table donat.process_start_requests
    add column caller_role text,
    add column caller_session_json jsonb
        check (
            caller_session_json is null
            or (
                jsonb_typeof(caller_session_json) = 'object'
                and pg_column_size(caller_session_json) <= 262144
            )
        ),
    add constraint process_start_requests_caller_context_check
        check (
            (caller_role is null and caller_session_json is null)
            or (
                caller_role is not null
                and caller_role <> ''
                and caller_role <> 'admin'
                and caller_session_json is not null
            )
        );

alter table donat.process_instances
    add column caller_role text,
    add column caller_session_json jsonb
        check (
            caller_session_json is null
            or (
                jsonb_typeof(caller_session_json) = 'object'
                and pg_column_size(caller_session_json) <= 262144
            )
        ),
    add column terminal_output_json jsonb
        check (
            terminal_output_json is null
            or pg_column_size(terminal_output_json) <= 262144
        ),
    add column failure_json jsonb
        check (
            failure_json is null
            or (
                jsonb_typeof(failure_json) = 'object'
                and pg_column_size(failure_json) <= 262144
            )
        ),
    add constraint process_instances_caller_context_check
        check (
            (caller_role is null and caller_session_json is null)
            or (
                caller_role is not null
                and caller_role <> ''
                and caller_role <> 'admin'
                and caller_session_json is not null
            )
        );

alter table donat.process_events
    drop constraint process_events_kind_check,
    add constraint process_events_kind_check check (kind in (
        'start',
        'continue',
        'signal',
        'timer',
        'activity_succeeded',
        'activity_failed',
        'retry_exhausted',
        'command_rejected',
        'cancellation'
    ));

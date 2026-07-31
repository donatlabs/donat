-- Receptive webhook delivery resolves one source-local wait marker by
-- instance/version, while late delivery uses bounded JSON containment over
-- durable marker history. Keep both paths indexed without indexing raw
-- provider payloads (which are never persisted).
create index process_events_wait_instance_idx
    on donat.process_events
        (source_name, instance_id, status, created_at, id)
    include (available_at, payload_json)
    where kind = 'timer';

create index process_events_webhook_wait_history_idx
    on donat.process_events
    using gin (payload_json jsonb_path_ops)
    where kind = 'timer'
      and payload_json ? 'connector_instance'
      and payload_json ? 'trigger';

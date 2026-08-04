-- Closed signal waits retain their timer marker as durable history. Late
-- command/webhook signals use this bounded containment lookup to distinguish
-- a known closed correlation from an unknown one.
create index process_events_signal_wait_history_idx
    on donat.process_events
    using gin (payload_json jsonb_path_ops)
    where kind = 'timer'
      and payload_json ? 'signal_name';

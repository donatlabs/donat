-- Collapse the occurrences the sub-second bug fanned out (v0.6.0 – v0.8.0).
--
-- Materialization kept the microseconds of the poll instant on the
-- occurrence it enqueued, so one `*/5` slot became one `scheduled` row per
-- poll, and each was delivered. The engine now enqueues the instant on the
-- grid. This puts every row already in the journal on that grid, keeping one
-- row per (trigger, second): the earliest one, which is the one a delivery
-- may already have claimed.
delete from donat.cron_events later
using donat.cron_events earlier
where later.trigger_name = earlier.trigger_name
  and date_trunc('second', later.scheduled_time) = date_trunc('second', earlier.scheduled_time)
  and (earlier.created_at, earlier.id) < (later.created_at, later.id);

update donat.cron_events
set scheduled_time = date_trunc('second', scheduled_time)
where scheduled_time <> date_trunc('second', scheduled_time);

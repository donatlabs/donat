-- Engine-internal support: the one helper the embedded engine needs at runtime.
--
-- Insert/update mutations that carry a permission `check` expression compile to
-- `CASE WHEN <condition> THEN donat.check_violation(...) END`, so the engine can
-- raise a structured 23514 constraint-violation. That function must exist.
--
-- The full Donat event-log machinery (donat.event_log + per-table AFTER
-- triggers) is NOT needed here: in the embedded model the Go executor fires the
-- registered handlers in-process from the compiled plan's Hook entries, so no
-- Postgres triggers are installed and nothing writes to an event log.
--
-- Idempotent (CREATE ... IF NOT EXISTS / OR REPLACE).

CREATE SCHEMA IF NOT EXISTS donat;

CREATE OR REPLACE FUNCTION donat.check_violation(msg text)
RETURNS json AS $$
BEGIN
    RAISE EXCEPTION USING message = msg, errcode = '23514';
END;
$$ LANGUAGE plpgsql;

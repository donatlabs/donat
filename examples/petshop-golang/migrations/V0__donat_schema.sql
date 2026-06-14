-- Engine-internal support schema.
--
-- The Donat engine requires a `donat` schema with a few helper functions and
-- tables. Normally these are created by `donat migrate`; in this standalone
-- in-memory example the Go app applies them itself at startup so no Rust
-- binary is needed at runtime.
--
-- All statements use CREATE ... IF NOT EXISTS or CREATE OR REPLACE so this
-- migration is safe to re-run (idempotent).

CREATE SCHEMA IF NOT EXISTS donat;

-- Used by insert/update mutations that have a `check` constraint: the Rust
-- sqlgen emits `CASE WHEN <condition> THEN donat.check_violation(...) END`
-- so the engine can raise a structured 23514 constraint-violation error.
CREATE OR REPLACE FUNCTION donat.check_violation(msg text)
RETURNS json AS $$
BEGIN
    RAISE EXCEPTION USING message = msg, errcode = '23514';
END;
$$ LANGUAGE plpgsql;

-- Event log: one row per captured row-change, awaiting in-process delivery.
-- In the embedded SDK model the Go hook fires synchronously after commit;
-- this table is kept for schema compatibility (the wasm core still compiles
-- the same SQL referencing donat.event_log via the PG trigger, but the
-- in-memory registry fires the Go handler before/instead of HTTP delivery).
CREATE TABLE IF NOT EXISTS donat.event_log (
    id                uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    trigger_name      text        NOT NULL,
    schema_name       text        NOT NULL,
    table_name        text        NOT NULL,
    op                text        NOT NULL,
    data_old          jsonb,
    data_new          jsonb,
    session_variables jsonb,
    status            text        NOT NULL DEFAULT 'scheduled',
    tries             int         NOT NULL DEFAULT 0,
    next_retry_at     timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS event_log_due_idx
    ON donat.event_log (status, created_at);

CREATE TABLE IF NOT EXISTS donat.event_invocation_logs (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id   uuid NOT NULL REFERENCES donat.event_log (id) ON DELETE CASCADE,
    status     int,
    request    jsonb,
    response   jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS event_invocation_logs_event_idx
    ON donat.event_invocation_logs (event_id);

-- Generic capture trigger function. Each event_trigger in the YAML metadata
-- gets a CREATE TRIGGER ... EXECUTE FUNCTION donat.notify_event() statement
-- applied by `donat migrate`; in this standalone example those triggers are
-- NOT installed (we don't run `donat migrate`). Instead the wasm core emits
-- Hook entries in the compiled plan which the Go executor fires in-process.
CREATE OR REPLACE FUNCTION donat.notify_event() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    v_old jsonb := null;
    v_new jsonb := null;
BEGIN
    IF (tg_op = 'INSERT') THEN
        v_new := to_jsonb(new);
    ELSIF (tg_op = 'UPDATE') THEN
        v_old := to_jsonb(old);
        v_new := to_jsonb(new);
    ELSIF (tg_op = 'DELETE') THEN
        v_old := to_jsonb(old);
    END IF;

    INSERT INTO donat.event_log
        (trigger_name, schema_name, table_name, op, data_old, data_new, session_variables)
    VALUES
        (tg_argv[0], tg_table_schema, tg_table_name, tg_op, v_old, v_new,
         nullif(current_setting('donat.user', true), '')::jsonb);

    RETURN null;
END;
$$;

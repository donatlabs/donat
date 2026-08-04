-- The application's own table, not the engine's.
--
-- It exists to show ExecuteTx: a host that needs its own write to be atomic
-- with a command's writes owns the transaction and hands it to the engine.
-- Nothing in metadata describes this table, and the engine never reads it.
CREATE TABLE IF NOT EXISTS public.audit_entry (
    id       bigserial PRIMARY KEY,
    actor    text        NOT NULL,
    action   text        NOT NULL,
    subject  text        NOT NULL,
    recorded timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS audit_entry_subject_idx ON public.audit_entry (subject);

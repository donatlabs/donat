-- What a durable Process writes when it finishes.
--
-- The Go host starts the Process inside the borrow statement; it cannot carry
-- it forward, because transitions and timers are a runtime loop that lives in
-- donat-server. A row here is therefore proof that something else drove the
-- Process the host originated — which is the point of the journal being
-- source-local and in the same database as the data.
CREATE TABLE IF NOT EXISTS public.loan_followup (
    id       bigserial PRIMARY KEY,
    loan_id  uuid        NOT NULL REFERENCES public.loan (id),
    note     text        NOT NULL,
    recorded timestamptz NOT NULL DEFAULT now(),
    UNIQUE (loan_id)
);

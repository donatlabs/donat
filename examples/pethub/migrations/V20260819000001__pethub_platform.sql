-- Pethub's own tables: the registry that decides which stores exist, the plans
-- that cap them, the counters a ceiling is read against, and the grants a
-- merchant writes for its own people.
--
-- None of this is Petshop's. Petshop is one store; Pethub is what runs
-- thousands of them, and everything that makes that true lives here and in
-- `metadata/`, never in the store's own files.

-- The registry. `status` gates every request: a store that is not `active` is
-- refused on every write and answers nothing on every read, however valid the
-- token in front of it still is.
CREATE TABLE public.store (
    id         text PRIMARY KEY,
    name       text NOT NULL,
    status     text NOT NULL DEFAULT 'active',
    plan_code  text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Platform-owned reference data. Every store reads the same rows and no store
-- writes any of them, which is what `shared: read_only` means and what
-- `donat validate` refuses to let a tenant-facing role break.
CREATE TABLE public.plan (
    code         text PRIMARY KEY,
    label        text NOT NULL,
    max_products bigint
);

-- The counter a ceiling moves. It is updated inside the statement that
-- performs the write, so two writers at once queue on this row rather than
-- both reading the same pre-lock count and both passing.
CREATE TABLE public.tenant_usage (
    tenant_id     text PRIMARY KEY REFERENCES public.store (id),
    product_count bigint NOT NULL DEFAULT 0
);

-- The flattened grant relation: one row per (tenant, subject, action). A
-- merchant writes these for its own people; the compiler turns them into the
-- predicate on every root its compiled roles expose.
CREATE TABLE public.iam_grant (
    id        bigserial PRIMARY KEY,
    tenant_id text NOT NULL REFERENCES public.store (id),
    user_id   text NOT NULL,
    action    text NOT NULL,
    UNIQUE (tenant_id, user_id, action)
);
CREATE INDEX ON public.iam_grant (tenant_id, user_id);

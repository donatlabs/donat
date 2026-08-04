-- A library member. `loan_limit` is per-member rather than a global constant
-- so the borrowing rule has something real to read.
CREATE TABLE IF NOT EXISTS public.member (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name       text NOT NULL,
    loan_limit int  NOT NULL DEFAULT 3
);

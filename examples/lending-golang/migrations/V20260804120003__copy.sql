-- One physical item on a shelf.
--
-- `status` is an explicit enum rather than a nullable "current loan" pointer:
-- the borrow command guards on it with an equality predicate, and a NULL-based
-- state would make that guard silently match nothing.
CREATE TABLE IF NOT EXISTS public.copy (
    id      uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    book_id uuid NOT NULL REFERENCES public.book (id),
    label   text NOT NULL,
    status  text NOT NULL DEFAULT 'available'
            CHECK (status IN ('available', 'on_loan'))
);

CREATE INDEX IF NOT EXISTS copy_book_id_idx ON public.copy (book_id);

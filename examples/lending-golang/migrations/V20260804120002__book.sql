-- A title. Physical items are `copy` rows; a member borrows a copy, not a book.
CREATE TABLE IF NOT EXISTS public.book (
    id     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    title  text NOT NULL,
    author text NOT NULL
);

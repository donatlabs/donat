-- One borrowing. Closed loans keep their row: the borrowing limit counts
-- `status = 'active'`, so history never costs a member their next loan.
CREATE TABLE IF NOT EXISTS public.loan (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    copy_id     uuid NOT NULL REFERENCES public.copy (id),
    member_id   uuid NOT NULL REFERENCES public.member (id),
    status      text NOT NULL DEFAULT 'active'
                CHECK (status IN ('active', 'returned')),
    borrowed_on date NOT NULL,
    due_on      date NOT NULL,
    returned_on date,
    extensions  int  NOT NULL DEFAULT 0
);

-- The borrow command counts a member's open loans through this index.
CREATE INDEX IF NOT EXISTS loan_member_status_idx
    ON public.loan (member_id, status);

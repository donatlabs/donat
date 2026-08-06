-- A loan receipt: a PDF the service renders when a book is borrowed.
--
-- The file is an ordinary uuid column on the loan's own table, so the row's
-- permissions govern it and nothing else has to. `donat.file_uploads` holds
-- the file itself; this column only points at it.
ALTER TABLE public.loan ADD COLUMN receipt uuid;

COMMENT ON COLUMN public.loan.receipt IS
  'The rendered borrowing receipt, stored by the render_loan_receipt action.';

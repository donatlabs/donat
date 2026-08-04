-- A file attachment on the customer record (spec 008).
--
-- The column is an ordinary uuid holding an upload id. What makes it a file is
-- the `attachments:` declaration in the table's metadata, beside the
-- permissions that decide who may write it — and therefore who may ask for an
-- upload URL for it.
alter table public.customer
    add column if not exists avatar uuid;

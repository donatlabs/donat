-- The binding an adopting application supplies, played here by the smallest
-- users table that satisfies the contract. This is test data, not part of the
-- module: it lives outside `migrations/` so a deployment applying the module's
-- schema never gets it, and `donat.test.yaml` names it explicitly.
--
-- `create or replace view` is the contract itself — a binding whose columns did
-- not match the shape the module ships would fail right here, which is what
-- makes this the same check an adopter gets.
create table if not exists public.app_user (
  id             uuid primary key,
  email          text not null,
  email_verified boolean not null default false,
  locale         text not null default 'en',
  timezone       text not null default 'UTC'
);

create or replace view notification.recipient as
select u.id::text as id, u.locale, u.timezone from public.app_user u;

-- `nullif` rather than `where email is not null`: a recipient whose address is
-- blank is a recipient with a row and no address, which is the case the module
-- records as unreachable rather than queues forever.
create or replace view notification.recipient_address as
select u.id::text as recipient_id,
       'email'::text as channel,
       nullif(u.email, '') as address,
       u.email_verified as verified
from public.app_user u;

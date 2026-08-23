-- The petshop's half of the notification module's contract.
--
-- The module ships `notification.recipient` as a view with the right shape and
-- no rows; this replaces the body with the store's own customers. `create or
-- replace view` refuses a replacement whose column names or types differ, so a
-- binding that does not fit is a failed migration here rather than a
-- notification that quietly goes nowhere.
--
-- `id` is `customer.customer_id` and not `customer.id`, because the module
-- matches a recipient against the `X-Donat-User-Id` session variable and that
-- is what this store puts there — the `bigserial` is storage, not identity.
--
-- Two columns the store does not keep are answered with constants rather than
-- left out: every address here is one the customer registered with, and the
-- shop has one locale and one timezone. A store that grows per-customer
-- settings changes this view and nothing else.
create or replace view notification.recipient as
select
    c.customer_id  as id,
    c.email        as email,
    true           as email_verified,
    'en'::text     as locale,
    'UTC'::text    as timezone
from public.customer c;

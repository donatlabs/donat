-- Renamed out of the way of the notification module.
--
-- This table predates `modules/notifications` and does a different job: it
-- records what an outbound *provider* said about one message the store's own
-- mock notification connector sent. The module tracks `notification.delivery`,
-- whose GraphQL name is `<schema>_<table>` — `notification_delivery` — and two
-- tracked tables cannot share a type name.
--
-- The store is the one adopting, so the store is the one that moves. Any
-- application adopting the module hits the same rule: the module owns the
-- `notification_*` GraphQL namespace, and a `public.notification_something` of
-- your own has to be renamed or left untracked.
alter table notification_delivery rename to provider_notification_receipt;
alter index notification_delivery_provider_message_unique
  rename to provider_notification_receipt_message_unique;

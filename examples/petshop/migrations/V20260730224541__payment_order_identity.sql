-- A checkout owns exactly one payment aggregate. Commands may select it by
-- order_id only because this database constraint makes that lookup singular.
CREATE UNIQUE INDEX payment_order_id_unique ON payment (order_id);

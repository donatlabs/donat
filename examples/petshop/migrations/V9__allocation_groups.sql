CREATE VIEW inventory_allocation_group AS
SELECT
  allocation_id::petshop_required_uuid AS allocation_id,
  order_id::petshop_required_uuid AS order_id,
  stock_location_code::petshop_required_text AS stock_location_code,
  status::petshop_required_text AS status,
  sum(quantity)::petshop_required_int8 AS quantity,
  currency::petshop_required_text AS currency
FROM inventory_allocation
GROUP BY allocation_id, order_id, stock_location_code, status, currency;

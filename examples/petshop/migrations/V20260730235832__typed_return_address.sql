CREATE OR REPLACE VIEW order_return_context AS
SELECT DISTINCT ON (orders.id)
  orders.id::petshop_required_uuid AS order_id,
  orders.customer_id::petshop_required_text AS customer_id,
  payment.id::petshop_required_uuid AS payment_id,
  orders.currency::petshop_required_text AS currency,
  jsonb_build_object(
    'recipient_name', customer.name,
    'address_line_1', customer_address.line1,
    'address_line_2', customer_address.line2,
    'city', customer_address.city,
    'region', '',
    'postal_code', customer_address.postal_code,
    'country_code', customer_address.country_code
  )::petshop_required_jsonb AS return_from,
  customer.name::petshop_required_text AS recipient_name,
  customer_address.line1::petshop_required_text AS address_line_1,
  customer_address.line2 AS address_line_2,
  customer_address.city::petshop_required_text AS city,
  ''::petshop_required_text AS region,
  customer_address.postal_code::petshop_required_text AS postal_code,
  customer_address.country_code::petshop_required_text AS country_code
FROM orders
JOIN payment ON payment.order_id = orders.id
JOIN customer ON customer.customer_id = orders.customer_id
JOIN customer_address ON customer_address.customer_id = orders.customer_id
WHERE payment.status IN ('captured', 'paid', 'refunded')
ORDER BY orders.id, payment.internal_id DESC, customer_address.id;

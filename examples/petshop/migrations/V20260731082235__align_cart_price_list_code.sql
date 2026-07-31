-- The price-list decision table routes a standard web shopper to `retail_usd`,
-- but this view published the candidate lines under `retail`, so the checkout
-- quote step matched no rows and every checkout failed before pricing. Publish
-- the code the decision actually returns.
CREATE OR REPLACE VIEW cart_price_candidate AS
SELECT
  cart_line.cart_id::petshop_required_int8 AS cart_id,
  cart.customer_id::petshop_required_text AS customer_id,
  'retail_usd'::petshop_required_text AS price_list_code,
  (row_number() OVER (
    PARTITION BY cart_line.cart_id
    ORDER BY cart_line.id, cart_line.variant_id
  )::integer)::petshop_required_int4 AS line_sequence,
  product_variant.id::petshop_required_int8 AS variant_id,
  product_variant.sku::petshop_required_text AS sku,
  product.title::petshop_required_text AS title,
  cart_line.quantity::petshop_required_int4 AS quantity,
  category.slug::petshop_required_text AS taxable_category,
  product_variant.price_minor::petshop_required_int8 AS unit_price_minor,
  (product_variant.price_minor * cart_line.quantity)::petshop_required_int8 AS line_subtotal_minor,
  product_variant.currency::petshop_required_text AS currency
FROM cart_line
JOIN cart ON cart.id = cart_line.cart_id
JOIN product_variant ON product_variant.id = cart_line.variant_id
JOIN product ON product.id = product_variant.product_id
JOIN category ON category.id = product.category_id
WHERE product.status = 'published' AND product_variant.active;

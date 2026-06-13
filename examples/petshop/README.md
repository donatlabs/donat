# Petshop example

A classic pet-store running on **dist-api** — a small catalogue of pets in
categories, customers, and their orders — wired up with the permission set a
normal store needs: a public catalogue, authenticated shoppers, store staff,
and the built-in admin.

```
docker compose up
```

This pulls the prebuilt public engine image
(`ghcr.io/pantyukhov/dist-api`, published by the release workflow), starts
Postgres seeded from [`db/init.sql`](db/init.sql), and serves GraphQL at
<http://localhost:8080/v1/graphql>. The schema (tables + foreign keys) comes
from the seeded database; the [`metadata/`](metadata) directory adds
relationships and the per-role permissions described below.

> The image is built and pushed only on release tags (`v*`). Before the first
> release exists, build it locally from the repo root instead:
> `docker build -t ghcr.io/pantyukhov/dist-api:latest .`

## Data model

| Table        | Purpose                                            |
|--------------|----------------------------------------------------|
| `category`   | Catalogue sections (Dogs, Cats, …)                 |
| `pet`        | Items for sale, with `status` available/pending/sold |
| `customer`   | Shoppers; `id` is the `X-Hasura-User-Id` value     |
| `orders`     | A customer's order with a fulfilment `status`      |
| `order_item` | Line items linking an order to pets                |

Relationships: `pet.category`, `category.pets`, `orders.customer`,
`customer.orders`, `orders.items`, `order_item.order`, `order_item.pet`.

## Roles

| Role        | Who                | Can do                                                                 |
|-------------|--------------------|-----------------------------------------------------------------------|
| `anonymous` | unauthenticated    | Browse categories and **available** pets only. No customer/order data.|
| `customer`  | a logged-in shopper| See own profile/orders, browse available pets, place orders for self.  |
| `staff`     | store employee     | Full inventory CRUD, read every customer/order, update order status.   |
| `admin`     | built-in           | Everything, no row/column limits.                                      |

`anonymous` is the `HASURA_GRAPHQL_UNAUTHORIZED_ROLE`: any request with no role
falls back to it. The admin secret is `petshop-secret` (see
`docker-compose.yml`) — a trusted request acts as admin and may impersonate any
role with the `X-Hasura-Role` header. In production, set a real secret and
issue JWTs instead of passing roles by hand.

## Try it

All examples below `POST` to `http://localhost:8080/v1/graphql`.

### Public catalogue (anonymous)

No headers needed — only the 4 available pets come back; `Nemo` (sold) and
`Shadow` (pending) are filtered out by the permission.

```bash
curl -s localhost:8080/v1/graphql -H 'content-type: application/json' -d '{
  "query": "{ category { name pets { name price status } } }"
}'
```

### Shopper (customer, impersonated as customer id 1)

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-hasura-admin-secret: petshop-secret' \
  -H 'x-hasura-role: customer' \
  -H 'x-hasura-user-id: 1' \
  -d '{ "query": "{ customer { name email orders { id status items { quantity pet { name } } } } }" }'
```

Returns only customer `1`'s own profile and orders — `customer 2`'s data is
invisible. Browsing `pet` still shows only available pets.

Place an order (the `customer_id` is forced to the session user by a preset, so
shoppers cannot order on someone else's behalf):

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-hasura-admin-secret: petshop-secret' \
  -H 'x-hasura-role: customer' \
  -H 'x-hasura-user-id: 1' \
  -d '{ "query": "mutation { insert_orders_one(object: {status: \"placed\"}) { id customer_id status } }" }'
```

### Store staff

Staff see every pet (including sold/pending) and every order, and can change an
order's fulfilment status:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-hasura-admin-secret: petshop-secret' \
  -H 'x-hasura-role: staff' \
  -d '{ "query": "mutation { update_orders(where: {id: {_eq: 1}}, _set: {status: \"shipped\"}) { affected_rows } }" }'
```

### Admin

Send the admin secret with **no** role header for unrestricted access:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-hasura-admin-secret: petshop-secret' \
  -d '{ "query": "{ customer { id name orders_aggregate { aggregate { count } } } }" }'
```

## Reset

```bash
docker compose down -v   # also drops the seeded database volume
```

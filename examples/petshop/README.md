# Petshop example

A classic pet-store running on **donat** — a small catalogue of pets in
categories, customers, and their orders — wired up with the permission set a
normal store needs: a public catalogue, authenticated shoppers, and store
staff. Every access goes through an explicit role permission — there is no
admin role.

```
docker compose up
```

All services use the same prebuilt public engine image
(`ghcr.io/pantyukhov/donat`, published by the release workflow) and follow
the project's deploy model:

1. **`migrate`** — `donat migrate` applies the versioned DDL in
   [`migrations/`](migrations) (one `V{n}__create_<table>.sql` per table) via
   refinery, tracked in `refinery_schema_history`. This is the only thing that
   runs DDL.
2. **`validate`** — `donat validate` loads the [`metadata/`](metadata),
   introspects the migrated database, and exits non-zero if anything tracked
   is missing, so a bad deploy fails before the server boots.
3. **`engine`** — serves GraphQL at <http://localhost:8080/v1/graphql>. The
   schema (tables + foreign keys) comes from the migrated database; the
   metadata directory adds relationships and the per-role permissions below.
   The serving engine never runs DDL and exposes no runtime `run_sql`.

> The image is built and pushed only on release tags (`v*`). Before the first
> release exists, build it locally from the repo root instead:
> `docker build -t ghcr.io/pantyukhov/donat:latest .`
> (The image needs the `migrate`/`validate` subcommands, so build from a
> revision that includes them.)

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

There is **no admin role**: every request runs as one of the roles above,
each scoped by its explicit permissions. `anonymous` is the
`HASURA_GRAPHQL_UNAUTHORIZED_ROLE` — any request with no/role-less auth falls
back to it. The secret `petshop-secret` (see `docker-compose.yml`) marks a
request as *trusted* so it may assert a role via the `X-Hasura-Role` header (a
demo stand-in for edge auth); a trusted request must still name a role. In
production, issue JWTs instead of passing roles by hand.

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

> A request with **no** role — even with the secret — runs as `anonymous`
> (the unauthorized-role fallback); there is no admin role or bypass. To read
> across all customers, ask as `staff`. (If `HASURA_GRAPHQL_UNAUTHORIZED_ROLE`
> were unset, a trusted role-less request would instead be rejected with
> `x-hasura-role header is required`.)

## Reset

```bash
docker compose down -v   # also drops the seeded database volume
```

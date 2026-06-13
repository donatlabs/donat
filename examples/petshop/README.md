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
(`ghcr.io/donatlabs/donat`, published by the release workflow) and follow
the project's deploy model:

1. **`migrate`** — `donat migrate` applies the versioned DDL in
   [`migrations/`](migrations) (one `V{n}__create_<table>.sql` per table) via
   refinery, tracked in `refinery_schema_history`. This is the only thing that
   runs DDL.
2. **`validate`** — `donat validate` loads the [`metadata/`](metadata),
   introspects the migrated database, and exits non-zero if anything tracked
   is missing, so a bad deploy fails before the server boots.
3. **`engine`** — serves the data plane over three transports, all sharing the
   same per-role permissions and auth: GraphQL at
   <http://localhost:8080/v1/graphql>, RESTified endpoints under
   <http://localhost:8080/api/rest/> (see [REST endpoints](#rest-endpoints)),
   and an MCP server at <http://localhost:8080/mcp> (see [MCP](#mcp)). The
   schema (tables + foreign keys) comes from the migrated database; the
   metadata directory adds relationships, the per-role permissions below, the
   saved queries in [`metadata/query_collections.yaml`](metadata/query_collections.yaml),
   and the REST routes in [`metadata/rest_endpoints.yaml`](metadata/rest_endpoints.yaml).
   The serving engine never runs DDL and exposes no runtime `run_sql`.
   All three surfaces are on by default; restrict them at deploy time with
   `DONAT_GRAPHQL_ENABLED_APIS` (comma-separated `graphql`/`rest`/`mcp`), e.g.
   `DONAT_GRAPHQL_ENABLED_APIS=graphql` to expose GraphQL only (REST and MCP
   then return `404`).

> The image is built and pushed only on release tags (`v*`). Before the first
> release exists, build it locally from the repo root instead:
> `docker build -t ghcr.io/donatlabs/donat:latest .`
> (The image needs the `migrate`/`validate` subcommands, so build from a
> revision that includes them.)

## Data model

| Table        | Purpose                                            |
|--------------|----------------------------------------------------|
| `category`   | Catalogue sections (Dogs, Cats, …)                 |
| `pet`        | Items for sale, with `status` available/pending/sold |
| `customer`   | Shoppers; `id` is the `X-Donat-User-Id` value     |
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
`DONAT_GRAPHQL_UNAUTHORIZED_ROLE` — any request with no/role-less auth falls
back to it. The secret `petshop-secret` (see `docker-compose.yml`) marks a
request as *trusted* so it may assert a role via the `X-Donat-Role` header (a
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
  -H 'x-donat-admin-secret: petshop-secret' \
  -H 'x-donat-role: customer' \
  -H 'x-donat-user-id: 1' \
  -d '{ "query": "{ customer { name email orders { id status items { quantity pet { name } } } } }" }'
```

Returns only customer `1`'s own profile and orders — `customer 2`'s data is
invisible. Browsing `pet` still shows only available pets.

Place an order (the `customer_id` is forced to the session user by a preset, so
shoppers cannot order on someone else's behalf):

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' \
  -H 'x-donat-role: customer' \
  -H 'x-donat-user-id: 1' \
  -d '{ "query": "mutation { insert_orders_one(object: {status: \"placed\"}) { id customer_id status } }" }'
```

### Store staff

Staff see every pet (including sold/pending) and every order, and can change an
order's fulfilment status:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' \
  -H 'x-donat-role: staff' \
  -d '{ "query": "mutation { update_orders(where: {id: {_eq: 1}}, _set: {status: \"shipped\"}) { affected_rows } }" }'
```

> A request with **no** role — even with the secret — runs as `anonymous`
> (the unauthorized-role fallback); there is no admin role or bypass. To read
> across all customers, ask as `staff`. (If `DONAT_GRAPHQL_UNAUTHORIZED_ROLE`
> were unset, a trusted role-less request would instead be rejected with
> `x-donat-role header is required`.)

## REST endpoints

The same data is also reachable over Donat **RESTified endpoints**: each route
in [`metadata/rest_endpoints.yaml`](metadata/rest_endpoints.yaml) maps an HTTP
method + URL template to a saved GraphQL operation in
[`metadata/query_collections.yaml`](metadata/query_collections.yaml). They run
through the *same* permission system as GraphQL — no admin bypass — so the rows
you get depend on your role. Path params, query-string keys, and JSON-body keys
bind the operation's GraphQL variables (precedence: path > query > body). A
successful call returns the GraphQL `data` object directly.

| Method & URL                  | Saved query     | Notes                                    |
|-------------------------------|-----------------|------------------------------------------|
| `GET /api/rest/pet/:id`       | `PetById`       | One pet; available-only for shoppers     |
| `GET /api/rest/pets?limit=N`  | `AvailablePets` | The catalogue the role may browse        |
| `GET /api/rest/categories`    | `Categories`    | Categories with their visible pets        |
| `POST /api/rest/pet`          | `CreatePet`     | Add inventory (staff only); body → vars  |

Browse the catalogue as the public (no headers → `anonymous`):

```bash
curl -s 'localhost:8080/api/rest/pets?limit=3'
# {"pet":[{"id":1,"name":"Rex",...},{"id":2,...},{"id":3,...}]}
```

The permission travels with the route — `Shadow` (pending) is hidden from the
public but visible to staff:

```bash
curl -s localhost:8080/api/rest/pet/4
# {"pet_by_pk":null}

curl -s localhost:8080/api/rest/pet/4 \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff'
# {"pet_by_pk":{"id":4,"name":"Shadow","status":"pending",...,"category":{"name":"Cats"}}}
```

Add a pet (staff only — the same call as `anonymous` comes back with a
`validation-failed` error and changes nothing):

```bash
curl -s localhost:8080/api/rest/pet \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"name":"Coco","category_id":3,"price":45,"status":"available","description":"Talkative parrot"}'
# {"insert_pet":{"affected_rows":1,"returning":[{"id":7,"name":"Coco",...}]}}
```

An unknown route is `404`; a known route called with the wrong method is `405`.

## MCP

The engine also speaks the **Model Context Protocol** over streamable HTTP at
`POST /mcp` (JSON-RPC 2.0, JSON mode), so an LLM client can read and write the
store under a role. It exposes six generic, table-parameterized tools —
`list_tables`, `describe_table`, `query`, `insert`, `update`, `delete` — each of
which runs as the request's role through the same permission system (a tool
call lacking permission comes back as `isError`, never a bypass).

Point an HTTP-capable MCP client at `http://localhost:8080/mcp` and send the
role headers with each request (here the demo secret + `X-Donat-Role`; in
production, a JWT). List the tools:

```bash
curl -s localhost:8080/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

Query the inventory as staff (arguments are passed as GraphQL variables — a
`where` filter, `order_by`, `limit`, …):

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"query",
        "arguments":{"table":"pet","columns":["id","name","status"],
                     "where":{"status":{"_eq":"pending"}},"order_by":{"id":"asc"}}}}'
# result.structuredContent: [{"id":4,"name":"Shadow","status":"pending"}]
```

Insert as staff (`update`/`delete` take a `where` + `set` the same way):

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"insert",
        "arguments":{"table":"pet","objects":[{"name":"Milo","category_id":2,"price":80,"status":"available"}],
                     "returning":["id","name"]}}}'
# result.structuredContent: {"affected_rows":1,"returning":[{"id":8,"name":"Milo"}]}
```

`list_tables` reports only what the role may touch — as `staff` it lists every
table with its allowed operations; as `anonymous` it shows just the catalogue.

## Reset

```bash
docker compose down -v   # also drops the seeded database volume
```

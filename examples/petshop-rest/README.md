# Petshop — REST example

A pet store served by **donat** over **RESTified endpoints only**. Same
schema, roles, and per-role permissions as the main [petshop](../petshop)
example, but the engine is started with `DONAT_GRAPHQL_ENABLED_APIS=rest`, so
the only mounted surface is `/api/rest/...` — `/v1/graphql` and `/mcp` return
`404`. There is no admin role: every request runs as an explicit role.

```
docker compose up
```

Deploy model (one-shot `migrate` → `validate` → serve) is identical to the
main example; only the served surface differs.

## How REST works here

Each route in [`metadata/rest_endpoints.yaml`](metadata/rest_endpoints.yaml)
maps an HTTP method + URL template to a saved GraphQL operation in
[`metadata/query_collections.yaml`](metadata/query_collections.yaml). The
endpoint runs through the same per-role pipeline as GraphQL would — the rows
you get depend on your role. Path params, query-string keys, and JSON-body
keys bind the operation's GraphQL variables (precedence **path > query >
body**). A successful call returns the GraphQL `data` object directly.

| Method & URL                  | Saved query     | Notes                                    |
|-------------------------------|-----------------|------------------------------------------|
| `GET /api/rest/pet/:id`       | `PetById`       | One pet; available-only for shoppers     |
| `GET /api/rest/pets?limit=N`  | `AvailablePets` | The catalogue the role may browse        |
| `GET /api/rest/categories`    | `Categories`    | Categories with their visible pets        |
| `POST /api/rest/pet`          | `CreatePet`     | Add inventory (staff only); body → vars  |

## Roles

| Role        | Who             | Sees / can do                                    |
|-------------|-----------------|--------------------------------------------------|
| `anonymous` | unauthenticated | Browse categories and **available** pets only.   |
| `customer`  | logged-in shopper | Available pets + own profile/orders.           |
| `staff`     | store employee  | Every pet (incl. pending/sold); full inventory.  |

`anonymous` is the `DONAT_GRAPHQL_UNAUTHORIZED_ROLE`: a request with no role
falls back to it. The secret `petshop-secret` marks a request *trusted* so it
may assert a role via `X-Donat-Role` (a demo stand-in for edge auth / JWTs).

## Try it

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

Add a pet (staff only — the same call as `anonymous` returns a
`validation-failed` error and changes nothing):

```bash
curl -s localhost:8080/api/rest/pet \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"name":"Coco","category_id":3,"price":45,"status":"available","description":"Talkative parrot"}'
# {"insert_pet":{"affected_rows":1,"returning":[{"id":7,"name":"Coco",...}]}}
```

An unknown route is `404`; a known route called with the wrong method is `405`.
Because only the REST surface is enabled, `POST /v1/graphql` and `POST /mcp`
also return `404`.

## Reset

```bash
docker compose down -v   # also drops the seeded database volume
```

---
name: donat-api-surfaces
description: Use when exposing a donat application over REST without writing controllers, publishing permission-scoped MCP tools for an agent, or deciding which surfaces to mount.
---

# GraphQL, REST and MCP

The three surfaces are not parallel stacks. REST and MCP translate into the
same pipeline as GraphQL, so filters, error contracts and permissions cannot
drift by transport. A mounted route still needs an explicit role and a matching
permission — publishing something does not grant it.

All request-facing surfaces are on by default. Restrict at deploy time with
`DONAT_GRAPHQL_ENABLED_APIS=graphql`.

## GraphQL: nothing to declare

Tracking a table with a permission is what creates its GraphQL roots. Queries,
mutations, subscriptions, Relay connections, aggregates, relationships,
computed fields, JSONB and PostGIS all follow from the declaration. See
`donat-tables-and-permissions`.

Roles are asserted per request:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $TOKEN" \
  -H 'x-donat-role: customer' \
  -H 'x-donat-user-id: customer-1' \
  -d '{"query":"{ orders { id order_status total_minor } }"}'
```

In production those headers come from a JWT rather than by hand — the secret
only marks a request as *trusted to assert a role*, and is never a permission.

## Saved operations: `query_collections.yaml`

REST and MCP both publish **saved** GraphQL operations, so the shape a client
sees is reviewed rather than composed at request time.

```yaml
- name: petshop
  definition:
    queries:
      - name: Products
        query: |
          query Products {
            product(where: {status: {_eq: "published"}}, order_by: {slug: asc}) {
              slug
              title
              variants(order_by: {sku: asc}) {
                sku
                price_minor
                currency
                stock { available_quantity }
              }
            }
          }
      - name: ProductBySlug
        query: |
          query ProductBySlug($slug: String!) {
            product(where: {slug: {_eq: $slug}}) { slug title description }
          }
```

Name operations after the resource and the lookup — `Products`,
`ProductBySlug`, `Cart`, `UpsertCartLine`. The name is the API's vocabulary.

## REST: `rest_endpoints.yaml`

An endpoint binds a URL and methods to a saved operation. Path parameters,
query string and body become the operation's variables.

```yaml
- name: get_product_by_slug
  url: products/:slug
  methods: [GET]
  definition:
    query:
      collection_name: petshop
      query_name: ProductBySlug
  comment: Read one published product by slug.

- name: put_cart_lines
  url: cart/lines
  methods: [PUT]
  definition:
    query:
      collection_name: petshop
      query_name: UpsertCartLine
  comment: Reuse the permission-aware GraphQL cart-line upsert.
```

`:slug` in the URL supplies `$slug`. A mutation endpoint reuses the same
permission-aware operation the GraphQL client would call — there is no second
authorization path to keep in sync, which is the whole point.

Served under `/api/rest/<url>`.

## MCP: `mcp.yaml`

A tracked table or a saved operation is **not agent-visible unless it is listed
here**. Publication is an explicit act, separate from the permission.

```yaml
tools:
  - name: catalogue.search
    title: Search the catalogue
    description: List currently visible pets in catalogue order.
    source:
      saved_query:
        collection: agent
        query: AvailablePets
    permissions: [anonymous, customer, staff]
    arguments:
      limit: Maximum number of catalogue items to return.

table_tools:
  - table: { schema: public, name: pet }
    description: Staff inventory operations.
    operations:
      - operation: query
        name: inventory.lookup
        description: Search the inventory with a filter and order.
        permissions: [staff]
      - operation: insert
        name: inventory.create
        description: Create an inventory item.
        permissions: [staff]
      - operation: update
        name: inventory.update
        description: Update matching inventory items.
        permissions: [staff]
```

- `tools` publish a saved operation as a narrow, named tool. Prefer these:
  a fixed query with a documented argument is a far smaller surface than
  generic CRUD.
- `table_tools` publish generic query/insert/update over a table. Every
  operation is still bound by the role's permissions — an agent running as
  `staff` sees exactly what a `staff` request sees.
- `permissions:` on a tool lists which roles may see and call it. A tool with a
  role the table does not grant is a tool that always fails; `donat validate`
  is the place to catch it.
- `description` is the agent's only instruction. Write it for a model that has
  never read your schema: what it returns, in what order, and what the
  arguments mean.

Served at `/mcp` (streamable HTTP).

## The publishing checklist

1. The permission exists and is right — publication does not grant anything.
2. The operation is saved in `query_collections.yaml` and named for what it
   returns.
3. The REST route or MCP tool binds it, with a `comment`/`description` written
   for whoever calls it.
4. A test asks as the *wrong* role and gets nothing.
5. If the surface is not needed, leave it out and narrow
   `DONAT_GRAPHQL_ENABLED_APIS`.

## Files to read

- [`examples/petshop-rest/`](https://github.com/donatlabs/donat/tree/main/examples/petshop-rest) — a whole REST API: five tables, saved operations,
  endpoints, migrations. The fastest complete example in the repository.
- [`examples/petshop-mcp/metadata/mcp.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop-mcp/metadata/mcp.yaml) — the agent contract above
- `examples/petshop/metadata/{query_collections,rest_endpoints}.yaml`
- `crates/conformance/tests/{rest_endpoints,mcp_tools}.rs`

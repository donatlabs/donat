# Petshop — MCP example

A pet store served by **donat** over the **Model Context Protocol only**. Same
schema, roles, and per-role permissions as the main [petshop](../petshop)
example, but the engine is started with `DONAT_GRAPHQL_ENABLED_APIS=mcp`, so
the only mounted surface is `POST /mcp` — `/v1/graphql` and `/api/rest` return
`404`. An LLM client reads and writes the store under a role; there is no admin
role, and every tool call goes through the same per-role permission system.

```
docker compose up
```

Deploy model (one-shot `migrate` → `validate` → serve) is identical to the
main example; only the served surface differs.

## The MCP server

`POST /mcp` speaks JSON-RPC 2.0 over streamable HTTP (JSON mode, protocol
`2025-06-18`): `initialize`, `tools/list`, `tools/call`. It exposes six
generic, table-parameterized tools — `list_tables`, `describe_table`,
`query`, `insert`, `update`, `delete` — derived from the tracked tables and
the caller's permissions. A tool call without permission comes back as
`isError`, never a bypass.

## Auth

The MCP request carries the same headers as the rest of the engine. In this
demo, the trusted secret `petshop-secret` plus an `X-Donat-Role` header pick
the role; in production you would send a JWT (`Authorization: Bearer <jwt>`)
and the role comes from its claims. `tools/list` needs no role; `tools/call`
does.

## Connect an MCP client

Point an HTTP-capable MCP client at `http://localhost:8080/mcp` and attach the
role headers. For example, a project `.mcp.json`:

```json
{
  "mcpServers": {
    "petshop": {
      "type": "http",
      "url": "http://localhost:8080/mcp",
      "headers": {
        "X-Donat-Admin-Secret": "petshop-secret",
        "X-Donat-Role": "staff"
      }
    }
  }
}
```

For a stdio-only client, bridge with `mcp-remote`:

```json
{
  "mcpServers": {
    "petshop": {
      "command": "npx",
      "args": [
        "mcp-remote", "http://localhost:8080/mcp",
        "--header", "X-Donat-Admin-Secret:petshop-secret",
        "--header", "X-Donat-Role:staff"
      ]
    }
  }
}
```

## Try it with curl

List the tools (no role required):

```bash
curl -s localhost:8080/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

`describe_table` returns each column's type and its **description** (sourced
from `configuration.column_config.<col>.comment` in the table metadata):

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"describe_table","arguments":{"table":"pet"}}}'
# ... structuredContent.columns: [{"name":"status","type":"text","nullable":false,
#     "description":"Availability of the pet. Exactly one of: \"available\" ..."}, ...]
# The comments come from configuration.column_config.<col>.comment in the table
# metadata — write them verbosely; the text is what an LLM reads to use a column.
```

Query the inventory as staff (arguments are passed as GraphQL variables — a
`where` filter, `order_by`, `limit`, …):

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
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
  -d '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
        "name":"insert",
        "arguments":{"table":"pet","objects":[{"name":"Milo","category_id":2,"price":80,"status":"available"}],
                     "returning":["id","name"]}}}'
# result.structuredContent: {"affected_rows":1,"returning":[{"id":8,"name":"Milo"}]}
```

`list_tables` reports only what the role may touch — as `staff` every table
with its allowed operations; as `anonymous` just the catalogue. A mutating
call under a role without permission returns `isError: true` and writes
nothing.

## Reset

```bash
docker compose down -v   # also drops the seeded database volume
```

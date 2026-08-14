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

`POST /mcp` speaks JSON-RPC 2.0 over HTTP: `initialize`, `tools/list`, and
`tools/call`. The separate [`metadata/mcp.yaml`](metadata/mcp.yaml) is an
explicit publication list: it exposes a curated catalogue search plus three
staff-only inventory operations. A tracked table or saved GraphQL query is not
agent-visible unless that file names it. Tool calls still run through the
normal GraphQL planner, so a tool never bypasses a role, row filter, allowlist,
or column permission.

## Auth

The MCP request carries the same auth as the rest of the engine: a verified
token (`Authorization: Bearer <jwt>`), with the role read from its claims. An
`X-Donat-Role` header only *picks* between several roles one token carries.
Curated `tools/list` and `tools/call` both need a role, because the published
catalogue is role-scoped.

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
        "Authorization": "Bearer <token from ../mint-token.sh staff>"
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
        "--header", "Authorization:Bearer <token from ../mint-token.sh staff>"
      ]
    }
  }
}
```

## Try it with curl

A role comes from a verified token; this engine has no admin role and no
header that names one. Mint one for this stand with the shared development
key:

```bash
STAFF=$(../mint-token.sh staff)
```

List the tools for a role:

```bash
curl -s localhost:8080/mcp -H 'content-type: application/json' \
  -H "authorization: Bearer $STAFF" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

Call the curated inventory lookup tool:

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $STAFF" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"inventory.lookup",
        "arguments":{"columns":["id","name","status"],"order_by":{"id":"asc"}}}}'
```

Search the catalogue as any permitted role; its variables are passed through
to the saved GraphQL operation:

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $STAFF" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"catalogue.search", "arguments":{"limit":10}}}'
```

Create an inventory item as staff:

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $STAFF" \
  -d '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
        "name":"inventory.create",
        "arguments":{"objects":[{"name":"Milo","category_id":2,"price":80,"status":"available"}],
                     "returning":["id","name"]}}}'
# result.structuredContent: {"affected_rows":1,"returning":[{"id":8,"name":"Milo"}]}
```

A mutating call under a role without permission is absent from discovery and
returns `isError: true` if called directly; it writes nothing.

## Reset

```bash
docker compose down -v   # also drops the seeded database volume
```

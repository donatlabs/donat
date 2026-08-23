"""The same store, through three different doors.

GraphQL, the RESTified endpoints and MCP are separate transports over one
permission system. A tester's question is whether the door changes the answer —
it must not, neither the rows nor the refusals.
"""

from __future__ import annotations


# -- the walls are the same height on every door ----------------------------


def test_mcp_lists_only_the_tables_a_role_may_touch(anonymous, staff):
    public = anonymous.mcp_tool("list_tables", {})
    inside = staff.mcp_tool("list_tables", {})

    public_tables = {row["name"] for row in public.value("result/structuredContent/tables", [])}
    staff_tables = {row["name"] for row in inside.value("result/structuredContent/tables", [])}

    assert public_tables, f"the public sees the catalogue: {public.text[:200]}"
    assert "orders" not in public_tables
    assert staff_tables > public_tables, "staff see at least what the public sees, and more"


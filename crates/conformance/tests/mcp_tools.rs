//! MCP conformance: a hand-rolled JSON-RPC 2.0 server at `POST /mcp` exposes
//! generic CRUD + discovery tools (`list_tables`, `describe_table`, `query`,
//! `insert`, `update`, `delete`). Each tool renders a parametrized GraphQL
//! operation (tool arguments become GraphQL variables) and runs it through the
//! normal pipeline, so per-role permissions gate every call.
//!
//! The harness compares the JSON-RPC `result` but ignores the `content` field
//! (a text duplicate of `structuredContent`); see `strip_mcp_content` in the
//! harness lib.
//!
//! Fixtures that mutate shared rows are sequenced so expectations stay stable:
//! the reads (`query`, `query_filter`) run before `insert`/`update`/`delete`,
//! and the mutations touch distinct rows.

use donat_conformance::{Suite, Transport};

const MCP: &str = "mcp";

#[test]
fn mcp_tools() {
    let s = Suite::new("mcp_tools").start();
    s.setup_v1q(&format!("{MCP}/setup.yaml"));

    for f in [
        "initialize.yaml",
        "tools_list.yaml",
        "permission_denied.yaml",
        "query.yaml",
        "query_filter.yaml",
        "insert.yaml",
        "update.yaml",
        "delete.yaml",
    ] {
        s.check_query_f(&format!("{MCP}/{f}"), Transport::Http);
    }

    s.teardown_v1q(&format!("{MCP}/teardown.yaml"));
}

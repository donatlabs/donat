//! Attack-coverage suite (not ported from tests-py): classic broken-access-
//! control / IDOR attempts and SQL-injection payloads, exercised end-to-end
//! against a multi-tenant schema where a row's `owner` must equal the caller's
//! X-Donat-User-Id. This engine has no admin role, so every case runs as the
//! `user` role scoped strictly by its permissions.
//!
//! Each fixture encodes the SECURE expected behaviour; a regression that
//! leaked another user's data, let injection through, or dropped a table would
//! flip the expected response and fail here.

use donat_conformance::{Suite, Transport};

const SEC: &str = "queries/security";

#[test]
fn security_access_control_and_injection() {
    let s = Suite::new("security").start();
    s.setup_v1q(&format!("{SEC}/setup.yaml"));

    // Read-path attacks (safe over http + websocket). No state mutation, so
    // these run before the inserts below.
    let reads = [
        // Horizontal privilege escalation / IDOR on reads.
        "idor_list_only_own.yaml",
        "idor_where_other_owner.yaml",
        "idor_or_filter_bypass.yaml",
        "idor_neq_filter_bypass.yaml",
        "idor_nin_filter_bypass.yaml",
        "idor_by_pk_other.yaml",
        "idor_aggregate_no_leak.yaml",
        "idor_via_relationship.yaml",
        // Column- and table-level access control.
        "column_hidden_secret_pin.yaml",
        "table_hidden_audit_log.yaml",
        // Permission needs a session var that is absent — must not fail open.
        "missing_session_var.yaml",
        // SQL injection via filters / session variables.
        "sqli_where_string_literal.yaml",
        "sqli_session_var.yaml",
        "sqli_like_injection.yaml",
        "sqli_stacked_in_list.yaml",
    ];
    for f in reads {
        s.check_query_f(&format!("{SEC}/{f}"), Transport::Both);
    }

    // Mutation-path attacks (http). The first three change nothing (they match
    // no permitted row); the rest insert under the caller's own owner.
    let mutations = [
        "idor_update_other.yaml",
        "idor_update_by_pk_other.yaml",
        "idor_delete_other.yaml",
        "forge_owner_column_rejected.yaml",
        "insert_memo_other_account_denied.yaml",
        "insert_preset_forces_owner.yaml",
        "sqli_insert_value_literal.yaml",
        "sqli_quotes_and_backslash.yaml",
    ];
    for f in mutations {
        s.check_query_f(&format!("{SEC}/{f}"), Transport::Http);
    }

    s.teardown_v1q(&format!("{SEC}/teardown.yaml"));
}

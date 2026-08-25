//! What counts as bounding rows to the caller, and what only looks like it.
//!
//! The analysis decides whether reaching TRUE requires a fact about who is
//! asking. Getting `_or` wrong is the interesting failure: an expression can
//! name a session variable and still admit every row, and a check that merely
//! grepped for `x-donat-` would wave it through.

use donat_metadata::{UnboundedPolicy, UnboundedReason, binds_caller, validate_permission_bounds};
use serde_json::json;

fn bounds(expr: serde_json::Value) -> bool {
    binds_caller(&expr, None)
}

#[test]
fn an_empty_filter_bounds_nobody() {
    assert!(!bounds(json!({})));
}

#[test]
fn a_comparison_against_the_session_bounds_the_caller() {
    assert!(bounds(json!({"owner_id": {"_eq": "X-Donat-User-Id"}})));
}

#[test]
fn a_comparison_against_a_literal_does_not() {
    assert!(!bounds(json!({"status": {"_eq": "paid"}})));
}

#[test]
fn a_session_value_inside_a_list_operand_still_bounds() {
    assert!(bounds(json!({"org_id": {"_in": ["X-Donat-Org-Id"]}})));
}

#[test]
fn a_relationship_traversal_answers_with_its_inner_expression() {
    assert!(bounds(json!({
        "memberships": {"user_id": {"_eq": "X-Donat-User-Id"}}
    })));
    assert!(!bounds(json!({
        "memberships": {"role": {"_eq": "owner"}}
    })));
}

#[test]
fn an_and_needs_only_one_bound_arm() {
    assert!(bounds(json!({"_and": [
        {"archived": {"_eq": false}},
        {"owner_id": {"_eq": "X-Donat-User-Id"}}
    ]})));
}

#[test]
fn several_keys_in_one_object_are_an_and() {
    assert!(bounds(json!({
        "archived": {"_eq": false},
        "owner_id": {"_eq": "X-Donat-User-Id"}
    })));
}

#[test]
fn an_or_with_one_unbounded_arm_bounds_nobody() {
    // The case the whole analysis exists for: a session variable is named and
    // every row is admitted anyway.
    assert!(!bounds(json!({"_or": [
        {"owner_id": {"_eq": "X-Donat-User-Id"}},
        {}
    ]})));
}

#[test]
fn an_or_bounds_when_every_arm_does() {
    assert!(bounds(json!({"_or": [
        {"owner_id": {"_eq": "X-Donat-User-Id"}},
        {"delegate_id": {"_eq": "X-Donat-User-Id"}}
    ]})));
}

#[test]
fn an_empty_or_bounds_nobody() {
    assert!(!bounds(json!({"_or": []})));
}

#[test]
fn a_negated_bound_is_not_a_bound() {
    assert!(!bounds(
        json!({"_not": {"owner_id": {"_eq": "X-Donat-User-Id"}}})
    ));
}

#[test]
fn legacy_dollar_spellings_are_read_the_same_way() {
    assert!(!bounds(json!({"$or": [
        {"owner_id": {"_eq": "X-Donat-User-Id"}},
        {}
    ]})));
    assert!(bounds(json!({"$and": [
        {"owner_id": {"_eq": "X-Donat-User-Id"}}
    ]})));
}

#[test]
fn an_exists_answers_with_its_where() {
    assert!(bounds(json!({"_exists": {
        "_table": {"schema": "public", "name": "membership"},
        "_where": {"user_id": {"_eq": "X-Donat-User-Id"}}
    }})));
    assert!(!bounds(json!({"_exists": {
        "_table": {"schema": "public", "name": "membership"},
        "_where": {"active": {"_eq": true}}
    }})));
}

#[test]
fn the_tenant_variable_is_not_a_caller_bound() {
    // Scoping by tenant admits every row of that tenant — every seller's order
    // in one marketplace. That is isolation, not ownership, and calling it a
    // caller bound is exactly the confusion this check exists to surface.
    let expr = json!({"tenant_id": {"_eq": "X-Donat-Tenant-Id"}});
    assert!(binds_caller(&expr, None));
    assert!(!binds_caller(&expr, Some("X-Donat-Tenant-Id")));
}

#[test]
fn the_tenant_variable_does_not_hide_a_real_bound_beside_it() {
    let expr = json!({"_and": [
        {"tenant_id": {"_eq": "X-Donat-Tenant-Id"}},
        {"owner_id": {"_eq": "X-Donat-User-Id"}}
    ]});
    assert!(binds_caller(&expr, Some("X-Donat-Tenant-Id")));
}

// --- the deploy-time rules -------------------------------------------------

fn metadata_with(filter: serde_json::Value, unbounded: Option<&str>, policy: &str) -> String {
    let declaration = unbounded
        .map(|reason| format!("\n              unbounded: {reason}"))
        .unwrap_or_default();
    format!(
        r#"
version: 3
permissions:
  unbounded_permissions: {policy}
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url:
          from_env: DONAT_GRAPHQL_DATABASE_URL
    tables:
      - table: {{schema: public, name: order}}
        select_permissions:
          - role: staff
            permission:
              columns: "*"
              filter: {filter}{declaration}
"#
    )
}

fn errors(yaml: &str) -> Vec<String> {
    let metadata: donat_metadata::Metadata = serde_yaml::from_str(yaml).expect("metadata parses");
    validate_permission_bounds(&metadata)
        .into_iter()
        .map(|error| error.to_string())
        .collect()
}

#[test]
fn an_unbounded_permission_with_no_reason_is_refused_where_the_check_is_on() {
    let found = errors(&metadata_with(json!({}), None, "declared"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("public.order.select_permissions[staff]"),
        "{found:?}"
    );
    assert!(found[0].contains("does not bound"), "{found:?}");
}

#[test]
fn the_same_permission_loads_where_the_deployment_never_asked() {
    assert!(errors(&metadata_with(json!({}), None, "unchecked")).is_empty());
}

#[test]
fn a_declared_reason_satisfies_the_check() {
    assert!(errors(&metadata_with(json!({}), Some("operator"), "declared")).is_empty());
}

#[test]
fn a_reason_on_a_permission_that_does_bound_is_refused_even_unchecked() {
    // A stale declaration is worse than none: it tells a reviewer a bound was
    // considered and declined where one is in fact present.
    let bounded = json!({"owner_id": {"_eq": "X-Donat-User-Id"}});
    let found = errors(&metadata_with(bounded, Some("catalogue"), "unchecked"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("the declaration is false"), "{found:?}");
}

#[test]
fn a_static_filter_is_still_unbounded() {
    // Non-empty is not the same as bounded: every caller of the role sees the
    // same rows, which is what a reviewer needs told.
    let found = errors(&metadata_with(
        json!({"status": {"_eq": "paid"}}),
        None,
        "declared",
    ));
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn the_policy_defaults_to_unchecked_so_v2_metadata_keeps_loading() {
    let yaml = r#"
version: 3
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url:
          from_env: DONAT_GRAPHQL_DATABASE_URL
    tables:
      - table: {schema: public, name: order}
        select_permissions:
          - role: staff
            permission: {columns: "*", filter: {}}
"#;
    let metadata: donat_metadata::Metadata = serde_yaml::from_str(yaml).expect("parses");
    assert_eq!(
        metadata.permissions.unbounded_permissions,
        UnboundedPolicy::Unchecked
    );
    assert!(validate_permission_bounds(&metadata).is_empty());
}

#[test]
fn every_reason_spells_itself_the_way_it_is_written() {
    assert_eq!(UnboundedReason::Catalogue.as_str(), "catalogue");
    assert_eq!(UnboundedReason::Operator.as_str(), "operator");
    assert_eq!(UnboundedReason::Worker.as_str(), "worker");
}

// --- `command` is checked, not believed -----------------------------------

fn command_metadata(ordinary_select: bool, plane: &str, unbounded: &str) -> String {
    let select = if ordinary_select {
        r#"
        select_permissions:
          - role: customer
            permission: {columns: "*", filter: {}, unbounded: catalogue}"#
    } else {
        ""
    };
    format!(
        r#"
version: 3
permissions:
  unbounded_permissions: declared
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url:
          from_env: DONAT_GRAPHQL_DATABASE_URL
    tables:
      - table: {{schema: public, name: booking_event}}{select}
        {plane}:
          - role: customer
            permission: {{columns: "*", check: {{}}, filter: {{}}, unbounded: {unbounded}}}
"#
    )
}

#[test]
fn command_is_accepted_on_a_command_plane() {
    assert!(
        errors(&command_metadata(
            true,
            "command_insert_permissions",
            "command"
        ))
        .is_empty(),
        "a command plane is exactly where the claim holds"
    );
}

#[test]
fn command_is_accepted_on_an_ordinary_plane_no_generic_root_reaches() {
    // No ordinary select for the role means the table is absent from that
    // role's schema entirely, so it has no insert, update or delete root
    // either — only a command step can arrive.
    assert!(errors(&command_metadata(false, "insert_permissions", "command")).is_empty());
}

#[test]
fn command_is_refused_where_a_generic_root_does_reach() {
    let found = errors(&command_metadata(true, "insert_permissions", "command"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("generic roots reach"), "{found:?}");
    assert!(found[0].contains("customer"), "{found:?}");
}

#[test]
fn an_inherited_select_also_counts_as_a_generic_root() {
    // A role inheriting from one that holds the select sees the table too, so
    // the claim is just as false there.
    let yaml = r#"
version: 3
permissions:
  unbounded_permissions: declared
inherited_roles:
  - role_name: vip_customer
    role_set: [customer]
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url:
          from_env: DONAT_GRAPHQL_DATABASE_URL
    tables:
      - table: {schema: public, name: booking_event}
        select_permissions:
          - role: customer
            permission: {columns: "*", filter: {}, unbounded: catalogue}
        insert_permissions:
          - role: vip_customer
            permission: {columns: "*", check: {}, unbounded: command}
"#;
    let found = errors(yaml);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("generic roots reach"), "{found:?}");
}

#[test]
fn an_inherited_select_counts_however_many_hops_away_it_is() {
    // The runtime walks the whole chain, so `A -> B -> C` gives `A` the table
    // when only `C` declares the select — and with it an insert root. A check
    // that stopped at one hop answered "no generic root" for a role that has
    // one, and accepted a `command` claim nothing in a command bounds.
    let yaml = r#"
version: 3
permissions:
  unbounded_permissions: declared
inherited_roles:
  - role_name: vip_customer
    role_set: [member]
  - role_name: member
    role_set: [customer]
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url:
          from_env: DONAT_GRAPHQL_DATABASE_URL
    tables:
      - table: {schema: public, name: booking_event}
        select_permissions:
          - role: customer
            permission: {columns: "*", filter: {}, unbounded: catalogue}
        insert_permissions:
          - role: vip_customer
            permission: {columns: "*", check: {}, unbounded: command}
"#;
    let found = errors(yaml);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("generic roots reach"), "{found:?}");
    assert!(found[0].contains("vip_customer"), "{found:?}");
}

#[test]
fn a_cycle_in_inherited_roles_does_not_hang_the_check() {
    let yaml = r#"
version: 3
permissions:
  unbounded_permissions: declared
inherited_roles:
  - role_name: a
    role_set: [b]
  - role_name: b
    role_set: [a]
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url:
          from_env: DONAT_GRAPHQL_DATABASE_URL
    tables:
      - table: {schema: public, name: booking_event}
        insert_permissions:
          - role: a
            permission: {columns: "*", check: {}, unbounded: command}
"#;
    // Nobody declares the select, so no generic root reaches it and the claim
    // stands; the point of the case is that the walk terminates.
    assert!(errors(yaml).is_empty());
}

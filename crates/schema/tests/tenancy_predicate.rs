//! Tenant isolation as the planner produces it, without a database.
//!
//! Every case here is a way a tenanted deployment could leak if the predicate
//! were a filter somebody wrote rather than a layer the compiler adds: a role
//! whose declared filter is `{}`, a nested relationship, an insert naming
//! another tenant's id, an update trying to move a row, a delete with a
//! caller-supplied `where`.

use std::collections::{BTreeMap, HashMap};

use donat_catalog_types::{Catalog, ColumnInfo, ForeignKey, RelationKind, TableInfo};
use donat_ir::{BoolExp, MutationRoot, RootField, SetOp};
use donat_metadata::Metadata;
use donat_schema::{Plan, PlanError, Planner, Session};
use serde_json::{Value as Json, json};

fn col(name: &str, pg_type: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        pg_type: pg_type.to_string(),
        pg_typmod: -1,
        native_type: None,
        nullable: false,
        has_default: false,
    }
}

fn catalog() -> Catalog {
    let mut tables = BTreeMap::new();
    tables.insert(
        "public.tenant".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "tenant".into(),
            relation_kind: RelationKind::Table,
            columns: vec![
                col("id", "uuid"),
                col("status", "text"),
                col("name", "text"),
            ],
            primary_key: vec!["id".into()],
            unique_keys: vec![],
            foreign_keys: vec![],
        },
    );
    tables.insert(
        "public.product".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "product".into(),
            relation_kind: RelationKind::Table,
            columns: vec![
                col("id", "uuid"),
                col("tenant_id", "uuid"),
                col("name", "text"),
                col("archived", "bool"),
            ],
            primary_key: vec!["id".into()],
            unique_keys: vec![],
            foreign_keys: vec![],
        },
    );
    tables.insert(
        "public.review".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "review".into(),
            relation_kind: RelationKind::Table,
            columns: vec![
                col("id", "uuid"),
                col("tenant_id", "uuid"),
                col("product_id", "uuid"),
                col("body", "text"),
            ],
            primary_key: vec!["id".into()],
            unique_keys: vec![],
            foreign_keys: vec![ForeignKey {
                constraint_name: "review_product_id_fkey".into(),
                column_mapping: BTreeMap::from([("product_id".into(), "id".into())]),
                referenced_schema: "public".into(),
                referenced_table: "product".into(),
            }],
        },
    );
    tables.insert(
        "public.plan".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "plan".into(),
            relation_kind: RelationKind::Table,
            columns: vec![col("code", "text"), col("max_products", "int4")],
            primary_key: vec!["code".into()],
            unique_keys: vec![],
            foreign_keys: vec![],
        },
    );
    Catalog {
        tables,
        functions: BTreeMap::new(),
    }
}

/// `staff` may do everything to its own tenant's rows, and its declared
/// filters are deliberately unrestricted: that is the ordinary shape, and the
/// one the tenant layer has to survive.
fn metadata(product_filter: Json) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {},
            "tables": [
                {
                    "table": { "schema": "public", "name": "tenant" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "product" },
                    "array_relationships": [{
                        "name": "reviews",
                        "using": { "foreign_key_constraint_on": {
                            "table": { "schema": "public", "name": "review" },
                            "column": "product_id"
                        }}
                    }],
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": product_filter } }
                    ],
                    "insert_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "check": {} } }
                    ],
                    "update_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ],
                    "delete_permissions": [
                        { "role": "staff", "permission": { "filter": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "review" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "plan" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ]
                }
            ]
        }],
        "tenancy": {
            "source": "default",
            "variable": "X-Donat-Tenant-Id",
            "key": "tenant_id",
            "registry": {
                "table": { "schema": "public", "name": "tenant" },
                "key": "id",
                "status": { "column": "status", "serving": ["active"] }
            },
            "keys": [{ "table": { "schema": "public", "name": "tenant" }, "key": "id" }],
            "exempt": [{ "table": { "schema": "public", "name": "plan" }, "shared": "read_only" }]
        }
    }))
    .expect("metadata deserializes")
}

fn session(vars: &[(&str, &str)]) -> Session {
    Session {
        role: "staff".to_string(),
        vars: vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
        backend_request: false,
    }
}

fn staff() -> Session {
    session(&[("x-donat-tenant-id", "11111111-1111-1111-1111-111111111111")])
}

fn plan_with(product_filter: Json, query: &str, sess: &Session) -> Result<Plan, PlanError> {
    let md = metadata(product_filter);
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("query parses")
        .into_static();
    planner.plan(&doc, None, &Default::default(), sess)
}

fn plan(query: &str, sess: &Session) -> Result<Plan, PlanError> {
    plan_with(json!({}), query, sess)
}

fn debug(plan: &Plan) -> String {
    format!("{plan:?}")
}

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const OTHER: &str = "22222222-2222-2222-2222-222222222222";

/// The predicate on the first select root.
fn root_predicate(plan: &Plan) -> Option<BoolExp> {
    match plan {
        Plan::Query(roots) => match roots.first().expect("one root") {
            RootField::Select { query, .. } => query.predicate.clone(),
            other => panic!("expected a select root, got {other:?}"),
        },
        other => panic!("expected a query plan, got {other:?}"),
    }
}

/// The ordinary case, and the one a hand-rolled tenancy gets wrong: a role
/// whose row filter is `{}` means "every row of my tenant", not "every row".
#[test]
fn an_unrestricted_filter_is_still_bounded_by_the_tenant() {
    let planned = plan("{ product { id name } }", &staff()).expect("planning succeeds");
    let predicate = root_predicate(&planned).expect("a tenanted table is never unrestricted");
    let rendered = format!("{predicate:?}");
    assert!(rendered.contains("tenant_id"), "{rendered}");
    assert!(rendered.contains(TENANT), "{rendered}");
}

#[test]
fn a_declared_filter_is_kept_and_the_tenant_is_added_to_it() {
    let planned = plan_with(
        json!({ "archived": { "_eq": false } }),
        "{ product { id } }",
        &staff(),
    )
    .expect("planning succeeds");
    let rendered = format!("{:?}", root_predicate(&planned).expect("a predicate"));
    assert!(rendered.contains("archived"), "{rendered}");
    assert!(rendered.contains("tenant_id"), "{rendered}");
    assert!(rendered.contains(TENANT), "{rendered}");
}

/// Platform reference data is exempt on purpose: every tenant reads the same
/// plans, and nothing may write them.
#[test]
fn shared_reference_data_is_not_scoped() {
    let planned = plan("{ plan { code } }", &staff()).expect("planning succeeds");
    assert!(root_predicate(&planned).is_none());
}

/// A nested selection resolves the *remote* table's permissions, so the child
/// rows are bounded by the child table's own tenant key rather than by the
/// parent's.
#[test]
fn a_nested_relationship_is_scoped_by_the_remote_tables_own_key() {
    let planned = plan("{ product { id reviews { body } } }", &staff()).expect("planning succeeds");
    let rendered = debug(&planned);
    // Once for the parent, once for the child.
    assert!(
        rendered.matches("tenant_id").count() >= 2,
        "the nested select is unscoped: {rendered}"
    );
}

/// No tenant, no answer. Serving an empty result instead would hide a
/// misconfigured token behind a screen that merely looks empty.
#[test]
fn a_request_with_no_tenant_is_refused_rather_than_answered_emptily() {
    let error = plan("{ product { id } }", &session(&[])).expect_err("planning must fail");
    assert_eq!(error.code, "access-denied");
    assert!(
        error.message.contains("x-donat-tenant-id"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("verified token"),
        "{}",
        error.message
    );
}

/// The client may name another tenant's id in the object it sends. The preset
/// replaces it, exactly as a preset on `user_id` replaces one today.
#[test]
fn an_insert_lands_in_the_callers_tenant_whatever_the_object_named() {
    let planned = plan(
        &format!(
            "mutation {{ insert_product(objects: [{{ name: \"a\", tenant_id: \"{OTHER}\" }}]) \
             {{ affected_rows }} }}"
        ),
        &staff(),
    )
    .expect("planning succeeds");
    let Plan::Mutation(roots) = &planned else {
        panic!("expected a mutation plan");
    };
    let MutationRoot::Insert { insert, .. } = roots.first().expect("one root") else {
        panic!("expected an insert root");
    };
    let index = insert
        .columns
        .iter()
        .position(|(name, _)| name == "tenant_id")
        .expect("the tenant column is written");
    let value = format!("{:?}", insert.rows[0][index]);
    assert!(value.contains(TENANT), "{value}");
    assert!(
        !value.contains(OTHER),
        "the caller's value survived: {value}"
    );
    // And the check still bounds the written row.
    let check = format!("{:?}", insert.check);
    assert!(
        check.contains("tenant_id") && check.contains(TENANT),
        "{check}"
    );
}

/// Two properties at once: the rows an update may touch are its own tenant's,
/// and the value it writes cannot move a row to another tenant.
#[test]
fn an_update_cannot_reach_or_create_another_tenants_row() {
    let planned = plan(
        "mutation { update_product(where: { name: { _eq: \"a\" } }, \
         _set: { name: \"b\" }) { affected_rows } }",
        &staff(),
    )
    .expect("planning succeeds");
    let Plan::Mutation(roots) = &planned else {
        panic!("expected a mutation plan");
    };
    let MutationRoot::Update { update, .. } = roots.first().expect("one root") else {
        panic!("expected an update root");
    };
    let predicate = format!("{:?}", update.predicate);
    assert!(
        predicate.contains("tenant_id") && predicate.contains(TENANT),
        "{predicate}"
    );
    assert!(
        update.sets.iter().any(|op| matches!(
            op,
            SetOp::Set { column, .. } if column == "tenant_id"
        )),
        "the tenant column is not pinned: {:?}",
        update.sets
    );
    let check = format!("{:?}", update.check);
    assert!(check.contains("tenant_id"), "{check}");
}

#[test]
fn a_delete_is_bounded_by_the_tenant_however_the_where_is_written() {
    let planned = plan(
        "mutation { delete_product(where: {}) { affected_rows } }",
        &staff(),
    )
    .expect("planning succeeds");
    let Plan::Mutation(roots) = &planned else {
        panic!("expected a mutation plan");
    };
    let MutationRoot::Delete { delete, .. } = roots.first().expect("one root") else {
        panic!("expected a delete root");
    };
    let predicate = format!("{:?}", delete.predicate);
    assert!(
        predicate.contains("tenant_id") && predicate.contains(TENANT),
        "{predicate}"
    );
}

/// The registry is keyed by its own identifier, not by a `tenant_id` column.
#[test]
fn the_registry_is_scoped_by_its_own_identifier() {
    let planned = plan("{ tenant { name } }", &staff()).expect("planning succeeds");
    let rendered = format!("{:?}", root_predicate(&planned).expect("a predicate"));
    assert!(rendered.contains("\"id\""), "{rendered}");
    assert!(rendered.contains(TENANT), "{rendered}");
    assert!(!rendered.contains("tenant_id"), "{rendered}");
}

/// An untenanted deployment must plan exactly as it does today.
#[test]
fn a_deployment_without_tenancy_is_untouched() {
    let mut md = metadata(json!({}));
    md.tenancy = None;
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>("{ product { id } }")
        .expect("query parses")
        .into_static();
    let planned = planner
        .plan(&doc, None, &Default::default(), &session(&[]))
        .expect("planning succeeds without any tenant");
    assert!(root_predicate(&planned).is_none());
}

/// A person who has just signed in belongs to some set of tenants and is in
/// none of them. Without this, a store switcher cannot exist — and the usual
/// workaround is a second role with a wider filter, which is exactly the
/// hand-rolled cross-tenant view this design exists to remove.
#[test]
fn a_declared_cross_tenant_read_is_bounded_by_the_caller_instead() {
    let mut md = metadata(json!({}));
    let tenancy = md.tenancy.as_mut().expect("tenancy");
    tenancy.cross_tenant_reads = vec![donat_metadata::CrossTenantRead {
        table: donat_metadata::QualifiedTable::Qualified {
            schema: "public".into(),
            name: "review".into(),
        },
        role: "staff".into(),
        scoped_by: donat_metadata::SubjectBinding {
            column: "body".into(),
            variable: "X-Donat-User-Id".into(),
        },
    }];
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>("{ review { id } }")
        .expect("query parses")
        .into_static();
    let sess = session(&[
        ("x-donat-tenant-id", TENANT),
        ("x-donat-user-id", "person-7"),
    ]);
    let planned = planner
        .plan(&doc, None, &Default::default(), &sess)
        .expect("planning succeeds");
    let rendered = format!("{:?}", root_predicate(&planned).expect("a predicate"));
    // Bounded by the caller, not by the tenant — but still bounded.
    assert!(rendered.contains("person-7"), "{rendered}");
    assert!(!rendered.contains("tenant_id"), "{rendered}");
}

/// The substitution is not a way out of being restricted: a caller with no
/// subject is refused exactly as one with no tenant is.
#[test]
fn a_cross_tenant_read_without_a_subject_is_still_refused() {
    let mut md = metadata(json!({}));
    let tenancy = md.tenancy.as_mut().expect("tenancy");
    tenancy.cross_tenant_reads = vec![donat_metadata::CrossTenantRead {
        table: donat_metadata::QualifiedTable::Qualified {
            schema: "public".into(),
            name: "review".into(),
        },
        role: "staff".into(),
        scoped_by: donat_metadata::SubjectBinding {
            column: "body".into(),
            variable: "X-Donat-User-Id".into(),
        },
    }];
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>("{ review { id } }")
        .expect("query parses")
        .into_static();
    let error = planner
        .plan(&doc, None, &Default::default(), &session(&[]))
        .expect_err("planning must fail");
    assert_eq!(error.code, "access-denied");
}

/// How many times the registry's status gate appears in one plan.
///
/// It is row-independent and identical everywhere, so a query over a parent
/// and its children repeats it once per table. Postgres evaluates an
/// uncorrelated `EXISTS` once and caches it as an InitPlan, but the *plan* is
/// still N copies — which is what `evaluation: request_cte` in the design was
/// about. This test exists to make the count visible rather than to assert a
/// particular number is fine.
#[test]
fn the_serving_gate_is_repeated_once_per_table_in_a_nested_read() {
    let planned = plan("{ product { id reviews { body } } }", &staff()).expect("planning succeeds");
    let rendered = format!("{planned:?}");
    let gates = rendered.matches("Exists").count();
    // Parent and child: two tables, two gates.
    assert_eq!(gates, 2, "unexpected gate count: {rendered}");
}

// --- the v1 planner is a write surface too -------------------------------
//
// It has no HTTP route today, which is exactly why it drifted: the tenant
// reached every GraphQL write and none of these. A planner that is one mount
// away from serving is not a planner the bound may skip.

fn v1_planner<'a>(md: &'a Metadata, cat: &'a Catalog) -> Planner<'a> {
    Planner::new(md, cat)
}

#[test]
fn a_v1_delete_is_bounded_by_the_tenant() {
    let md = metadata(json!({}));
    let cat = catalog();
    let plan = v1_planner(&md, &cat)
        .plan_v1_delete(
            &json!({"table": {"schema": "public", "name": "product"}, "where": {}}),
            &staff(),
        )
        .expect("planning succeeds");
    let rendered = format!("{plan:?}");
    assert!(
        rendered.contains("tenant_id"),
        "a v1 delete with an empty where reached every tenant: {rendered}"
    );
}

#[test]
fn a_v1_update_is_bounded_by_the_tenant() {
    let md = metadata(json!({}));
    let cat = catalog();
    let plan = v1_planner(&md, &cat)
        .plan_v1_update(
            &json!({
                "table": {"schema": "public", "name": "product"},
                "where": {},
                "$set": {"name": "renamed"}
            }),
            &staff(),
        )
        .expect("planning succeeds");
    let rendered = format!("{plan:?}");
    assert!(
        rendered.contains("tenant_id"),
        "a v1 update with an empty where rewrote every tenant: {rendered}"
    );
}

#[test]
fn a_v1_insert_presets_the_tenant_over_the_callers_value() {
    let md = metadata(json!({}));
    let cat = catalog();
    let plan = v1_planner(&md, &cat)
        .plan_v1_insert(
            &json!({
                "table": {"schema": "public", "name": "product"},
                "objects": [{
                    "name": "smuggled",
                    "tenant_id": "22222222-2222-2222-2222-222222222222"
                }]
            }),
            &staff(),
        )
        .expect("planning succeeds");
    let rendered = format!("{plan:?}");
    assert!(
        rendered.contains("11111111-1111-1111-1111-111111111111"),
        "the tenant preset never reached the v1 insert: {rendered}"
    );
    assert!(
        !rendered.contains("22222222-2222-2222-2222-222222222222"),
        "a v1 insert stored the tenant the caller named: {rendered}"
    );
}

#[test]
fn a_v1_upsert_cannot_take_another_tenants_row_on_conflict() {
    // The `DO UPDATE` branch changes an *existing* row. Without the bound, a
    // caller colliding on a key that does not include the tenant overwrites
    // somebody else's row — and with the tenant among the re-applied columns,
    // `tenant_id = EXCLUDED.tenant_id` moves it into the caller's tenant on the
    // way. Both halves are the same sentence: an upsert is an update.
    let md = metadata(json!({}));
    let cat = catalog();
    let plan = v1_planner(&md, &cat)
        .plan_v1_insert(
            &json!({
                "table": {"schema": "public", "name": "product"},
                "objects": [{"name": "collided"}],
                "on_conflict": {"constraint": "product_pkey", "action": "update"}
            }),
            &staff(),
        )
        .expect("planning succeeds");
    let rendered = format!("{plan:?}");

    let on_conflict = rendered
        .split_once("on_conflict")
        .map(|(_, tail)| tail.to_string())
        .expect("the plan carries an on_conflict");
    assert!(
        on_conflict.contains("tenant_id"),
        "the DO UPDATE branch reached every tenant's rows: {rendered}"
    );
    assert!(
        !on_conflict.contains("update_columns: [\"tenant_id\"")
            && !on_conflict.contains("\"tenant_id\"]"),
        "the tenant was re-applied from EXCLUDED, which moves the row: {rendered}"
    );
}

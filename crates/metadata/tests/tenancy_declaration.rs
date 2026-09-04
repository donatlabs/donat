//! The `tenancy.yaml` declaration and every refusal it earns before a database
//! is opened. No database needed: these are the rules that can be decided from
//! metadata alone, and they are the ones that make forgetting a table
//! impossible rather than merely unlikely.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use donat_metadata::{
    LoadError, Metadata, QualifiedTable, TableScope, TenancyBinding, TenancyTrust,
    UnscopedStepPolicy, load_metadata_dir,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "donat_metadata_tenancy_{tag}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

const TENANT_TABLE: &str = "\
table: { schema: public, name: tenant }
select_permissions:
  - role: staff
    permission:
      columns: \"*\"
      filter: {}
";

const PRODUCT_TABLE: &str = "\
table: { schema: public, name: product }
select_permissions:
  - role: staff
    permission:
      columns: \"*\"
      filter: {}
insert_permissions:
  - role: staff
    permission:
      columns: \"*\"
      check: {}
";

const PLAN_TABLE: &str = "\
table: { schema: public, name: plan }
select_permissions:
  - role: staff
    permission:
      columns: \"*\"
      filter: {}
";

const PLATFORM_USER_TABLE: &str = "\
table: { schema: public, name: platform_user }
array_relationships:
  - name: memberships
    using:
      foreign_key_constraint_on:
        table: { schema: public, name: iam_membership }
        column: user_id
select_permissions:
  - role: staff
    permission:
      columns: \"*\"
      filter: {}
";

const IAM_MEMBERSHIP_TABLE: &str = "\
table: { schema: public, name: iam_membership }
select_permissions:
  - role: staff
    permission:
      columns: \"*\"
      filter: {}
";

const TENANCY_YAML: &str = "\
source: default
binding: row_key
variable: X-Donat-Tenant-Id
trust: jwt_claim
key: tenant_id
registry:
  table: { schema: public, name: tenant }
  key: id
  status:
    column: status
    serving: [active]
keys:
  - { table: { schema: public, name: tenant }, key: id }
exempt:
  - table: { schema: public, name: plan }
    shared: read_only
  - table: { schema: public, name: platform_user }
    scope_via: memberships
unscoped_steps: audited
";

/// A metadata directory with the five tables above and an optional
/// `tenancy.yaml`. `tables` lets a case drop or replace one of them.
fn build(tag: &str, kind: &str, tables: &[(&str, &str)], tenancy: Option<&str>) -> PathBuf {
    let dir = tempdir(tag);
    write(&dir, "version.yaml", "version: 3\n");
    write(
        &dir,
        "databases/databases.yaml",
        &format!(
            "\
- name: default
  kind: {kind}
  configuration:
    connection_info:
      database_url:
        from_env: DONAT_GRAPHQL_DATABASE_URL
  tables: \"!include default/tables/tables.yaml\"
"
        ),
    );
    let index = tables
        .iter()
        .map(|(file, _)| format!("- \"!include {file}.yaml\"\n"))
        .collect::<String>();
    write(&dir, "databases/default/tables/tables.yaml", &index);
    for (file, body) in tables {
        write(&dir, &format!("databases/default/tables/{file}.yaml"), body);
    }
    if let Some(tenancy) = tenancy {
        write(&dir, "tenancy.yaml", tenancy);
    }
    dir
}

fn all_tables() -> Vec<(&'static str, &'static str)> {
    vec![
        ("public_tenant", TENANT_TABLE),
        ("public_product", PRODUCT_TABLE),
        ("public_plan", PLAN_TABLE),
        ("public_platform_user", PLATFORM_USER_TABLE),
        ("public_iam_membership", IAM_MEMBERSHIP_TABLE),
    ]
}

fn tenancy_error(result: Result<Metadata, LoadError>) -> String {
    match result {
        Err(LoadError::Tenancy { message, .. }) => message,
        Err(other) => panic!("expected a tenancy error, got {other}"),
        Ok(_) => panic!("expected a tenancy error, but the metadata loaded"),
    }
}

fn table(schema: &str, name: &str) -> QualifiedTable {
    QualifiedTable::Qualified {
        schema: schema.to_string(),
        name: name.to_string(),
    }
}

#[test]
fn a_deployment_without_the_file_is_exactly_what_it_is_today() {
    let dir = build("absent", "postgres", &all_tables(), None);
    let metadata = load_metadata_dir(&dir).unwrap();
    assert!(metadata.tenancy.is_none());
}

#[test]
fn the_declaration_loads_and_answers_how_each_table_is_scoped() {
    let dir = build("ok", "postgres", &all_tables(), Some(TENANCY_YAML));
    let metadata = load_metadata_dir(&dir).unwrap();
    let tenancy = metadata.tenancy.expect("tenancy.yaml loaded");

    assert_eq!(tenancy.source, "default");
    assert_eq!(tenancy.binding, TenancyBinding::RowKey);
    assert_eq!(tenancy.trust, TenancyTrust::JwtClaim);
    assert_eq!(tenancy.variable_key(), "x-donat-tenant-id");
    assert_eq!(tenancy.unscoped_steps, UnscopedStepPolicy::Audited);

    // A table nobody mentioned carries the default key. That is the rule that
    // makes tracking a table enough to scope it.
    assert_eq!(
        tenancy.table_scope(&table("public", "product")),
        TableScope::Key("tenant_id")
    );
    // The registry's own tenant is its identifier.
    assert_eq!(
        tenancy.table_scope(&table("public", "tenant")),
        TableScope::Key("id")
    );
    assert_eq!(
        tenancy.table_scope(&table("public", "plan")),
        TableScope::Shared
    );
    assert_eq!(
        tenancy.table_scope(&table("public", "platform_user")),
        TableScope::ScopeVia("memberships")
    );
}

/// An unqualified `table: product` and `{schema: public, name: product}` are
/// the same table, and a scope lookup that missed that would silently fall
/// back to the default key.
#[test]
fn a_table_is_matched_however_its_name_is_spelled() {
    let dir = build("spelling", "postgres", &all_tables(), Some(TENANCY_YAML));
    let tenancy = load_metadata_dir(&dir).unwrap().tenancy.unwrap();
    assert_eq!(
        tenancy.table_scope(&QualifiedTable::Name("plan".to_string())),
        TableScope::Shared
    );
    assert_eq!(
        tenancy.table_scope(&QualifiedTable::Name("public.plan".to_string())),
        TableScope::Shared
    );
}

#[test]
fn the_step_escape_hatch_is_closed_unless_it_is_opened() {
    let yaml = TENANCY_YAML.replace("unscoped_steps: audited\n", "");
    let dir = build("unscoped_default", "postgres", &all_tables(), Some(&yaml));
    let tenancy = load_metadata_dir(&dir).unwrap().tenancy.unwrap();
    assert_eq!(tenancy.unscoped_steps, UnscopedStepPolicy::Forbidden);
}

#[test]
fn a_binding_this_build_cannot_keep_is_refused_by_name() {
    let yaml = TENANCY_YAML.replace("binding: row_key", "binding: schema");
    let dir = build("binding", "postgres", &all_tables(), Some(&yaml));
    match load_metadata_dir(&dir) {
        Err(LoadError::Yaml { .. }) => {}
        other => panic!("expected the unknown binding to be refused, got {other:?}"),
    }
}

#[test]
fn a_tenant_may_not_be_asserted_by_something_other_than_a_session_variable() {
    let yaml = TENANCY_YAML.replace("variable: X-Donat-Tenant-Id", "variable: tenant");
    let dir = build("variable", "postgres", &all_tables(), Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("tenancy.variable") && message.contains("not a session variable"),
        "unexpected message: {message}"
    );
}

#[test]
fn an_exemption_must_say_why_it_is_one() {
    let yaml = TENANCY_YAML.replace("    shared: read_only\n", "");
    let dir = build("bare_exempt", "postgres", &all_tables(), Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("is exempt but says nothing about why"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_shared_table_a_tenant_can_write_is_refused() {
    let plan_with_writer = format!(
        "{PLAN_TABLE}\
insert_permissions:
  - role: staff
    permission:
      columns: \"*\"
      check: {{}}
"
    );
    let mut tables = all_tables();
    tables[2] = ("public_plan", Box::leak(plan_with_writer.into_boxed_str()));
    let dir = build("shared_writer", "postgres", &tables, Some(TENANCY_YAML));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("shared read-only")
            && message.contains("role `staff` holds a insert permission"),
        "unexpected message: {message}"
    );
}

#[test]
fn exempting_the_registry_would_publish_every_tenant_to_every_tenant() {
    let yaml = TENANCY_YAML
        .replace(
            "keys:\n  - { table: { schema: public, name: tenant }, key: id }\n",
            "",
        )
        .replace(
            "exempt:\n",
            "exempt:\n  - table: { schema: public, name: tenant }\n    shared: read_only\n",
        );
    let dir = build("exempt_registry", "postgres", &all_tables(), Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("would let a member of any tenant read every tenant"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_scope_via_that_names_no_relationship_is_refused() {
    let yaml = TENANCY_YAML.replace("scope_via: memberships", "scope_via: stores");
    let dir = build("scope_via", "postgres", &all_tables(), Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("declares no array relationship named `stores`"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_declaration_about_a_table_nothing_tracks_is_refused() {
    let yaml = TENANCY_YAML.replace(
        "  - table: { schema: public, name: plan }\n    shared: read_only\n",
        "  - table: { schema: public, name: invoice }\n    shared: read_only\n",
    );
    let dir = build("untracked", "postgres", &all_tables(), Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("public.invoice") && message.contains("is not tracked"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_table_is_keyed_or_exempt_and_never_both() {
    let yaml = TENANCY_YAML.replace(
        "exempt:\n",
        "exempt:\n  - table: { schema: public, name: tenant }\n    shared: read_only\n",
    );
    let dir = build("both", "postgres", &all_tables(), Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("is declared twice"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_registry_serving_nothing_would_refuse_every_request() {
    let yaml = TENANCY_YAML.replace("serving: [active]", "serving: []");
    let dir = build("serving", "postgres", &all_tables(), Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("no status is served"),
        "unexpected message: {message}"
    );
}

#[test]
fn tenancy_is_refused_on_a_source_no_suite_proves() {
    let dir = build("sqlite", "sqlite", &all_tables(), Some(TENANCY_YAML));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("tenancy is supported on Postgres sources only"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_tenanted_source_that_is_not_declared_is_refused() {
    let yaml = TENANCY_YAML.replace("source: default", "source: warehouse");
    let dir = build("no_source", "postgres", &all_tables(), Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("source `warehouse` is not declared"),
        "unexpected message: {message}"
    );
}

/// A row belonging to several tenants has no single tenant a write could be
/// bounded by, so the ordinary write path must not be offered for one at all.
#[test]
fn a_row_belonging_to_several_tenants_may_not_be_written_through_ordinary_crud() {
    let user_with_writer = format!(
        "{PLATFORM_USER_TABLE}\
update_permissions:
  - role: staff
    permission:
      columns: \"*\"
      filter: {{}}
"
    );
    let mut tables = all_tables();
    tables[3] = (
        "public_platform_user",
        Box::leak(user_with_writer.into_boxed_str()),
    );
    let dir = build("scope_via_writer", "postgres", &tables, Some(TENANCY_YAML));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("scoped through a relationship")
            && message.contains("write it from a command instead"),
        "unexpected message: {message}"
    );
}

// ------------------------------------------- the two declared escapes
//
// A command may step outside the caller's tenant in exactly two ways, and each
// is refused unless it is spelled out completely. These rules exist so that a
// reviewer can find every such command by reading one file.

const REGISTER: &str = "\
- name: register_merchant
  source: default
  tenant:
    establishes: { step: store, column: id }
  permissions:
    - role: founder
  steps:
    - name: store
      insert:
        table: { schema: public, name: tenant }
        object: { status: { literal: active } }
        returning: [id]
";

/// A metadata directory with the standard tables plus a `commands.yaml`.
fn build_with_commands(tag: &str, commands: &str, tenancy: Option<&str>) -> PathBuf {
    let dir = build(tag, "postgres", &all_tables(), tenancy);
    write(&dir, "commands.yaml", commands);
    dir
}

#[test]
fn a_command_may_declare_where_its_tenant_comes_from() {
    let dir = build_with_commands("establishes", REGISTER, Some(TENANCY_YAML));
    let metadata = load_metadata_dir(&dir).unwrap();
    let command = &metadata.commands[0];
    assert!(command.tenant.as_ref().expect("tenant").establishes());
    assert_eq!(command.tenant.as_ref().unwrap().step().step, "store");
}

#[test]
fn a_command_whose_tenant_step_does_not_exist_is_refused() {
    let commands = REGISTER.replace("step: store", "step: nowhere");
    let dir = build_with_commands("missing_step", &commands, Some(TENANCY_YAML));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("takes its tenant from step `nowhere`, which it does not declare"),
        "unexpected message: {message}"
    );
}

/// `establishes` must point at the write that creates the tenant. Pointing it
/// at a read would compile and scope nothing.
#[test]
fn establishes_must_name_the_step_that_creates_the_tenant() {
    let commands = REGISTER.replace(
        "      insert:\n        table: { schema: public, name: tenant }\n        object: { status: { literal: active } }\n        returning: [id]\n",
        "      select_one:\n        table: { schema: public, name: tenant }\n        by: { id: { arg: id } }\n        returning: [id]\n",
    );
    let dir = build_with_commands("wrong_shape", &commands, Some(TENANCY_YAML));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("which is not an insert step"),
        "unexpected message: {message}"
    );
}

const ACCEPT: &str = "\
- name: accept_invite
  source: default
  tenant:
    from: { step: invite, column: tenant_id }
  permissions:
    - role: joiner
  steps:
    - name: invite
      tenant: unscoped
      select_one:
        table: { schema: public, name: iam_membership }
        by: { user_id: { arg: token } }
        returning: [tenant_id]
";

#[test]
fn an_unscoped_read_is_allowed_only_where_the_deployment_opened_it() {
    let yaml = TENANCY_YAML.replace("unscoped_steps: audited\n", "");
    let dir = build_with_commands("unscoped_closed", ACCEPT, Some(&yaml));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("has not opened that escape"),
        "unexpected message: {message}"
    );
}

/// Reading one tenant's row and then writing with the caller's tenant would be
/// worse than either, so the command must be scoped by what it read.
#[test]
fn an_unscoped_read_must_scope_the_rest_of_the_command() {
    let commands = ACCEPT.replace(
        "  tenant:\n    from: { step: invite, column: tenant_id }\n",
        "",
    );
    let dir = build_with_commands("unscoped_unbound", &commands, Some(TENANCY_YAML));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("does not declare `tenant: { from: invite }`"),
        "unexpected message: {message}"
    );
}

/// A declaration in a deployment with no tenancy is not harmless — it reads as
/// a guarantee nothing is enforcing.
#[test]
fn a_tenant_declaration_without_tenancy_is_refused() {
    let dir = build_with_commands("no_tenancy", REGISTER, None);
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("but source `default` is not tenanted"),
        "unexpected message: {message}"
    );
}

/// From the tenant step onward a command has a tenant, and every write after
/// it carries that value. Before it there is nothing to carry, so a write
/// placed there is refused at deploy rather than storing a row that belongs to
/// nobody (`knowledgebase/declarative-saas/decisions/101-*`).
#[test]
fn a_write_before_the_tenant_step_is_refused_at_deploy() {
    let commands = ACCEPT.replace(
        "  steps:\n    - name: invite\n",
        "  steps:\n    - name: early\n      insert:\n        table: { schema: public, name: iam_membership }\n        object: { user_id: { arg: token } }\n        returning: [tenant_id]\n    - name: invite\n",
    );
    let dir = build_with_commands("write_before_tenant_step", &commands, Some(TENANCY_YAML));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("step `early` — an insert — runs before it")
            && message.contains("Move it after `invite`"),
        "unexpected message: {message}"
    );
}

/// A scoped read before the tenant step would be answered from the caller's
/// tenant, which is not this command's. It is refused the same way.
#[test]
fn a_scoped_read_before_the_tenant_step_is_refused_at_deploy() {
    let commands = ACCEPT.replace(
        "  steps:\n    - name: invite\n",
        "  steps:\n    - name: peek\n      select_many:\n        table: { schema: public, name: iam_membership }\n        by: { user_id: { arg: token } }\n        order_by: [user_id]\n        returning: [tenant_id]\n        maximum_rows: 10\n    - name: invite\n",
    );
    let dir = build_with_commands("read_before_tenant_step", &commands, Some(TENANCY_YAML));
    let message = tenancy_error(load_metadata_dir(&dir));
    assert!(
        message.contains("step `peek` reads `public.iam_membership`, a tenanted table, before it"),
        "unexpected message: {message}"
    );
}

/// After the tenant step an update is an ordinary bounded write: its predicate
/// compares against what the step resolved. The blanket refusal of updates in
/// a `from` command is gone.
#[test]
fn an_update_after_the_tenant_step_is_accepted() {
    let commands = [
        ACCEPT,
        "    - name: mark\n      update:\n        table: { schema: public, name: iam_membership }\n        where: { user_id: { arg: token } }\n        set: { role: { literal: member } }\n        returning: [tenant_id]\n",
    ]
    .concat();
    let dir = build_with_commands("update_after_tenant_step", &commands, Some(TENANCY_YAML));
    let metadata = load_metadata_dir(&dir).expect("an update after the tenant step loads");
    assert_eq!(metadata.commands[0].steps.len(), 2);
}

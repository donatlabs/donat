use std::env;

use donat_catalog::{RelationKind, introspect};
use tokio_postgres::NoTls;

#[tokio::test]
async fn introspection_retains_table_view_and_materialized_view_relation_kinds() {
    let pg_url = env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_string());
    let (client, connection) = tokio_postgres::connect(&pg_url, NoTls)
        .await
        .expect("Postgres must be available for catalog introspection tests");
    let connection_task = tokio::spawn(connection);

    let prefix = format!("catalog_relation_kind_{}", std::process::id());
    let table = format!("{prefix}_table");
    let view = format!("{prefix}_view");
    let materialized_view = format!("{prefix}_materialized_view");
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (id integer PRIMARY KEY); \
             CREATE VIEW public.{view} AS SELECT id FROM public.{table}; \
             CREATE MATERIALIZED VIEW public.{materialized_view} AS SELECT id FROM public.{table};"
        ))
        .await
        .expect("test relations must be created");

    let catalog = introspect(&client)
        .await
        .expect("test relations must be introspected");

    assert_eq!(
        catalog.table("public", &table).unwrap().relation_kind,
        RelationKind::Table
    );
    assert_eq!(
        catalog.table("public", &view).unwrap().relation_kind,
        RelationKind::View
    );
    assert_eq!(
        catalog
            .table("public", &materialized_view)
            .unwrap()
            .relation_kind,
        RelationKind::MaterializedView
    );

    client
        .batch_execute(&format!(
            "DROP MATERIALIZED VIEW public.{materialized_view}; \
             DROP VIEW public.{view}; \
             DROP TABLE public.{table};"
        ))
        .await
        .expect("test relations must be dropped");
    connection_task.abort();
}

#[tokio::test]
async fn introspection_retains_raw_postgres_type_modifiers() {
    let pg_url = env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_string());
    let (client, connection) = tokio_postgres::connect(&pg_url, NoTls)
        .await
        .expect("Postgres must be available for catalog introspection tests");
    let connection_task = tokio::spawn(connection);

    let table = format!("catalog_type_modifiers_{}", std::process::id());
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (\
                id integer PRIMARY KEY, \
                short_value smallint, \
                integer_value integer, \
                big_value bigint, \
                amount numeric(5, 2), \
                code varchar(3), \
                fixed_code char(2), \
                created_at timestamp(3), \
                received_at timestamptz(6)\
            );"
        ))
        .await
        .expect("test table must be created");

    let catalog = introspect(&client)
        .await
        .expect("test table must be introspected");
    let table_info = catalog.table("public", &table).expect("test table exists");

    assert_eq!(table_info.column("amount").unwrap().pg_type, "numeric");
    assert_eq!(
        table_info.column("amount").unwrap().pg_typmod,
        ((5 << 16) | 2) + 4
    );
    assert_eq!(table_info.column("code").unwrap().pg_typmod, 3 + 4);
    assert_eq!(table_info.column("created_at").unwrap().pg_typmod, 3);
    assert_eq!(table_info.column("received_at").unwrap().pg_typmod, 6);
    assert_eq!(table_info.column("id").unwrap().pg_typmod, -1);

    client
        .batch_execute(&format!("DROP TABLE public.{table};"))
        .await
        .expect("test table must be dropped");
    connection_task.abort();
}

#[tokio::test]
async fn introspection_preserves_domain_view_nullability_and_base_logical_type() {
    let pg_url = env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_string());
    let (client, connection) = tokio_postgres::connect(&pg_url, NoTls)
        .await
        .expect("Postgres must be available for catalog introspection tests");
    let connection_task = tokio::spawn(connection);

    let suffix = std::process::id();
    let required_domain = format!("catalog_required_text_{suffix}");
    let nullable_domain = format!("catalog_nullable_text_{suffix}");
    let source = format!("catalog_domain_source_{suffix}");
    let view = format!("catalog_domain_view_{suffix}");
    client
        .batch_execute(&format!(
            "CREATE DOMAIN public.{required_domain} AS text NOT NULL; \
             CREATE DOMAIN public.{nullable_domain} AS text; \
             CREATE TABLE public.{source} (\
               id integer PRIMARY KEY, \
               required_value text NOT NULL, \
               optional_value text\
             ); \
             INSERT INTO public.{source} VALUES (1, 'present', NULL); \
             CREATE VIEW public.{view} AS SELECT \
               required_value::{required_domain} AS required_domain_value, \
               optional_value::{nullable_domain} AS nullable_domain_value, \
               optional_value::{required_domain} AS checked_required_value, \
               required_value AS base_view_value \
             FROM public.{source};"
        ))
        .await
        .expect("domain-backed test view must be created");

    let catalog = introspect(&client)
        .await
        .expect("domain-backed view must be introspected");
    let view_info = catalog.table("public", &view).expect("test view exists");

    let required = view_info
        .column("required_domain_value")
        .expect("required domain column exists");
    assert_eq!(required.pg_type, "text");
    assert_eq!(
        required.native_type.as_deref(),
        Some(format!("public.{required_domain}").as_str())
    );
    assert!(!required.nullable);

    let nullable = view_info
        .column("nullable_domain_value")
        .expect("nullable domain column exists");
    assert_eq!(nullable.pg_type, "text");
    assert_eq!(
        nullable.native_type.as_deref(),
        Some(format!("public.{nullable_domain}").as_str())
    );
    assert!(nullable.nullable);

    let base = view_info
        .column("base_view_value")
        .expect("base view column exists");
    assert_eq!(base.pg_type, "text");
    assert_eq!(base.native_type, None);
    assert!(
        base.nullable,
        "plain view expressions retain PostgreSQL's conservative nullability"
    );

    let error = client
        .query(
            &format!("SELECT checked_required_value FROM public.{view}"),
            &[],
        )
        .await
        .expect_err("a NOT NULL domain cast must reject a null view expression");
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::NOT_NULL_VIOLATION)
    );

    client
        .batch_execute(&format!(
            "DROP VIEW public.{view}; \
             DROP TABLE public.{source}; \
             DROP DOMAIN public.{nullable_domain}; \
             DROP DOMAIN public.{required_domain};"
        ))
        .await
        .expect("domain-backed test objects must be dropped");
    connection_task.abort();
}

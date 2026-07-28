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

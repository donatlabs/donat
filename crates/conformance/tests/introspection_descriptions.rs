//! Per-column field descriptions: a tracked-table column carries the
//! `configuration.column_config.<col>.comment` from metadata as its GraphQL
//! introspection `description`; a column with no comment stays `null`.

use donat_conformance::{Suite, Transport};

const COLDESC: &str = "queries/graphql_introspection/column_descriptions";

#[test]
fn introspection_column_descriptions() {
    let s = Suite::new("introspection_column_descriptions").start();
    s.setup_v1q(&format!("{COLDESC}/setup.yaml"));

    s.check_query_f(
        &format!("{COLDESC}/person_field_descriptions.yaml"),
        Transport::Http,
    );

    s.teardown_v1q(&format!("{COLDESC}/teardown.yaml"));
}

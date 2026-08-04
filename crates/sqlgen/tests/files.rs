//! File attachments (spec 008): the three shapes sqlgen renders for them —
//! the file column's projection, the claim gate on a write, and the statement
//! that mints an upload URL.
//!
//! What these pin is that every one of them stays a single statement, and that
//! the URL a caller receives is built by the database rather than by the engine
//! walking the response.

use donat_ir::*;
use donat_sqlgen::{mutation_to_sql, operation_to_sql};
use serde_json::json;

fn table(schema: &str, name: &str) -> Table {
    Table {
        schema: schema.into(),
        name: name.into(),
    }
}

/// The download expression as the planner renders it: the store's own presign,
/// finished per row by the database.
const DOWNLOAD_URL_SQL: &str = concat!(
    "donat.s3_presigned_url('\\xa1b2'::bytea, 'AKIA%2F…', ",
    "'20260801/eu-central-1/s3/aws4_request', '20260801T120000Z', 300, ",
    "'https://bucket.s3.example', 'bucket.s3.example', '/' || {row}.object_key, 'GET')"
);

fn file_field(alias: &str) -> OutputField {
    OutputField {
        alias: alias.to_string(),
        value: FieldValue::FileRef {
            column: "photo".to_string(),
            attachment: "public.pet.photo".to_string(),
            url_sql: DOWNLOAD_URL_SQL.to_string(),
            fields: vec![
                FileRefOutput {
                    alias: "id".into(),
                    field: FileRefField::Id,
                },
                FileRefOutput {
                    alias: "file_name".into(),
                    field: FileRefField::FileName,
                },
                FileRefOutput {
                    alias: "media_type".into(),
                    field: FileRefField::MediaType,
                },
                FileRefOutput {
                    alias: "size".into(),
                    field: FileRefField::Size,
                },
                FileRefOutput {
                    alias: "url".into(),
                    field: FileRefField::Url,
                },
            ],
        },
    }
}

fn select_with(fields: Vec<OutputField>) -> SelectQuery {
    SelectQuery {
        from: FromSource::Table(table("public", "pet")),
        fields,
        predicate: None,
        order_by: vec![],
        limit: None,
        nodes_limit: None,
        offset: None,
        distinct_on: vec![],
        single: false,
    }
}

#[test]
fn a_file_column_projects_an_object_whose_url_is_signed_in_sql() {
    let sql = operation_to_sql(&[RootField::Select {
        alias: "pet".into(),
        query: select_with(vec![
            OutputField {
                alias: "id".into(),
                value: FieldValue::Column {
                    column: "id".into(),
                    pg_type: "uuid".into(),
                },
            },
            file_field("photo"),
        ]),
    }]);
    // The signing chain and the timestamp are constants of the statement; only
    // the per-row canonical request and HMAC are left to the database.
    assert!(sql.contains("donat.s3_presigned_url("), "{sql}");
    assert!(sql.contains("FROM donat.file_uploads"), "{sql}");
    insta::assert_snapshot!(sql);
}

#[test]
fn a_null_file_column_selects_no_upload_row() {
    // The projection is a correlated subquery on the column's value, so an
    // unset attachment is NULL rather than an object of nulls. Pinning the
    // shape is what guarantees that.
    let sql = operation_to_sql(&[RootField::Select {
        alias: "pet".into(),
        query: select_with(vec![file_field("photo")]),
    }]);
    // The join is narrowed to an upload claimed for this very column, so a
    // value that was written around the claim gate reads as NULL instead of
    // getting a signed URL.
    assert!(
        sql.contains("WHERE \"_t1\".\"id\" = \"_t0\".\"photo\""),
        "{sql}"
    );
    assert!(
        sql.contains("\"_t1\".\"state\" = 'claimed'")
            && sql.contains("\"_t1\".\"attachment\" = 'public.pet.photo'"),
        "{sql}"
    );
}

fn claim(ids: &[&str]) -> FileClaim {
    FileClaim {
        attachment: "public.pet.photo".into(),
        upload_ids: ids.iter().map(|id| id.to_string()).collect(),
        role: "customer".into(),
        session_key: Some("u-1".into()),
        error_path: "$.selectionSet.insert_pet.args.objects".into(),
        message: "file upload is not available".into(),
    }
}

fn insert_with_claims(claims: Vec<FileClaim>) -> MutationRoot {
    MutationRoot::Insert {
        alias: "insert_pet".into(),
        insert: InsertMutation {
            table: table("public", "pet"),
            columns: vec![
                ("name".into(), "text".into()),
                ("photo".into(), "uuid".into()),
            ],
            rows: vec![vec![
                Some(Scalar::Json(json!("Kit"))),
                Some(Scalar::Json(json!("11111111-1111-1111-1111-111111111111"))),
            ]],
            nested_object_inserts: vec![],
            on_conflict: None,
            check: None,
            check_path: "$".into(),
            validators: vec![],
            file_claims: claims,
            output: MutationOutput::SingleRow(vec![OutputField {
                alias: "id".into(),
                value: FieldValue::Column {
                    column: "id".into(),
                    pg_type: "uuid".into(),
                },
            }]),
        },
    }
}

#[test]
fn a_write_claims_its_uploads_in_the_same_statement() {
    let sql = mutation_to_sql(&insert_with_claims(vec![claim(&[
        "11111111-1111-1111-1111-111111111111",
    ])]));
    // One statement: the claim is a CTE beside the insert, and a claim that
    // matches no row raises, which rolls the insert back with it.
    assert_eq!(sql.matches("INSERT INTO").count(), 1, "{sql}");
    assert!(
        sql.contains("UPDATE donat.file_uploads SET state = 'claimed'"),
        "{sql}"
    );
    assert!(sql.contains("state = 'pending'"), "{sql}");
    assert!(sql.contains("byte_size > 0"), "{sql}");
    assert!(
        sql.contains("session_key IS NOT DISTINCT FROM 'u-1'"),
        "{sql}"
    );
    assert!(
        sql.contains("donat.raise_graphql_error('validation-failed'"),
        "{sql}"
    );
    insta::assert_snapshot!(sql);
}

#[test]
fn a_write_without_a_file_column_has_no_gate_at_all() {
    let sql = mutation_to_sql(&insert_with_claims(vec![]));
    assert!(!sql.contains("donat.file_uploads"), "{sql}");
}

#[test]
fn the_expected_claim_count_is_the_number_of_distinct_ids() {
    let sql = mutation_to_sql(&insert_with_claims(vec![claim(&[
        "11111111-1111-1111-1111-111111111111",
        "22222222-2222-2222-2222-222222222222",
    ])]));
    assert!(sql.contains(") <> 2 THEN"), "{sql}");
}

#[test]
fn requesting_an_upload_inserts_the_pending_row_and_returns_its_url() {
    let sql = mutation_to_sql(&MutationRoot::RequestFileUpload {
        alias: "donat_request_file_upload".into(),
        request: FileUploadRequest {
            upload_id: "33333333-3333-3333-3333-333333333333".into(),
            attachment: "public.pet.photo".into(),
            backend: "local".into(),
            object_key: "public.pet.photo/33333333-3333-3333-3333-333333333333".into(),
            file_name: "cat.png".into(),
            media_type: "image/png".into(),
            declared_bytes: 20481,
            byte_size: None,
            role: "customer".into(),
            session_key: Some("u-1".into()),
            expires_at_epoch: 1_754_000_900,
            max_pending_per_session: 20,
            max_per_minute_per_session: 60,
            limit_message: "too many upload requests".into(),
            error_path: "$.selectionSet.donat_request_file_upload".into(),
            url_sql:
                "'/v1/files/upload/33333333-3333-3333-3333-333333333333?exp=1754000900&sig=abc'"
                    .into(),
            complete_url_sql: None,
            method: "PUT".into(),
            headers: vec![("Content-Type".into(), "image/png".into())],
            fields: vec![
                FileUploadOutput {
                    alias: "id".into(),
                    field: FileUploadField::Id,
                },
                FileUploadOutput {
                    alias: "url".into(),
                    field: FileUploadField::Url,
                },
                FileUploadOutput {
                    alias: "method".into(),
                    field: FileUploadField::Method,
                },
                FileUploadOutput {
                    alias: "headers".into(),
                    field: FileUploadField::Headers,
                },
                FileUploadOutput {
                    alias: "complete_url".into(),
                    field: FileUploadField::CompleteUrl,
                },
                FileUploadOutput {
                    alias: "expires_at".into(),
                    field: FileUploadField::ExpiresAt,
                },
            ],
        },
    });
    assert_eq!(sql.matches("INSERT INTO").count(), 1, "{sql}");
    assert!(sql.contains("'pending'"), "{sql}");
    // The session's budget is counted in the same statement, so parallel
    // requests cannot each see room that only one of them has.
    assert!(sql.contains("< 20") && sql.contains("< 60"), "{sql}");
    assert!(sql.contains("interval '1 minute'"), "{sql}");
    // A disk upload has no completion call: the bytes pass through the engine.
    assert!(sql.contains("'complete_url', NULL"), "{sql}");
    insta::assert_snapshot!(sql);
}

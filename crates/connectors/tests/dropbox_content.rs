//! Dropbox content-origin connector proofs (spec 025 §4), against the SDK's
//! local provider stub.
//!
//! This file carries spec 025 §4's first addition —
//! `dropbox_content_download_is_bounded` — and the proof that the two Dropbox
//! connectors are two origins rather than one connector pointed at two hosts.

use base64::Engine;
use donat_connectors::providers::{dropbox, dropbox_content};
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AccessToken, AuthPlan, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass,
    MAX_HTTP_BODY_BYTES, Operation, Origin, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const PATH: &str = "/Homework/math/Prime_Numbers.txt";
const CONTENT: &[u8] = b"2 3 5 7 11 13 17 19 23 29";

fn operation(id: &str) -> &'static Operation {
    dropbox_content::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the dropbox_content declaration publishes {id}"))
}

fn applied_token() -> AccessToken {
    AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))
}

/// One rendered request, through the binder the runtime uses.
fn render(origin: &Origin, input: JsonValue) -> RequestPlan {
    let bound = dropbox_content::download_arg_input(&ConnectorConfiguration::default(), &input)
        .expect("the module composes its argument header");
    let mut request = operation("file.download")
        .plan_request(origin, &bound)
        .expect("the declared request renders");
    AuthPlan::oauth2_authorization_code()
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&applied_token()),
        )
        .expect("the declared plan applies the stored credential");
    request
}

fn result_header() -> String {
    json!({
        ".tag": "file",
        "id": "id:a4ayc_80_OEAAAAAAAAAYa",
        "name": "Prime_Numbers.txt",
        "rev": "a1c10ce0dd78",
        "size": CONTENT.len(),
    })
    .to_string()
}

/// `dropbox_content_request_shape`: the exact method, path, and argument header
/// Dropbox publishes for a content-download endpoint, with no request body at
/// all.
#[tokio::test]
async fn dropbox_content_request_shape() {
    let stub = ProviderStub::start([Expectation::new("POST", "/2/files/download")
        .query("")
        .header("dropbox-api-arg", &json!({ "path": PATH }).to_string())
        .no_body()
        .respond_header("dropbox-api-result", &result_header())
        .respond_bytes(200, CONTENT.to_vec())])
    .await;

    let request = render(&stub.origin(), json!({ "path": PATH }));
    assert_eq!(request.method(), "POST");
    assert_eq!(request.url().path(), "/2/files/download");
    assert!(request.body().is_empty(), "a download sends no body");
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `dropbox_content_auth_is_applied`: the stored OAuth2 token reaches the wire
/// as `Authorization: Bearer …` and appears nowhere else — including not in the
/// argument header this module composes.
#[tokio::test]
async fn dropbox_content_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("POST", "/2/files/download")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_bytes(200, CONTENT.to_vec())])
    .await;

    let request = render(&stub.origin(), json!({ "path": PATH }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(
        !request
            .headers()
            .get("dropbox-api-arg")
            .and_then(|value| value.to_str().ok())
            .expect("the argument header is composed")
            .contains(SECRET_SENTINEL)
    );
    assert!(!format!("{request:?}").contains(SECRET_SENTINEL));
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `dropbox_content_error_map`: the shared Dropbox status table classifies a
/// failed download, and no provider prose crosses the boundary.
#[tokio::test]
async fn dropbox_content_error_map() {
    for (status, expected) in [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (409, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
    ] {
        let failure = dropbox_content::decode(
            operation("file.download"),
            status,
            &reqwest::header::HeaderMap::new(),
            format!(r#"{{"error_summary":"path/not_found/..{SECRET_SENTINEL}"}}"#).as_bytes(),
        )
        .expect_err("a non-success status is never a download");
        assert_eq!(failure.class(), expected, "status {status}");
        assert!(!failure.diagnostic().contains(SECRET_SENTINEL));
    }
}

/// `dropbox_content_download_is_bounded` (spec 025 §4 addition 1): a response
/// over the ceiling fails as `validation` with no partial output.
#[test]
fn dropbox_content_download_is_bounded() {
    let download = operation("file.download");
    let headers = reqwest::header::HeaderMap::new();

    // The exact boundary is admitted.
    let at_ceiling = vec![b'x'; MAX_HTTP_BODY_BYTES];
    let decoded = dropbox_content::decode(download, 200, &headers, &at_ceiling)
        .expect("the exact ceiling is a download");
    assert_eq!(
        decoded.get("content_bytes"),
        Some(&json!(MAX_HTTP_BODY_BYTES))
    );

    // One byte over is refused, and nothing partial escapes with the refusal.
    let over_ceiling = vec![b'x'; MAX_HTTP_BODY_BYTES + 1];
    let failure = dropbox_content::decode(download, 200, &headers, &over_ceiling)
        .expect_err("a body past the ceiling is not a download");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "connector_response_too_large");
    assert_eq!(failure.provider_status(), Some(200));
    let diagnostic = failure.diagnostic();
    assert!(
        !diagnostic.contains("content_base64") && !diagnostic.contains('x'),
        "a refused download carries no part of the file: {diagnostic}"
    );
}

/// `dropbox_content_output_contract`: the bytes are composed into the declared
/// output, and Dropbox's own result header travels beside them.
#[test]
fn dropbox_content_output_contract() {
    let download = operation("file.download");
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        "dropbox-api-result",
        reqwest::header::HeaderValue::from_str(&result_header()).expect("a fixture header"),
    );

    let decoded = dropbox_content::decode(download, 200, &headers, CONTENT)
        .expect("the declared contract is satisfied");
    assert_eq!(
        decoded.get("content_base64").and_then(JsonValue::as_str),
        Some(
            base64::engine::general_purpose::STANDARD
                .encode(CONTENT)
                .as_str()
        )
    );
    assert_eq!(decoded.get("content_bytes"), Some(&json!(CONTENT.len())));
    assert_eq!(
        decoded.get("content_type"),
        Some(&json!("application/octet-stream"))
    );
    assert_eq!(
        decoded
            .get("metadata")
            .and_then(|metadata| metadata.get("rev")),
        Some(&json!("a1c10ce0dd78")),
        "Dropbox's own result header is the file's metadata"
    );

    // A response with neither header is still a download: the bytes are the
    // answer, and the two optional fields are null.
    let bare = dropbox_content::decode(download, 200, &reqwest::header::HeaderMap::new(), CONTENT)
        .expect("a download with no metadata header is still a download");
    assert_eq!(bare.get("content_type"), Some(&JsonValue::Null));
    assert_eq!(bare.get("metadata"), Some(&JsonValue::Null));

    // An empty file is a file.
    let empty =
        dropbox_content::decode(download, 200, &headers, b"").expect("an empty file is a download");
    assert_eq!(empty.get("content_bytes"), Some(&json!(0)));
}

/// `dropbox_content_argument_is_composed_by_the_module`: the argument header is
/// never a caller's, and a value that could not be one header line is refused
/// before the request is rendered.
#[test]
fn dropbox_content_argument_is_composed_by_the_module() {
    let configuration = ConnectorConfiguration::default();

    let bound = dropbox_content::download_arg_input(&configuration, &json!({ "path": PATH }))
        .expect("a declared path composes");
    assert_eq!(
        bound.get("api_arg").and_then(JsonValue::as_str),
        Some(json!({ "path": PATH }).to_string().as_str())
    );

    // An input that carries the composed slot is refused rather than
    // overwritten: it would be a caller choosing the whole argument document.
    assert!(
        dropbox_content::download_arg_input(
            &configuration,
            &json!({ "path": PATH, "api_arg": "{\"path\":\"/etc/passwd\"}" }),
        )
        .is_err()
    );
    // A value that could forge a second header line never becomes one.
    for hostile in ["", "/a\r\nX-Injected: 1", "/a\u{0}b"] {
        assert!(
            dropbox_content::download_arg_input(&configuration, &json!({ "path": hostile }))
                .is_err(),
            "`{hostile}` is not a Dropbox path"
        );
    }
    // The slot is the module's, so no Process can bind it.
    let projection = operation("file.download").project();
    assert!(
        projection
            .inputs()
            .iter()
            .all(|input| input.name() != "api_arg"),
        "the argument header is composed, never published as an input"
    );
    assert!(
        projection
            .inputs()
            .iter()
            .any(|input| input.name() == "path"),
        "the caller binds one typed field"
    );
}

/// The structural answer of spec 025 §2, asserted rather than described: the two
/// Dropbox connectors are two origins, and neither can render a request onto the
/// other's host.
#[test]
fn dropbox_content_is_a_second_connector_with_its_own_origin() {
    let metadata_origin =
        Origin::parse("https://api.dropboxapi.com").expect("the published RPC origin is valid");
    let content_origin = Origin::parse("https://content.dropboxapi.com")
        .expect("the published content origin is valid");
    assert_ne!(
        metadata_origin.as_url().host_str(),
        content_origin.as_url().host_str()
    );

    // Every metadata operation renders on the metadata host and none of them is
    // the download.
    for operation in dropbox::connector().operations() {
        assert!(
            operation.id() != "file.download",
            "the download is the content connector's"
        );
    }
    assert!(dropbox::connector().operation("file.download").is_none());

    // The content connector publishes exactly one operation, and it renders on
    // the content host.
    let content = dropbox_content::connector();
    assert_eq!(content.operations().len(), 1);
    let request = render(&content_origin, json!({ "path": PATH }));
    assert_eq!(
        request.url().host_str(),
        Some("content.dropboxapi.com"),
        "a compiled origin is not moved by an argument"
    );
    assert_eq!(
        content
            .operation("file.download")
            .and_then(Operation::effect_class),
        Some(EffectClass::ReadOnly)
    );
    // The two connectors are two deployment instances with two names.
    assert_ne!(dropbox::NAME, dropbox_content::NAME);
}

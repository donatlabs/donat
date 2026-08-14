//! The language, checked against its own corpus.
//!
//! `tests/data` is Kriti's evaluation test suite, vendored unchanged from
//! <https://github.com/hasura/kriti-lang> (Apache-2.0, `tests/data/LICENSE`):
//! 34 templates, one shared input document, and the output each template is
//! supposed to produce. It exercises every part of the language this port
//! claims to implement — loops, conditionals, optional lookups, defaulting,
//! interpolation and the whole function collection — which is why it is here
//! rather than a set of examples we thought up ourselves. A port is either
//! this or it is a different language with the same syntax.
//!
//! Values are compared as parsed JSON, not as text: the golden files are
//! pretty-printed and that is a property of the harness that produced them,
//! not of the language.

use std::path::PathBuf;

use serde_json::{Map, Value as Json};

fn data(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The corpus binds its input document to `$`.
fn context() -> Map<String, Json> {
    let source: Json = serde_json::from_str(&data("source.json")).expect("source.json parses");
    let mut context = Map::new();
    context.insert("$".to_string(), source);
    context
}

#[test]
fn every_example_produces_its_golden_output() {
    let context = context();
    let mut failures = Vec::new();

    for number in 1..=34 {
        let template = data(&format!("example{number}.kriti"));
        let expected: Json = serde_json::from_str(&data(&format!("golden{number}.json")))
            .unwrap_or_else(|e| panic!("golden{number}.json parses: {e}"));

        match donat_kriti::render(&template, &context) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => failures.push(format!(
                "example{number}: got {} , expected {}",
                truncate(&actual.to_string()),
                truncate(&expected.to_string())
            )),
            Err(error) => failures.push(format!("example{number}: {error}")),
        }
    }

    assert!(
        failures.is_empty(),
        "{} of 34 examples differ from Kriti's own output:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn truncate(text: &str) -> String {
    if text.chars().count() > 200 {
        format!("{}…", text.chars().take(200).collect::<String>())
    } else {
        text.to_string()
    }
}

/// The shapes a transform actually uses, spelled out rather than left to the
/// corpus: this is what a request transform looks like in metadata.
#[test]
fn a_request_transform_is_an_ordinary_template() {
    let mut context = Map::new();
    context.insert(
        "$body".to_string(),
        serde_json::json!({
            "input": { "id": "42", "name": "Sam", "tags": ["a", "b"] },
            "action": { "name": "update_user" }
        }),
    );
    context.insert(
        "$base_url".to_string(),
        Json::String("http://idp:8080/auth/v1".to_string()),
    );
    context.insert(
        "$session_variables".to_string(),
        serde_json::json!({ "x-donat-role": "support" }),
    );

    let url = donat_kriti::render(r#""{{$base_url}}/users/{{$body.input.id}}""#, &context)
        .expect("the url renders");
    assert_eq!(url, Json::String("http://idp:8080/auth/v1/users/42".into()));

    let body = donat_kriti::render(
        r#"{ "name": {{ $body.input.name }}, "roles": {{ range _, t := $body.input.tags }} {{ t }} {{ end }} }"#,
        &context,
    )
    .expect("the body renders");
    assert_eq!(
        body,
        serde_json::json!({ "name": "Sam", "roles": ["a", "b"] })
    );

    // A session variable decides a header, and an absent one defaults rather
    // than failing the request.
    let header = donat_kriti::render(r#""{{ $session_variables['x-donat-role'] }}""#, &context)
        .expect("the header renders");
    assert_eq!(header, Json::String("support".into()));

    let missing = donat_kriti::render(r#"{{ $session_variables?['nope'] ?? "none" }}"#, &context)
        .expect("the default applies");
    assert_eq!(missing, Json::String("none".into()));
}

#[test]
fn a_template_that_cannot_be_parsed_is_refused_when_it_is_written() {
    assert!(donat_kriti::Template::parse("{{ range i, x := $ }} 1").is_err());
    assert!(donat_kriti::Template::parse("{{ if true }} 1 {{ end }}").is_ok());
    assert!(donat_kriti::Template::parse("{{ }}").is_err());
}

#[test]
fn a_missing_binding_is_an_error_unless_it_was_asked_for_optionally() {
    let context = Map::new();
    assert!(donat_kriti::render("{{ $nope }}", &context).is_err());
    assert_eq!(
        donat_kriti::render("{{ $nope? }}", &context).expect("optional"),
        Json::Null
    );
}

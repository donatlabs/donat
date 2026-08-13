//! Applying a Donat v2 request transform.
//!
//! Without one, an Action calls its handler the way this engine always has:
//! `POST <handler>` with `{action, input, session_variables}`. That is the
//! right shape for a handler written for this engine and the wrong shape for
//! every API that already existed — which is what a transform is for. Each
//! field of the declaration replaces one part of the outgoing request, and a
//! field that is absent leaves that part alone.
//!
//! The templates are [Kriti](donat_kriti), and the bindings are the ones Donat
//! binds: `$body` (the request as it would have been sent), `$base_url`,
//! `$query_params` and `$session_variables`.
//!
//! The same machinery runs the other way for a `response_transform`, where
//! `$body` is what the handler replied and the template says what the action
//! returns — which is how an API whose fields are not the ones the schema
//! promises can still satisfy that schema.

use std::collections::BTreeMap;

use donat_metadata::{BodyTransform, QueryParamsTransform, RequestTransform, ResponseTransform};
use serde_json::{Map as JsonMap, Value as Json};

/// The request being assembled, before it becomes a `reqwest` builder.
pub(crate) struct Outgoing {
    pub method: reqwest::Method,
    pub url: String,
    pub query: Vec<(String, Option<String>)>,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

pub(crate) enum Body {
    None,
    Json(Json),
    Form(Vec<(String, String)>),
}

impl Outgoing {
    /// The call this engine makes when nothing is transformed.
    pub(crate) fn donat(url: &str, payload: &Json) -> Self {
        Self {
            method: reqwest::Method::POST,
            url: url.to_string(),
            query: Vec::new(),
            headers: Vec::new(),
            body: Body::Json(payload.clone()),
        }
    }

    pub(crate) fn into_request(self, client: &reqwest::Client) -> reqwest::RequestBuilder {
        let url = match query_string(&self.query) {
            Some(query) => {
                let separator = if self.url.contains('?') { '&' } else { '?' };
                format!("{}{separator}{query}", self.url)
            }
            None => self.url.clone(),
        };
        let mut req = client.request(self.method, url);
        for (name, value) in self.headers {
            req = req.header(name, value);
        }
        match self.body {
            Body::None => req,
            Body::Json(body) => req.json(&body),
            Body::Form(fields) => req.form(&fields),
        }
    }
}

/// Render the query, keeping the difference between `?flag` and `?flag=`.
///
/// A parameter declared with a `null` value is sent bare, because some APIs
/// read the two differently — which is the only reason the declaration can say
/// `null` at all. `reqwest`'s own `query()` cannot express it: every pair it
/// writes has an `=`.
fn query_string(pairs: &[(String, Option<String>)]) -> Option<String> {
    if pairs.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (name, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&percent_encode(name));
        if let Some(value) = value {
            out.push('=');
            out.push_str(&percent_encode(value));
        }
    }
    Some(out)
}

/// Percent-encode one query component. Everything outside RFC 3986's
/// unreserved set is escaped — a value is somebody's argument, not syntax.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The bindings a transform's templates are evaluated against.
pub(crate) fn context(
    base_url: &str,
    payload: &Json,
    session_vars: &JsonMap<String, Json>,
) -> JsonMap<String, Json> {
    let mut context = JsonMap::new();
    context.insert("$body".to_string(), payload.clone());
    context.insert("$base_url".to_string(), Json::String(base_url.to_string()));
    context.insert(
        "$session_variables".to_string(),
        Json::Object(session_vars.clone()),
    );
    context.insert("$query_params".to_string(), Json::Object(JsonMap::new()));
    context
}

/// Render a template that produces a string — a URL, a header, a query value.
///
/// A template evaluating to something other than a string contributes its
/// compact JSON, which is what an unescaped template does in Donat.
fn render_text(source: &str, context: &JsonMap<String, Json>) -> Result<String, String> {
    donat_kriti::render_unescaped(source, context).map_err(|e| e.to_string())
}

fn render_json(source: &str, context: &JsonMap<String, Json>) -> Result<Json, String> {
    donat_kriti::render(source, context).map_err(|e| e.to_string())
}

/// Apply the declaration to the request.
pub(crate) fn apply(
    outgoing: &mut Outgoing,
    transform: &RequestTransform,
    context: &JsonMap<String, Json>,
) -> Result<(), String> {
    if let Some(engine) = &transform.template_engine
        && !engine.eq_ignore_ascii_case("kriti")
    {
        return Err(format!("unsupported template engine '{engine}'"));
    }

    if let Some(method) = &transform.method {
        outgoing.method = method
            .to_ascii_uppercase()
            .parse()
            .map_err(|_| format!("'{method}' is not an HTTP method"))?;
    }

    if let Some(url) = &transform.url {
        outgoing.url = render_text(url, context)?;
    }

    if let Some(params) = &transform.query_params {
        outgoing.query = query_params(params, context)?;
    }

    if let Some(headers) = &transform.request_headers {
        for name in &headers.remove_headers {
            outgoing
                .headers
                .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        }
        for (name, template) in &headers.add_headers {
            let value = render_text(template, context)?;
            outgoing
                .headers
                .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            outgoing.headers.push((name.clone(), value));
        }
    }

    if let Some(body) = &transform.body {
        outgoing.body = match body {
            // Version 1 wrote a bare template, which always meant "send this".
            BodyTransform::Template(template) => Body::Json(render_json(template, context)?),
            BodyTransform::Action(action) => match action.action.as_str() {
                "remove" => Body::None,
                "transform" => {
                    let template = action
                        .template
                        .as_deref()
                        .ok_or("a 'transform' body needs a 'template'")?;
                    Body::Json(render_json(template, context)?)
                }
                "x_www_form_urlencoded" => {
                    let fields = action
                        .form_template
                        .as_ref()
                        .ok_or("an 'x_www_form_urlencoded' body needs a 'form_template'")?;
                    Body::Form(form_fields(fields, context)?)
                }
                other => return Err(format!("unknown body action '{other}'")),
            },
        };
    }

    Ok(())
}

/// Every template a transform declares, refused now rather than at the first
/// call.
///
/// A transform is deploy-time configuration, and a template that cannot parse
/// is a typo — one that would otherwise sit quiet until an operator pressed a
/// button, and then fail as a runtime error in someone else's request.
pub fn unparsable_templates(
    request: Option<&RequestTransform>,
    response: Option<&ResponseTransform>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut check_json = |what: &str, source: &str| {
        if let Err(error) = donat_kriti::Template::parse(source) {
            problems.push(format!("{what}: {error}"));
        }
    };
    if let Some(request) = request {
        if let Some(url) = &request.url {
            check_json("url", &format!("\"{url}\""));
        }
        if let Some(params) = &request.query_params {
            match params {
                QueryParamsTransform::Table(table) => {
                    for (name, template) in table {
                        if let Some(template) = template {
                            check_json(&format!("query_params.{name}"), &format!("\"{template}\""));
                        }
                    }
                }
                QueryParamsTransform::Template(template) => check_json("query_params", template),
            }
        }
        if let Some(headers) = &request.request_headers {
            for (name, template) in &headers.add_headers {
                check_json(
                    &format!("request_headers.{name}"),
                    &format!("\"{template}\""),
                );
            }
        }
        if let Some(body) = &request.body {
            check_body(&mut check_json, "body", body);
        }
    }
    if let Some(response) = response
        && let Some(body) = &response.body
    {
        check_body(&mut check_json, "response body", body);
    }
    problems
}

fn check_body(check: &mut impl FnMut(&str, &str), what: &str, body: &BodyTransform) {
    match body {
        BodyTransform::Template(template) => check(what, template),
        BodyTransform::Action(action) => {
            if let Some(template) = &action.template {
                check(what, template);
            }
            if let Some(fields) = &action.form_template {
                for (name, template) in fields {
                    check(&format!("{what}.{name}"), &format!("\"{template}\""));
                }
            }
        }
    }
}

/// Apply a response transform to what the handler replied.
pub(crate) fn apply_response(
    transform: &ResponseTransform,
    body: &Json,
    session_vars: &JsonMap<String, Json>,
) -> Result<Json, String> {
    if let Some(engine) = &transform.template_engine
        && !engine.eq_ignore_ascii_case("kriti")
    {
        return Err(format!("unsupported template engine '{engine}'"));
    }
    let Some(body_transform) = &transform.body else {
        return Ok(body.clone());
    };
    let mut context = JsonMap::new();
    context.insert("$body".to_string(), body.clone());
    context.insert(
        "$session_variables".to_string(),
        Json::Object(session_vars.clone()),
    );
    match body_transform {
        BodyTransform::Template(template) => render_json(template, &context),
        BodyTransform::Action(action) => match action.action.as_str() {
            // "Send nothing" has no meaning coming back; an action still has
            // to return something its output type accepts.
            "remove" => Ok(Json::Null),
            "transform" => {
                let template = action
                    .template
                    .as_deref()
                    .ok_or("a 'transform' body needs a 'template'")?;
                render_json(template, &context)
            }
            other => Err(format!("unknown response body action '{other}'")),
        },
    }
}

fn query_params(
    params: &QueryParamsTransform,
    context: &JsonMap<String, Json>,
) -> Result<Vec<(String, Option<String>)>, String> {
    match params {
        QueryParamsTransform::Table(table) => {
            let mut out = Vec::with_capacity(table.len());
            for (name, template) in table {
                let value = match template {
                    Some(template) => Some(render_text(template, context)?),
                    None => None,
                };
                out.push((name.clone(), value));
            }
            Ok(out)
        }
        // One template for the whole set, which has to produce an object.
        QueryParamsTransform::Template(template) => match render_json(template, context)? {
            Json::Object(map) => Ok(map
                .into_iter()
                .map(|(name, value)| {
                    let value = match value {
                        Json::Null => None,
                        Json::String(text) => Some(text),
                        other => Some(other.to_string()),
                    };
                    (name, value)
                })
                .collect()),
            other => Err(format!(
                "query_params must produce an object, got {}",
                match other {
                    Json::Array(_) => "an array",
                    Json::String(_) => "a string",
                    _ => "a scalar",
                }
            )),
        },
    }
}

fn form_fields(
    fields: &BTreeMap<String, String>,
    context: &JsonMap<String, Json>,
) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::with_capacity(fields.len());
    for (name, template) in fields {
        out.push((name.clone(), render_text(template, context)?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn invocation() -> (Json, JsonMap<String, Json>) {
        let payload = json!({
            "action": { "name": "idp_users" },
            "input": { "limit": 10, "id": "u-1", "name": "Sam" },
            "session_variables": { "x-donat-role": "support" }
        });
        let mut session = JsonMap::new();
        session.insert("x-donat-role".into(), Json::String("support".into()));
        let context = context("http://idp:8080/auth/v1", &payload, &session);
        (payload, context)
    }

    fn transform(json: Json) -> RequestTransform {
        serde_json::from_value(json).expect("the declaration parses")
    }

    #[test]
    fn without_a_transform_the_call_is_the_one_this_engine_has_always_made() {
        let (payload, _) = invocation();
        let outgoing = Outgoing::donat("http://handler", &payload);
        assert_eq!(outgoing.method, reqwest::Method::POST);
        assert!(matches!(outgoing.body, Body::Json(_)));
    }

    #[test]
    fn a_query_keeps_the_difference_between_a_flag_and_an_empty_value() {
        assert_eq!(query_string(&[]), None);
        assert_eq!(
            query_string(&[("limit".into(), Some("10".into()))]).as_deref(),
            Some("limit=10")
        );
        // Declared with no value at all: sent bare, which is what some APIs
        // read as "on" and `?flag=` as "set to nothing".
        assert_eq!(
            query_string(&[("archived".into(), None)]).as_deref(),
            Some("archived")
        );
        assert_eq!(
            query_string(&[("q".into(), Some("a b&c".into())), ("flag".into(), None),]).as_deref(),
            Some("q=a%20b%26c&flag")
        );
    }

    #[test]
    fn a_rest_read_is_a_method_a_url_and_a_query() {
        let (payload, context) = invocation();
        let mut outgoing = Outgoing::donat("http://idp:8080/auth/v1", &payload);
        apply(
            &mut outgoing,
            &transform(json!({
                "version": 2,
                "method": "GET",
                "url": "{{$base_url}}/users",
                "query_params": { "limit": "{{$body.input.limit}}" },
                "body": { "action": "remove" }
            })),
            &context,
        )
        .expect("the transform applies");

        assert_eq!(outgoing.method, reqwest::Method::GET);
        assert_eq!(outgoing.url, "http://idp:8080/auth/v1/users");
        assert_eq!(
            outgoing.query,
            vec![("limit".to_string(), Some("10".to_string()))]
        );
        // The envelope would be nonsense to an API that never asked for it.
        assert!(matches!(outgoing.body, Body::None));
    }

    #[test]
    fn an_argument_can_be_part_of_the_path() {
        let (payload, context) = invocation();
        let mut outgoing = Outgoing::donat("http://idp:8080/auth/v1", &payload);
        apply(
            &mut outgoing,
            &transform(json!({
                "version": 2,
                "method": "PUT",
                "url": "{{$base_url}}/users/{{$body.input.id}}",
                "body": { "action": "transform", "template": "{ \"given_name\": {{$body.input.name}} }" }
            })),
            &context,
        )
        .expect("the transform applies");

        assert_eq!(outgoing.url, "http://idp:8080/auth/v1/users/u-1");
        match &outgoing.body {
            Body::Json(body) => assert_eq!(body, &json!({ "given_name": "Sam" })),
            _ => panic!("expected a JSON body"),
        }
    }

    #[test]
    fn a_header_can_come_from_the_session_and_a_default_stands_in_for_an_absent_one() {
        let (payload, context) = invocation();
        let mut outgoing = Outgoing::donat("http://idp", &payload);
        outgoing
            .headers
            .push(("content-type".into(), "application/json".into()));
        apply(
            &mut outgoing,
            &transform(json!({
                "version": 2,
                "request_headers": {
                    "add_headers": {
                        "x-acting-role": "{{$session_variables['x-donat-role']}}",
                        "x-tenant": "{{$session_variables?['x-tenant'] ?? \"default\"}}"
                    },
                    "remove_headers": ["content-type"]
                }
            })),
            &context,
        )
        .expect("the transform applies");

        let headers: BTreeMap<_, _> = outgoing.headers.iter().cloned().collect();
        assert_eq!(
            headers.get("x-acting-role").map(String::as_str),
            Some("support")
        );
        assert_eq!(headers.get("x-tenant").map(String::as_str), Some("default"));
        assert!(!headers.contains_key("content-type"));
    }

    #[test]
    fn version_one_writes_the_body_as_a_bare_template() {
        let (payload, context) = invocation();
        let mut outgoing = Outgoing::donat("http://idp", &payload);
        apply(
            &mut outgoing,
            &transform(json!({ "body": "{ \"name\": {{$body.input.name}} }" })),
            &context,
        )
        .expect("the transform applies");
        match &outgoing.body {
            Body::Json(body) => assert_eq!(body, &json!({ "name": "Sam" })),
            _ => panic!("expected a JSON body"),
        }
    }

    #[test]
    fn a_form_body_sends_form_fields() {
        let (payload, context) = invocation();
        let mut outgoing = Outgoing::donat("http://idp", &payload);
        apply(
            &mut outgoing,
            &transform(json!({
                "version": 2,
                "body": {
                    "action": "x_www_form_urlencoded",
                    "form_template": { "username": "{{$body.input.name}}", "grant_type": "password" }
                }
            })),
            &context,
        )
        .expect("the transform applies");
        match &outgoing.body {
            Body::Form(fields) => assert_eq!(
                fields,
                &vec![
                    ("grant_type".to_string(), "password".to_string()),
                    ("username".to_string(), "Sam".to_string()),
                ]
            ),
            _ => panic!("expected a form body"),
        }
    }

    #[test]
    fn a_template_that_cannot_render_refuses_the_call_rather_than_sending_half_of_it() {
        let (payload, context) = invocation();
        let mut outgoing = Outgoing::donat("http://idp", &payload);
        let error = apply(
            &mut outgoing,
            &transform(json!({ "version": 2, "url": "{{$body.input.nope}}" })),
            &context,
        )
        .expect_err("a missing argument is an error");
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn only_kriti_is_a_template_engine() {
        let (payload, context) = invocation();
        let mut outgoing = Outgoing::donat("http://idp", &payload);
        assert!(
            apply(
                &mut outgoing,
                &transform(json!({ "template_engine": "Mustache", "url": "x" })),
                &context,
            )
            .is_err()
        );
    }
}

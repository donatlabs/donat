//! Rust stand-in for the tests-py node.js upstream GraphQL services
//! (`remote_schemas/nodejs/{remote_schema_perms,secondary_remote_schema_perms,
//! secondary_remote_schema_perms_error}.js`).
//!
//! Behaviour:
//! - Introspection requests (query text contains `__schema`) are answered
//!   with the canned introspection JSON captured from the real node
//!   services (`fixtures/remote_stub/*.introspection.json`) — services 1
//!   and 2 share identical typeDefs, service 3 differs (`user_id: Float`).
//! - Everything else runs through a small GraphQL executor that ports the
//!   apollo-server resolvers faithfully: in-memory `allMessages`,
//!   `user`/`users`, `userMessages` filtering (whered eq/gt/lt + includes),
//!   `gimmeText`, `hello`, `message`, `communications`, `profilePicture`,
//!   and the error-throwing `errorMsg` / "invalid argument" variants. The
//!   JS `formatError` deletes `extensions`, so thrown errors surface as
//!   `{message, locations, path}` only.

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::OnceLock;

use axum::extract::State;
use graphql_parser::query::{
    Definition, Document, Field, OperationDefinition, Selection, SelectionSet, Value as QValue,
};
use serde_json::{Map, Value as Json, json};

/// Which upstream node service to impersonate.
#[derive(Clone, Copy)]
pub enum Service {
    /// remote_schema_perms.js (GRAPHQL_SERVICE_1)
    One,
    /// secondary_remote_schema_perms.js (GRAPHQL_SERVICE_2)
    Two,
    /// secondary_remote_schema_perms_error.js (GRAPHQL_SERVICE_3)
    Three,
}

impl Service {
    fn introspection_file(self) -> &'static str {
        match self {
            Service::One => "remote_schema_perms.introspection.json",
            Service::Two => "secondary_remote_schema_perms.introspection.json",
            Service::Three => "secondary_remote_schema_perms_error.introspection.json",
        }
    }
}

pub struct Stub {
    pub url: String,
}

/// Boot the stub on 127.0.0.1:0 (axum on a current-thread tokio runtime in
/// a background thread) and return the bound URL. The server lives for the
/// remainder of the test process — matching the class-scoped node fixtures.
pub fn start(service: Service) -> Stub {
    let intro_path = dist_conformance::fixture_root()
        .join("remote_stub")
        .join(service.introspection_file());
    let intro_text =
        std::fs::read_to_string(&intro_path).expect("reading canned introspection json");
    let introspection: Json =
        serde_json::from_str(&intro_text).expect("parsing canned introspection json");
    let introspection: &'static Json = Box::leak(Box::new(introspection));

    let listener = TcpListener::bind("127.0.0.1:0").expect("binding stub listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking stub listener");
    let addr = listener.local_addr().expect("stub local addr");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("stub tokio runtime");
        rt.block_on(async move {
            let listener =
                tokio::net::TcpListener::from_std(listener).expect("tokio stub listener");
            // apollo-server answers POST on any path; the engine posts to
            // the bare base URL, so accept everything.
            let app = axum::Router::new()
                .fallback(axum::routing::post(handler))
                .with_state(introspection);
            axum::serve(listener, app).await.expect("stub serve");
        });
    });

    Stub {
        url: format!("http://127.0.0.1:{}", addr.port()),
    }
}

async fn handler(
    State(introspection): State<&'static Json>,
    axum::Json(body): axum::Json<Json>,
) -> axum::Json<Json> {
    let query = body.get("query").and_then(Json::as_str).unwrap_or("");
    if query.contains("__schema") {
        return axum::Json(introspection.clone());
    }
    let operation_name = body.get("operationName").and_then(Json::as_str);
    let variables = body
        .get("variables")
        .and_then(Json::as_object)
        .cloned()
        .unwrap_or_default();
    axum::Json(execute(query, operation_name, &variables))
}

// ------------------------------------------------------------ data

/// const allMessages = [...]
fn all_messages() -> &'static [Json] {
    static DATA: OnceLock<Vec<Json>> = OnceLock::new();
    DATA.get_or_init(|| {
        vec![
            json!({"id": 1, "name": "alice", "msg": "You win!"}),
            json!({"id": 2, "name": "bob", "msg": "You lose!"}),
            json!({"id": 3, "name": "alice", "msg": "Another alice"}),
        ]
    })
}

// ------------------------------------------------------------ executor

struct FieldError {
    message: String,
    line: usize,
    column: usize,
}

struct Ctx<'a> {
    frags: HashMap<&'a str, &'a graphql_parser::query::FragmentDefinition<'a, String>>,
    vars: Map<String, Json>,
    errors: Vec<Json>,
}

fn execute(query: &str, operation_name: Option<&str>, variables: &Map<String, Json>) -> Json {
    let doc: Document<'_, String> = match graphql_parser::parse_query(query) {
        Ok(d) => d,
        Err(e) => {
            return json!({ "errors": [{ "message": format!("{e}") }] });
        }
    };

    let mut frags = HashMap::new();
    for def in &doc.definitions {
        if let Definition::Fragment(f) = def {
            frags.insert(f.name.as_str(), f);
        }
    }

    // Pick the operation (by name when given, else the first one).
    let mut selected: Option<&OperationDefinition<'_, String>> = None;
    for def in &doc.definitions {
        if let Definition::Operation(op) = def {
            let name = match op {
                OperationDefinition::Query(q) => q.name.as_deref(),
                OperationDefinition::Mutation(m) => m.name.as_deref(),
                OperationDefinition::Subscription(s) => s.name.as_deref(),
                OperationDefinition::SelectionSet(_) => None,
            };
            match operation_name {
                Some(wanted) => {
                    if name == Some(wanted) {
                        selected = Some(op);
                        break;
                    }
                }
                None => {
                    selected = Some(op);
                    break;
                }
            }
        }
    }
    let Some(op) = selected else {
        return json!({ "errors": [{ "message": "Must provide an operation." }] });
    };

    // Variable values: request-supplied, falling back to declared defaults.
    let mut vars = variables.clone();
    let (set, var_defs) = match op {
        OperationDefinition::Query(q) => (&q.selection_set, Some(&q.variable_definitions)),
        OperationDefinition::SelectionSet(s) => (s, None),
        OperationDefinition::Mutation(m) => (&m.selection_set, Some(&m.variable_definitions)),
        OperationDefinition::Subscription(s) => (&s.selection_set, Some(&s.variable_definitions)),
    };
    if let Some(defs) = var_defs {
        for vd in defs.iter() {
            if !vars.contains_key(&vd.name) {
                if let Some(default) = &vd.default_value {
                    let v = value_to_json(default, &Map::new());
                    vars.insert(vd.name.clone(), v);
                }
            }
        }
    }

    let mut ctx = Ctx {
        frags,
        vars,
        errors: vec![],
    };
    let data = exec_set(&mut ctx, "Query", &json!({}), set, &mut vec![]);
    if ctx.errors.is_empty() {
        json!({ "data": data })
    } else {
        json!({ "data": data, "errors": ctx.errors })
    }
}

/// Execute a selection set against a runtime object of type `typename`.
fn exec_set(
    ctx: &mut Ctx,
    typename: &str,
    obj: &Json,
    set: &SelectionSet<'_, String>,
    path: &mut Vec<Json>,
) -> Json {
    let mut out = Map::new();
    exec_into(ctx, typename, obj, set, path, &mut out);
    Json::Object(out)
}

fn exec_into(
    ctx: &mut Ctx,
    typename: &str,
    obj: &Json,
    set: &SelectionSet<'_, String>,
    path: &mut Vec<Json>,
    out: &mut Map<String, Json>,
) {
    for item in &set.items {
        match item {
            Selection::Field(f) => {
                let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
                path.push(Json::String(key.clone()));
                let value = if f.name == "__typename" {
                    Json::String(typename.to_string())
                } else {
                    match resolve(ctx, typename, obj, f, path) {
                        Ok(v) => v,
                        Err(e) => {
                            let mut err = Map::new();
                            err.insert("message".into(), json!(e.message));
                            err.insert(
                                "locations".into(),
                                json!([{ "line": e.line, "column": e.column }]),
                            );
                            err.insert("path".into(), Json::Array(path.clone()));
                            ctx.errors.push(Json::Object(err));
                            Json::Null
                        }
                    }
                };
                path.pop();
                out.insert(key, value);
            }
            Selection::FragmentSpread(fs) => {
                if let Some(frag) = ctx.frags.get(fs.fragment_name.as_str()).copied() {
                    let graphql_parser::query::TypeCondition::On(cond) = &frag.type_condition;
                    if type_matches(cond, typename) {
                        exec_into(ctx, typename, obj, &frag.selection_set, path, out);
                    }
                }
            }
            Selection::InlineFragment(inf) => {
                let applies = match &inf.type_condition {
                    Some(graphql_parser::query::TypeCondition::On(cond)) => {
                        type_matches(cond, typename)
                    }
                    None => true,
                };
                if applies {
                    exec_into(ctx, typename, obj, &inf.selection_set, path, out);
                }
            }
        }
    }
}

/// Abstract-type conditions from the JS typeDefs: Message implements
/// Communication; Person implements Name; SearchResult = Photo | Person.
fn type_matches(cond: &str, typename: &str) -> bool {
    cond == typename
        || (cond == "Communication" && typename == "Message")
        || (cond == "Name" && typename == "Person")
        || (cond == "SearchResult" && (typename == "Photo" || typename == "Person"))
}

/// Resolve one field; ports the apollo resolver map.
fn resolve(
    ctx: &mut Ctx,
    typename: &str,
    obj: &Json,
    f: &Field<'_, String>,
    path: &mut Vec<Json>,
) -> Result<Json, FieldError> {
    let err = |message: &str| FieldError {
        message: message.to_string(),
        line: f.position.line,
        column: f.position.column,
    };
    match (typename, f.name.as_str()) {
        ("Query", "hello") => Ok(json!("world")),
        ("Query", "message") => {
            let id = arg(ctx, f, "id");
            let found = all_messages().iter().find(|m| num_eq(&m["id"], &id));
            match found {
                Some(m) => Ok(exec_set(ctx, "Message", &m.clone(), &f.selection_set, path)),
                None => Ok(Json::Null),
            }
        }
        ("Query", "messages") => {
            let wherev = arg(ctx, f, "where");
            let includes = arg(ctx, f, "includes");
            let rows = filter_messages(all_messages().to_vec(), &wherev, &includes)
                .map_err(|m| err(&m))?;
            Ok(map_rows(ctx, "Message", rows, &f.selection_set, path))
        }
        ("Query", "user") => {
            let user_id = arg(ctx, f, "user_id");
            let user = json!({ "user_id": user_id });
            Ok(exec_set(ctx, "User", &user, &f.selection_set, path))
        }
        ("Query", "users") => {
            let ids = arg(ctx, f, "user_ids");
            let rows: Vec<Json> = ids
                .as_array()
                .map(|xs| xs.iter().map(|v| json!({ "user_id": v })).collect())
                .unwrap_or_default();
            Ok(map_rows(ctx, "User", rows, &f.selection_set, path))
        }
        ("Query", "communications") => {
            let id = arg(ctx, f, "id");
            // JS: if(id) — 0/null are falsy.
            let rows: Vec<Json> = if truthy(&id) {
                all_messages()
                    .iter()
                    .filter(|m| num_eq(&m["id"], &id))
                    .cloned()
                    .collect()
            } else {
                all_messages().to_vec()
            };
            // __resolveType: communication.name present -> "Message".
            Ok(map_rows(ctx, "Message", rows, &f.selection_set, path))
        }
        ("Query", "profilePicture") => {
            let dimensions = arg(ctx, f, "dimensions");
            if dimensions.is_null() {
                Ok(Json::Null)
            } else {
                Ok(exec_set(ctx, "Photo", &dimensions, &f.selection_set, path))
            }
        }
        ("User", "user_id") => Ok(obj.get("user_id").cloned().unwrap_or(Json::Null)),
        ("User", "userMessages") => {
            let parent_id = obj.get("user_id").cloned().unwrap_or(Json::Null);
            let base: Vec<Json> = all_messages()
                .iter()
                .filter(|m| num_eq(&m["id"], &parent_id))
                .cloned()
                .collect();
            let whered = arg(ctx, f, "whered");
            let includes = arg(ctx, f, "includes");
            let rows = filter_messages(base, &whered, &includes).map_err(|m| err(&m))?;
            Ok(map_rows(ctx, "Message", rows, &f.selection_set, path))
        }
        ("User", "gimmeText") => {
            let text = arg(ctx, f, "text");
            // JS: if (text) — null and "" are falsy.
            match text {
                Json::String(s) if !s.is_empty() => Ok(Json::String(s)),
                _ => Ok(json!("no text")),
            }
        }
        ("Message", "errorMsg") => Err(err("intentional-error")),
        ("Message", name @ ("id" | "name" | "msg")) => {
            Ok(obj.get(name).cloned().unwrap_or(Json::Null))
        }
        ("Photo", name @ ("height" | "width")) => Ok(obj.get(name).cloned().unwrap_or(Json::Null)),
        _ => Ok(Json::Null),
    }
}

fn map_rows(
    ctx: &mut Ctx,
    typename: &str,
    rows: Vec<Json>,
    set: &SelectionSet<'_, String>,
    path: &mut Vec<Json>,
) -> Json {
    let mut out = vec![];
    for (i, row) in rows.iter().enumerate() {
        path.push(json!(i));
        out.push(exec_set(ctx, typename, row, set, path));
        path.pop();
    }
    Json::Array(out)
}

/// The shared where/includes filter from `messages` / `userMessages`.
/// Unknown operators throw ApolloError("invalid argument", "invalid").
fn filter_messages(
    mut rows: Vec<Json>,
    wherev: &Json,
    includes: &Json,
) -> Result<Vec<Json>, String> {
    if let Some(int_exp) = wherev
        .get("id")
        .filter(|v| truthy(v))
        .and_then(Json::as_object)
    {
        for (op, v) in int_exp {
            match op.as_str() {
                "eq" => rows.retain(|m| num_eq(&m["id"], v)),
                "gt" => rows.retain(|m| num_cmp(&m["id"], v) == Some(std::cmp::Ordering::Greater)),
                "lt" => rows.retain(|m| num_cmp(&m["id"], v) == Some(std::cmp::Ordering::Less)),
                _ => return Err("invalid argument".to_string()),
            }
        }
    }
    if let Some(str_exp) = wherev
        .get("name")
        .filter(|v| truthy(v))
        .and_then(Json::as_object)
    {
        for (op, v) in str_exp {
            match op.as_str() {
                "eq" => rows.retain(|m| &m["name"] == v),
                _ => return Err("invalid argument".to_string()),
            }
        }
    }
    if let Some(ids) = includes
        .get("id")
        .filter(|v| truthy(v))
        .and_then(Json::as_array)
    {
        rows.retain(|m| ids.iter().any(|v| num_eq(&m["id"], v)));
    }
    if let Some(names) = includes
        .get("name")
        .filter(|v| truthy(v))
        .and_then(Json::as_array)
    {
        rows.retain(|m| names.iter().any(|v| v == &m["name"]));
    }
    Ok(rows)
}

// ------------------------------------------------------------ values

/// Argument lookup with literal+variable resolution; missing -> null
/// (matching JS destructuring of absent args).
fn arg(ctx: &Ctx, f: &Field<'_, String>, name: &str) -> Json {
    f.arguments
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| value_to_json(v, &ctx.vars))
        .unwrap_or(Json::Null)
}

fn value_to_json(v: &QValue<'_, String>, vars: &Map<String, Json>) -> Json {
    match v {
        QValue::Variable(name) => vars.get(name).cloned().unwrap_or(Json::Null),
        QValue::Int(n) => json!(n.as_i64().unwrap_or(0)),
        QValue::Float(x) => json!(x),
        QValue::String(s) => Json::String(s.clone()),
        QValue::Boolean(b) => Json::Bool(*b),
        QValue::Null => Json::Null,
        QValue::Enum(e) => Json::String(e.clone()),
        QValue::List(items) => Json::Array(items.iter().map(|i| value_to_json(i, vars)).collect()),
        QValue::Object(map) => Json::Object(
            map.iter()
                .map(|(k, val)| (k.clone(), value_to_json(val, vars)))
                .collect(),
        ),
    }
}

/// JS truthiness for the values that can appear here.
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Json::String(s) => !s.is_empty(),
        Json::Array(_) | Json::Object(_) => true,
    }
}

/// JS `==` over numbers (1 == 1.0).
fn num_eq(a: &Json, b: &Json) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn num_cmp(a: &Json, b: &Json) -> Option<std::cmp::Ordering> {
    a.as_f64()
        .zip(b.as_f64())
        .and_then(|(x, y)| x.partial_cmp(&y))
}

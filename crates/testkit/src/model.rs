//! The shape of a `*_test.yaml` file.
//!
//! ```yaml
//! tests:
//!   - name: a shopper checks out and the process authorizes the order
//!     steps:
//!       - providers:
//!           /v1/payment-authorizations: { status: authorized, authorization_id: auth_1 }
//!       - sql: insert into cart (customer_id) values ('customer-1')
//!       - as: { role: customer, user: customer-1 }
//!       - graphql: 'mutation { start_checkout(cart_id: 1, request_id: "…") { cart_id } }'
//!         expect: { data: { start_checkout: { cart_id: 1 } } }
//!       - await: { terminal: checkout_payment, expect: { payment_status: authorized } }
//!       - calls: { path: /v1/payment-authorizations, count: 1 }
//! ```
//!
//! A step is a mapping, and the key that names its kind is the one a reader
//! would look for first. Steps are parsed one at a time, after `${name}`
//! references to values captured by earlier steps are substituted, so the
//! file is loaded through the fixture loader (`!include` works) and each
//! step's shape is checked when it runs.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestFile {
    /// Constants every step may reference as `${name}`: the request ids,
    /// the user ids, the fixed values a file repeats.
    #[serde(default)]
    pub vars: BTreeMap<String, Json>,
    pub tests: Vec<TestCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    pub name: String,
    /// Raw steps; see [`Step::parse`].
    pub steps: Vec<Json>,
}

/// The kinds of step, each named by the key that identifies it.
const STEP_KEYS: &[&str] = &[
    "for",
    "url",
    "sql",
    "as",
    "graphql",
    "providers",
    "hold",
    "release",
    "await",
    "calls",
];

#[derive(Debug)]
pub enum Step {
    /// A request in the conformance fixture shape: `url`, `method` (POST),
    /// `status` (200), `headers`, `query` | `body`, `response` |
    /// `allowed_responses`. The response is compared exactly, as a fixture is.
    Http(Json),
    /// SQL on the stand database. Without `expect` or `error` it is a seed
    /// that must succeed.
    Sql(SqlStep),
    /// The actor every later `graphql` step runs as.
    As(AsStep),
    /// A GraphQL operation as the current actor. `expect` is a subset match
    /// over the whole response body; without it the response must carry no
    /// `errors`.
    Graphql(GraphqlStep),
    /// Answers for the provider stub, by request path (`*` matches one
    /// segment). A mapping is the default answer, `200` with that body; a
    /// list is a queue of `{status, body}` answers consumed in order before
    /// the default applies. Allowed at any point in a test.
    Providers(BTreeMap<String, ProviderAnswer>),
    /// Wait for durable state — a process reaching a terminal status, a
    /// table receiving a row — polling the database, never the clock.
    Await(AwaitStep),
    /// What the provider stub recorded for a path.
    Calls(CallsStep),
    /// Hold every request to a provider path unanswered until `release`, so
    /// a test can act while the engine is mid-call.
    Hold(String),
    /// Answer the requests held on a path, with whatever `providers` now says.
    Release(String),
    /// A table-driven step: run `do` once per item, with the item bound as
    /// `${item}` (and `${item.field}`). This is the whole of what the format
    /// borrows from a programming language — a list of examples over the same
    /// steps, as a Go table test — and it does not nest: a `for` inside a
    /// `for` is refused, and there is no condition, no expression and no
    /// loop over a computed value. A test that needs one of those is a
    /// test whose check belongs elsewhere: a decision table's `test_cases`,
    /// a validator, a CHECK constraint.
    For { items: Vec<Json>, steps: Vec<Json> },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlStep {
    pub sql: String,
    /// The rows the statement returns, as JSON objects keyed by column,
    /// compared with `subset_matches`: list the columns that matter, and
    /// exactly as many rows as there are.
    #[serde(default)]
    pub expect: Option<Vec<Json>>,
    /// The statement must fail with this error class.
    #[serde(default)]
    pub error: Option<SqlError>,
    /// Name → column of the first row, for `${name}` in later steps.
    #[serde(default)]
    pub capture: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlError {
    CheckViolation,
    UniqueViolation,
    ForeignKeyViolation,
    NotNullViolation,
    RaiseException,
}

impl SqlError {
    pub fn sqlstate(self) -> &'static str {
        match self {
            SqlError::CheckViolation => "23514",
            SqlError::UniqueViolation => "23505",
            SqlError::ForeignKeyViolation => "23503",
            SqlError::NotNullViolation => "23502",
            SqlError::RaiseException => "P0001",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsStep {
    #[serde(rename = "as")]
    pub actor: Actor,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub role: String,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphqlStep {
    pub graphql: String,
    #[serde(default)]
    pub variables: Option<Json>,
    #[serde(default)]
    pub expect: Option<Json>,
    /// Name → JSON pointer into the response body.
    #[serde(default)]
    pub capture: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ProviderAnswer {
    Queue(Vec<ScriptedAnswer>),
    Default(Json),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptedAnswer {
    #[serde(default = "ok")]
    pub status: u16,
    #[serde(default)]
    pub body: Json,
}

fn ok() -> u16 {
    200
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AwaitStep {
    #[serde(rename = "await")]
    pub what: Await,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Await {
    /// A process (by name) whose instance reaches the `terminal` status;
    /// `expect` and `capture` then apply to its terminal output.
    Terminal {
        terminal: String,
        #[serde(default)]
        expect: Option<Json>,
        #[serde(default)]
        capture: BTreeMap<String, String>,
    },
    /// A query whose rows match `expect`, polled until they do. The same
    /// shape as a `sql` step, and the difference is the whole point: a `sql`
    /// step asks what is true now, and this one waits for durable work to make
    /// it true — a delivery reaching a status, a second row arriving, a claim
    /// being taken. `row` is the special case of "any first row"; this is what
    /// a test uses when several instances of one Process are in flight and the
    /// row it is waiting for is named by a value, not by being first.
    Rows {
        sql: String,
        expect: Vec<Json>,
        #[serde(default)]
        capture: BTreeMap<String, String>,
    },
    /// A table receiving its first row; `capture` names its columns.
    Row {
        row: String,
        #[serde(default)]
        capture: BTreeMap<String, String>,
    },
    /// A process (by name) whose instance ends in its declared fail
    /// terminal; `expect` then applies to the journal's `failure_json`
    /// (`{kind, code, message}`).
    Failed {
        failed: String,
        #[serde(default)]
        expect: Option<Json>,
    },
    /// A process's wait becoming receptive to a signal. Signals are never
    /// buffered: one sent before the wait's timer event exists is audited as
    /// unexpected rather than matched late, so a test that sends one waits
    /// for the timer, not merely for the state name.
    Receptive { receptive: String, state: String },
    /// The engine's request to a held provider path arriving.
    Held { held: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallsStep {
    pub calls: Calls,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calls {
    pub path: String,
    /// How many calls the stub recorded for the path.
    #[serde(default)]
    pub count: Option<usize>,
    /// Which recorded call `body` and `headers` describe.
    #[serde(default)]
    pub index: usize,
    /// Subset match over the call's JSON body.
    #[serde(default)]
    pub body: Option<Json>,
    /// Header name (case-insensitive) → expected value; matchers
    /// (`@present`, `@regex …`) apply as in any `expect`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl Step {
    /// The key that names a step's kind, for a failure message.
    pub fn kind_of(raw: &Json) -> &'static str {
        raw.as_object()
            .and_then(|m| STEP_KEYS.iter().find(|k| m.contains_key(**k)))
            .copied()
            .unwrap_or("step")
    }

    pub fn parse(raw: Json) -> Result<Self> {
        let Some(map) = raw.as_object() else {
            return Err(anyhow!("a step is a mapping"));
        };
        let has = |k: &str| map.contains_key(k);
        if has("for") {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Wrapper {
                #[serde(rename = "for")]
                items: Vec<Json>,
                #[serde(rename = "do")]
                steps: Vec<Json>,
            }
            let w: Wrapper = serde_json::from_value(raw)?;
            if w.steps.iter().any(|s| s.get("for").is_some()) {
                return Err(anyhow!("a `for` does not nest"));
            }
            return Ok(Step::For {
                items: w.items,
                steps: w.steps,
            });
        }
        if has("url") {
            return Ok(Step::Http(raw));
        }
        if has("sql") {
            return Ok(Step::Sql(serde_json::from_value(raw)?));
        }
        if has("as") {
            return Ok(Step::As(serde_json::from_value(raw)?));
        }
        if has("graphql") {
            return Ok(Step::Graphql(serde_json::from_value(raw)?));
        }
        if has("providers") {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Wrapper {
                providers: BTreeMap<String, ProviderAnswer>,
            }
            let w: Wrapper = serde_json::from_value(raw)?;
            return Ok(Step::Providers(w.providers));
        }
        if has("hold") || has("release") {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Wrapper {
                hold: Option<String>,
                release: Option<String>,
            }
            let w: Wrapper = serde_json::from_value(raw)?;
            return match (w.hold, w.release) {
                (Some(path), None) => Ok(Step::Hold(path)),
                (None, Some(path)) => Ok(Step::Release(path)),
                _ => Err(anyhow!("a step is either `hold` or `release`")),
            };
        }
        if has("await") {
            // An untagged enum says only "data did not match any variant",
            // naming neither the key nor the line. An await is the step most
            // often written from memory, so the shapes are named here instead.
            const AWAIT_KINDS: &[&str] = &["terminal", "row", "sql", "failed", "receptive", "held"];
            let what = map.get("await").and_then(Json::as_object);
            if let Some(what) = what
                && !AWAIT_KINDS.iter().any(|k| what.contains_key(*k))
            {
                return Err(anyhow!(
                    "an await names one of {}; this one names {}",
                    AWAIT_KINDS.join(", "),
                    what.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            return Ok(Step::Await(serde_json::from_value(raw).map_err(
                |error| {
                    anyhow!(
                        "an await of this shape is not one the runner knows \
                     (terminal/failed take `expect` and `capture`, row takes \
                     `capture`, sql takes `expect` and `capture`, receptive \
                     takes `state`): {error}"
                    )
                },
            )?));
        }
        if has("calls") {
            return Ok(Step::Calls(serde_json::from_value(raw)?));
        }
        Err(anyhow!(
            "a step needs one of {}; this one has keys [{}]",
            STEP_KEYS
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", "),
            map.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    }
}

/// Replace `${name}` in every string of `value` with the captured value.
/// A string that is exactly one reference becomes the captured value itself,
/// whatever its type — a number captured from a row stays a number in a
/// provider body. Inside a longer string a captured string is spliced as is
/// and anything else as its JSON text.
pub fn substitute(value: &Json, vars: &BTreeMap<String, Json>) -> Result<Json> {
    Ok(match value {
        Json::String(s)
            if s.starts_with("${") && s.ends_with('}') && s.matches("${").count() == 1 =>
        {
            lookup(&s[2..s.len() - 1], vars)?.clone()
        }
        Json::String(s) if s.contains("${") => Json::String(substitute_str(s, vars)?),
        Json::Array(xs) => Json::Array(
            xs.iter()
                .map(|x| substitute(x, vars))
                .collect::<Result<_>>()?,
        ),
        Json::Object(m) => Json::Object(
            m.iter()
                .map(|(k, v)| substitute(v, vars).map(|v| (k.clone(), v)))
                .collect::<Result<_>>()?,
        ),
        other => other.clone(),
    })
}

fn substitute_str(s: &str, vars: &BTreeMap<String, Json>) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow!("unterminated `${{` in {s:?}"))?;
        let name = &after[..end];
        let value = lookup(name, vars)?;
        match value {
            Json::String(v) => out.push_str(v),
            other => out.push_str(&other.to_string()),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `name` or `name.field.inner`: a captured or declared value, or a field of
/// one — the way a `for` item's columns are reached.
fn lookup<'a>(path: &str, vars: &'a BTreeMap<String, Json>) -> Result<&'a Json> {
    let (name, rest) = path.split_once('.').unwrap_or((path, ""));
    let root = vars.get(name).ok_or_else(|| {
        anyhow!("`${{{path}}}`: `{name}` is neither declared in `vars` nor captured")
    })?;
    if rest.is_empty() {
        return Ok(root);
    }
    let pointer = format!("/{}", rest.replace('.', "/"));
    root.pointer(&pointer)
        .ok_or_else(|| anyhow!("`${{{path}}}`: no `{rest}` in {root}"))
}

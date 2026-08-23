//! The shape of a `*_test.yaml` file.
//!
//! ```yaml
//! tests:
//!   - name: a customer creates a cart
//!     steps:
//!       - sql: insert into customer (id) values ('customer-1')
//!       - url: /v1/graphql
//!         headers: { X-Donat-Role: customer, X-Donat-User-Id: customer-1 }
//!         query: { query: "mutation { insert_cart(objects: [{}]) { affected_rows } }" }
//!         response: { data: { insert_cart: { affected_rows: 1 } } }
//! ```
//!
//! A step is a mapping, and the key that names its kind is the one a reader
//! would look for first: `url` is a request in the conformance fixture shape,
//! `sql` is a statement on the stand database. The file is loaded through the
//! fixture loader, so `!include` works here as it does in a fixture.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestFile {
    pub tests: Vec<TestCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    pub name: String,
    pub steps: Vec<Step>,
}

#[derive(Debug)]
pub enum Step {
    /// A request in the conformance fixture shape: `url`, `method` (POST),
    /// `status` (200), `headers`, `query` | `body`, `response` |
    /// `allowed_responses`. The response is compared exactly, as a fixture is.
    Http(Json),
    /// SQL on the stand database. Without `expect` or `error` it is a seed
    /// that must succeed.
    Sql(SqlStep),
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

impl Step {
    pub fn kind(&self) -> &'static str {
        match self {
            Step::Http(_) => "request",
            Step::Sql(_) => "sql",
        }
    }
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, Json>::deserialize(deserializer)?;
        Step::from_map(raw).map_err(serde::de::Error::custom)
    }
}

impl Step {
    fn from_map(raw: BTreeMap<String, Json>) -> Result<Self> {
        let keys = raw.keys().cloned().collect::<Vec<_>>();
        let to_json = |raw: BTreeMap<String, Json>| Json::Object(raw.into_iter().collect());
        if raw.contains_key("url") {
            return Ok(Step::Http(to_json(raw)));
        }
        if raw.contains_key("sql") {
            return Ok(Step::Sql(serde_json::from_value(to_json(raw))?));
        }
        Err(anyhow!(
            "a step needs one of `url`, `sql`; this one has keys [{}]",
            keys.join(", ")
        ))
    }
}

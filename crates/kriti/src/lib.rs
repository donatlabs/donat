//! Kriti — the template language Donat v2 request and response transforms are
//! written in.
//!
//! A transform is what lets a GraphQL Action stand in front of an HTTP API
//! that was never written for this engine: the action declares a method, a URL
//! and a body as *templates*, and each one is evaluated against the invocation
//! before the request goes out. Donat's transforms are Kriti templates, so
//! metadata exported from an existing project has to keep meaning what it
//! meant — which is why this is a port of the language rather than a smaller
//! one of our own.
//!
//! A Kriti template is **JSON with holes**, not text with holes. `{{ … }}` in
//! a value position produces a value, an object stays an object, and the
//! result is a JSON document rather than a string that has to parse as one:
//!
//! ```text
//! { "name": "{{ $body.input.name }}", "tags": {{ range _, t := $body.input.tags }} {{ t }} {{ end }} }
//! ```
//!
//! The language is small and completely specified by its own test corpus,
//! which this crate is verified against (`tests/data`, Apache-2.0, from
//! <https://github.com/hasura/kriti-lang>):
//!
//! - **paths** — `$`, `$body.input.id`, `$[0]`, `$['a key']`
//! - **optional paths** — `$body?.foo`, `$?[3]`, which short-circuit to `null`
//!   instead of failing, and take the rest of the chain with them
//! - **defaulting** — `a ?? b`, which replaces `null`
//! - **conditionals** — `if` / `elif` / `else`, with `&&`, `||`, `not`, `in`,
//!   and the six comparisons
//! - **loops** — `range i, x := …`, which evaluates to an array
//! - **string interpolation** — `"user_{{ $body.id }}"`, where a value that is
//!   not a string arrives as its compact JSON
//! - **functions** — the standard collection, in [`functions`]
//!
//! What it deliberately has no notion of is assignment, recursion or IO: a
//! template can only rearrange what it was given.

mod eval;
mod parser;

pub mod functions;

pub use eval::{EvalError, Value};
pub use parser::{Node, ParseError};

use serde_json::{Map, Value as Json};

/// A parsed template, ready to be evaluated any number of times.
#[derive(Debug, Clone)]
pub struct Template {
    root: Node,
}

impl Template {
    /// Parse a template, so a declaration can be refused when it is written
    /// rather than when it is first called.
    pub fn parse(source: &str) -> Result<Self, ParseError> {
        Ok(Self {
            root: parser::parse(source)?,
        })
    }

    /// Evaluate against a context — `$body`, `$base_url`, and whatever else
    /// the caller binds.
    pub fn render(&self, context: &Map<String, Json>) -> Result<Json, EvalError> {
        eval::eval(&self.root, context)
    }
}

/// Parse and evaluate in one step.
pub fn render(source: &str, context: &Map<String, Json>) -> Result<Json, Error> {
    Template::parse(source)?
        .render(context)
        .map_err(Error::Eval)
}

/// Evaluate a template that stands where a string is expected — a URL, a
/// header value, one query parameter.
///
/// Such a template is written without quotes (`{{$base_url}}/users`), so it is
/// quoted and then evaluated as an ordinary string literal. That is Donat's
/// own `wrapUnescapedTemplate`, and it is the reason a bare template can hold
/// text around the holes at all: the parser would otherwise read `/users` as
/// input trailing a finished document.
pub fn render_unescaped(source: &str, context: &Map<String, Json>) -> Result<String, Error> {
    match render(&format!("\"{source}\""), context)? {
        Json::String(text) => Ok(text),
        other => Ok(other.to_string()),
    }
}

#[derive(Debug)]
pub enum Error {
    Parse(ParseError),
    Eval(EvalError),
}

impl From<ParseError> for Error {
    fn from(error: ParseError) -> Self {
        Error::Parse(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(error) => write!(f, "{error}"),
            Error::Eval(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

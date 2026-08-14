//! Evaluating a template against a context.
//!
//! Two rules carry most of the language's character and are worth stating
//! plainly, because both are places where a reasonable implementation would
//! differ and a template would then quietly mean something else.
//!
//! **A missing value is an error unless it was asked for optionally.** `$a.b`
//! fails when `b` is absent; `$a?.b` is `null`. And an optional lookup that
//! came back `null` takes the rest of the chain with it — `$a?.b.c.d` is
//! `null` rather than "cannot index into null".
//!
//! **Interpolation stringifies.** Inside a string literal, a hole holding a
//! string contributes its characters and a hole holding anything else
//! contributes its compact JSON — so `"{{ $a }}"` where `$a` is an object is a
//! string containing `{"…":…}`, not an object.

use serde_json::{Map, Value as Json};

use crate::functions;
use crate::parser::{BinOp, Expr, Node, Step, StepKind, StrPart};

#[derive(Debug, Clone)]
pub struct EvalError {
    pub message: String,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EvalError {}

fn fail<T>(message: impl Into<String>) -> Result<T, EvalError> {
    Err(EvalError {
        message: message.into(),
    })
}

/// What a template evaluates to.
pub type Value = Json;

/// The bindings in scope: the caller's context, plus whatever `range` bound
/// around the expression being evaluated.
struct Scope<'a> {
    context: &'a Map<String, Json>,
    locals: Vec<(String, Json)>,
}

impl Scope<'_> {
    fn get(&self, name: &str) -> Option<&Json> {
        self.locals
            .iter()
            .rev()
            .find(|(bound, _)| bound == name)
            .map(|(_, value)| value)
            .or_else(|| self.context.get(name))
    }
}

pub fn eval(node: &Node, context: &Map<String, Json>) -> Result<Json, EvalError> {
    let mut scope = Scope {
        context,
        locals: Vec::new(),
    };
    eval_node(node, &mut scope)
}

fn eval_node(node: &Node, scope: &mut Scope<'_>) -> Result<Json, EvalError> {
    match node {
        Node::Json(value) => Ok(value.clone()),
        Node::Str(parts) => Ok(Json::String(eval_string(parts, scope)?)),
        Node::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(eval_node(item, scope)?);
            }
            Ok(Json::Array(out))
        }
        Node::Object(members) => {
            let mut out = Map::new();
            for (key, value) in members {
                out.insert(eval_string(key, scope)?, eval_node(value, scope)?);
            }
            Ok(Json::Object(out))
        }
        Node::Expr(expr) => eval_expr(expr, scope),
        Node::If { arms, otherwise } => {
            for (condition, body) in arms {
                if truthy(&eval_expr(condition, scope)?)? {
                    return eval_node(body, scope);
                }
            }
            match otherwise {
                Some(body) => eval_node(body, scope),
                // An `if` with nothing to fall back on contributes nothing.
                None => Ok(Json::Null),
            }
        }
        Node::Range {
            index,
            value,
            source,
            body,
        } => {
            let items = match eval_expr(source, scope)? {
                Json::Array(items) => items,
                other => {
                    return fail(format!("range expects an array, got {}", type_name(&other)));
                }
            };
            let mut out = Vec::with_capacity(items.len());
            for (position, item) in items.into_iter().enumerate() {
                let depth = scope.locals.len();
                if let Some(index) = index {
                    scope.locals.push((index.clone(), Json::from(position)));
                }
                scope.locals.push((value.clone(), item));
                let evaluated = eval_node(body, scope);
                scope.locals.truncate(depth);
                out.push(evaluated?);
            }
            Ok(Json::Array(out))
        }
    }
}

fn eval_string(parts: &[StrPart], scope: &mut Scope<'_>) -> Result<String, EvalError> {
    let mut out = String::new();
    for part in parts {
        match part {
            StrPart::Text(text) => out.push_str(text),
            StrPart::Hole(expr) => match eval_expr(expr, scope)? {
                Json::String(text) => out.push_str(&text),
                other => out.push_str(&other.to_string()),
            },
        }
    }
    Ok(out)
}

fn eval_expr(expr: &Expr, scope: &mut Scope<'_>) -> Result<Json, EvalError> {
    match expr {
        Expr::Lit(value) => Ok(value.clone()),
        Expr::Str(parts) => Ok(Json::String(eval_string(parts, scope)?)),
        Expr::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(eval_expr(item, scope)?);
            }
            Ok(Json::Array(out))
        }
        Expr::Object(members) => {
            let mut out = Map::new();
            for (key, value) in members {
                out.insert(eval_string(key, scope)?, eval_expr(value, scope)?);
            }
            Ok(Json::Object(out))
        }
        Expr::Var { name, optional } => match scope.get(name) {
            Some(value) => Ok(value.clone()),
            None if *optional => Ok(Json::Null),
            None => fail(format!("{name} is not bound")),
        },
        Expr::Path { base, steps } => {
            let mut current = eval_expr(base, scope)?;
            let mut short_circuited = false;
            for step in steps {
                if short_circuited {
                    break;
                }
                match apply_step(&current, step)? {
                    Lookup::Found(value) => current = value,
                    Lookup::Missing => {
                        current = Json::Null;
                        short_circuited = true;
                    }
                }
            }
            Ok(current)
        }
        Expr::Not(inner) => Ok(Json::Bool(!truthy(&eval_expr(inner, scope)?)?)),
        Expr::Call { name, args } => {
            if args.len() != 1 {
                return fail(format!("{name} takes exactly one argument"));
            }
            let argument = eval_expr(&args[0], scope)?;
            functions::call(name, argument).map_err(|message| EvalError { message })
        }
        Expr::Template(node) => eval_node(node, scope),
        Expr::Binary { op, lhs, rhs } => eval_binary(*op, lhs, rhs, scope),
    }
}

fn eval_binary(
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    scope: &mut Scope<'_>,
) -> Result<Json, EvalError> {
    // `??` and the two boolean operators decide whether to look at the right
    // side at all, so they are evaluated before it is.
    if op == BinOp::Default {
        let left = eval_expr(lhs, scope)?;
        return if left.is_null() {
            eval_expr(rhs, scope)
        } else {
            Ok(left)
        };
    }
    let left = eval_expr(lhs, scope)?;
    if op == BinOp::And && !truthy(&left)? {
        return Ok(Json::Bool(false));
    }
    if op == BinOp::Or && truthy(&left)? {
        return Ok(Json::Bool(true));
    }
    let right = eval_expr(rhs, scope)?;

    Ok(match op {
        BinOp::Default => unreachable!("handled above"),
        BinOp::And | BinOp::Or => Json::Bool(truthy(&right)?),
        BinOp::Eq => Json::Bool(left == right),
        BinOp::Ne => Json::Bool(left != right),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ordering = compare(&left, &right)?;
            Json::Bool(match op {
                BinOp::Lt => ordering.is_lt(),
                BinOp::Le => ordering.is_le(),
                BinOp::Gt => ordering.is_gt(),
                _ => ordering.is_ge(),
            })
        }
        BinOp::In => Json::Bool(match (&left, &right) {
            (Json::String(key), Json::Object(map)) => map.contains_key(key.as_str()),
            (needle, Json::Array(items)) => items.contains(needle),
            _ => return fail("'in' expects an object with a string key, or an array"),
        }),
    })
}

fn compare(left: &Json, right: &Json) -> Result<std::cmp::Ordering, EvalError> {
    match (left, right) {
        (Json::Number(a), Json::Number(b)) => {
            let (a, b) = (a.as_f64(), b.as_f64());
            match (a, b) {
                (Some(a), Some(b)) => a.partial_cmp(&b).ok_or_else(|| EvalError {
                    message: "cannot order these numbers".to_string(),
                }),
                _ => fail("cannot order these numbers"),
            }
        }
        (Json::String(a), Json::String(b)) => Ok(a.cmp(b)),
        _ => fail(format!(
            "cannot compare {} with {}",
            type_name(left),
            type_name(right)
        )),
    }
}

enum Lookup {
    Found(Json),
    /// Absent, and asked for optionally.
    Missing,
}

fn apply_step(value: &Json, step: &Step) -> Result<Lookup, EvalError> {
    let found = match (&step.kind, value) {
        (StepKind::Field(name), Json::Object(map)) => map.get(name.as_str()).cloned(),
        (StepKind::Key(name), Json::Object(map)) => map.get(name.as_str()).cloned(),
        (StepKind::Index(index), Json::Array(items)) => usize::try_from(*index)
            .ok()
            .and_then(|i| items.get(i))
            .cloned(),
        (_, _) if step.optional => None,
        (StepKind::Field(name) | StepKind::Key(name), other) => {
            return fail(format!("cannot look up '{name}' in {}", type_name(other)));
        }
        (StepKind::Index(index), other) => {
            return fail(format!("cannot index {} with {index}", type_name(other)));
        }
    };
    match (found, step.optional) {
        (Some(value), _) => Ok(Lookup::Found(value)),
        (None, true) => Ok(Lookup::Missing),
        (None, false) => match &step.kind {
            StepKind::Field(name) | StepKind::Key(name) => fail(format!("'{name}' is not bound")),
            StepKind::Index(index) => fail(format!("index {index} is out of range")),
        },
    }
}

fn truthy(value: &Json) -> Result<bool, EvalError> {
    match value {
        Json::Bool(flag) => Ok(*flag),
        other => fail(format!(
            "expected a boolean condition, got {}",
            type_name(other)
        )),
    }
}

fn type_name(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "an array",
        Json::Object(_) => "an object",
    }
}

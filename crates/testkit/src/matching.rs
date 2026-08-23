//! Response comparison faithful to tests-py `check_query_f`.

use serde_json::Value as Json;

/// Selection tree extracted from the fixture's GraphQL query: response-alias
/// -> nested selections (None for leaf fields). Used to replicate tests-py
/// `collapse_order_not_selset`: key order is enforced only among keys that
/// are part of the selection set; everything else (errors, jsonb column
/// values, ...) compares order-insensitively.
#[derive(Default)]
pub struct SelMap(std::collections::HashMap<String, Option<SelMap>>);

impl SelMap {
    pub fn contains_key(&self, k: &str) -> bool {
        self.0.contains_key(k)
    }
    pub fn get(&self, k: &str) -> Option<&Option<SelMap>> {
        self.0.get(k)
    }
}

pub fn sel_tree_from_query(query: &str) -> Option<SelMap> {
    use graphql_parser::query::{Definition, OperationDefinition, Selection, SelectionSet};

    let doc = graphql_parser::parse_query::<String>(query).ok()?;
    let mut frags = std::collections::HashMap::new();
    for def in &doc.definitions {
        if let Definition::Fragment(f) = def {
            frags.insert(f.name.clone(), &f.selection_set);
        }
    }
    fn build<'a>(
        ss: &SelectionSet<'a, String>,
        frags: &std::collections::HashMap<String, &SelectionSet<'a, String>>,
    ) -> SelMap {
        let mut out = SelMap::default();
        for item in &ss.items {
            match item {
                Selection::Field(f) => {
                    let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
                    let child = if f.selection_set.items.is_empty() {
                        None
                    } else {
                        Some(build(&f.selection_set, frags))
                    };
                    out.0.insert(key, child);
                }
                Selection::FragmentSpread(fs) => {
                    if let Some(inner) = frags.get(&fs.fragment_name) {
                        out.0.extend(build(inner, frags).0);
                    }
                }
                Selection::InlineFragment(inf) => {
                    out.0.extend(build(&inf.selection_set, frags).0);
                }
            }
        }
        out
    }
    for def in &doc.definitions {
        let ss = match def {
            Definition::Operation(OperationDefinition::Query(q)) => &q.selection_set,
            Definition::Operation(OperationDefinition::Mutation(m)) => &m.selection_set,
            Definition::Operation(OperationDefinition::Subscription(s)) => &s.selection_set,
            Definition::Operation(OperationDefinition::SelectionSet(ss)) => ss,
            Definition::Fragment(_) => continue,
        };
        return Some(build(ss, &frags));
    }
    None
}

/// Deep comparison. `sel` carries the selection tree for the current level;
/// among keys present in the tree, the relative order in expected and actual
/// must match, and their children recurse with their sub-tree. Keys outside
/// the tree (and everything once `sel` is None) compare order-insensitively.
/// Numbers compare by value (1 == 1.0), like Python.
pub fn json_matches(exp: &Json, act: &Json, sel: Option<&SelMap>) -> bool {
    match (exp, act) {
        (Json::Object(e), Json::Object(a)) => {
            if e.len() != a.len() || !e.keys().all(|k| a.contains_key(k)) {
                return false;
            }
            if let Some(tree) = sel {
                let eseq: Vec<&String> = e.keys().filter(|k| tree.contains_key(k)).collect();
                let aseq: Vec<&String> = a.keys().filter(|k| tree.contains_key(k)).collect();
                if eseq != aseq {
                    return false;
                }
            }
            e.iter().all(|(k, ve)| {
                let child = sel.and_then(|t| t.get(k)).and_then(|c| c.as_ref());
                json_matches(ve, &a[k], child)
            })
        }
        (Json::Array(e), Json::Array(a)) => {
            e.len() == a.len() && e.iter().zip(a.iter()).all(|(x, y)| json_matches(x, y, sel))
        }
        (Json::Number(e), Json::Number(a)) => {
            e == a || (e.as_f64().zip(a.as_f64()).is_some_and(|(x, y)| x == y))
        }
        _ => exp == act,
    }
}

/// Compare a full HTTP-level response: top-level object unordered, the
/// `data` subtree governed by the query's selection tree.
pub fn response_matches(exp: &Json, act: &Json, query_text: Option<&str>) -> bool {
    let tree = query_text.and_then(sel_tree_from_query);
    match (exp, act) {
        (Json::Object(e), Json::Object(a)) => {
            if e.len() != a.len() || !e.keys().all(|k| a.contains_key(k)) {
                return false;
            }
            e.iter().all(|(k, ve)| {
                let sel = if k == "data" { tree.as_ref() } else { None };
                json_matches(ve, &a[k], sel)
            })
        }
        _ => json_matches(exp, act, None),
    }
}

/// Normalize a JSON-RPC `result` for MCP comparison by dropping fields that
/// are not part of the conformance contract:
///
/// - `content` (always): a text duplicate of the structured data.
/// - `structuredContent` *only when* `isError` is true: an error tool result's
///   structured payload carries engine-dependent GraphQL error details, so the
///   contract for a failure is just `isError: true`. On success,
///   `structuredContent` (the real data) is kept and asserted.
///
/// Everything else (`isError`, `tools`, `protocolVersion`, `serverInfo`,
/// `capabilities`, ...) is asserted as-is. GraphQL/REST comparison never calls
/// this.
pub fn strip_mcp_content(v: &Json) -> Json {
    let mut out = v.clone();
    if let Some(result) = out.get_mut("result").and_then(Json::as_object_mut) {
        result.remove("content");
        if result.get("isError") == Some(&Json::Bool(true)) {
            result.remove("structuredContent");
        }
    }
    out
}

/// The comparison an application test's `expect` uses: the expected value
/// lists what must hold, and says nothing about the rest.
///
/// - an object matches when every expected key is present and matches;
///   keys the expectation does not mention are free;
/// - an array matches element-wise and must have the same length — a list
///   of one row is a claim about how many rows there are;
/// - the string `"@any"` matches any value, null included;
/// - the string `"@present"` matches any value except null;
/// - `"@uuid"`, `"@number"`, `"@string"`, `"@bool"` match by type;
/// - `"@gt N"`, `"@gte N"`, `"@lt N"`, `"@lte N"` compare a number;
/// - `"@regex R"` matches a string against an anchored-as-written regex;
/// - `"@len N"` matches an array or string of that length;
/// - numbers compare by value (`1 == 1.0`); everything else by equality.
///
/// Conformance fixtures keep the exact [`response_matches`]: they pin a
/// contract byte for byte. An application test asserts a behaviour, and a
/// column added tomorrow should not fail it.
fn matcher(spec: &str, act: &Json) -> bool {
    let (name, arg) = spec.split_once(' ').unwrap_or((spec, ""));
    let arg = arg.trim();
    let number = || act.as_f64();
    let bound = || arg.parse::<f64>().ok();
    match name {
        "@any" => true,
        "@present" => !act.is_null(),
        "@uuid" => act.as_str().is_some_and(|s| {
            s.len() == 36
                && s.bytes().enumerate().all(|(i, b)| {
                    if matches!(i, 8 | 13 | 18 | 23) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
        }),
        "@number" => act.is_number(),
        "@string" => act.is_string(),
        "@bool" => act.is_boolean(),
        "@gt" => number().zip(bound()).is_some_and(|(a, b)| a > b),
        "@gte" => number().zip(bound()).is_some_and(|(a, b)| a >= b),
        "@lt" => number().zip(bound()).is_some_and(|(a, b)| a < b),
        "@lte" => number().zip(bound()).is_some_and(|(a, b)| a <= b),
        "@regex" => act
            .as_str()
            .zip(regex::Regex::new(arg).ok())
            .is_some_and(|(s, re)| re.is_match(s)),
        "@len" => arg.parse::<usize>().ok().is_some_and(|n| match act {
            Json::Array(xs) => xs.len() == n,
            Json::String(s) => s.chars().count() == n,
            _ => false,
        }),
        // Not a matcher: a literal string that happens to start with `@`.
        _ => act.as_str() == Some(spec),
    }
}

pub fn subset_matches(exp: &Json, act: &Json) -> bool {
    match (exp, act) {
        (Json::String(s), _) if s.starts_with('@') => matcher(s, act),
        (Json::Object(e), Json::Object(a)) => e
            .iter()
            .all(|(k, ve)| a.get(k).is_some_and(|va| subset_matches(ve, va))),
        (Json::Array(e), Json::Array(a)) => {
            e.len() == a.len() && e.iter().zip(a).all(|(x, y)| subset_matches(x, y))
        }
        (Json::Number(e), Json::Number(a)) => {
            e == a || (e.as_f64().zip(a.as_f64()).is_some_and(|(x, y)| x == y))
        }
        _ => exp == act,
    }
}

//! The standard collection.
//!
//! Each of these is a port of the function of the same name in Kriti's own
//! `CustomFunctions`, including the parts that look arbitrary — `size` of a
//! number is that number, `inverse` of a number is its reciprocal, `empty` of
//! a boolean is an error rather than a guess. A template written against the
//! original has to keep meaning what it meant, and "obviously it should be…"
//! is how that stops being true.

use serde_json::{Map, Value as Json};

pub type FunctionError = String;

/// Call a function by name, or report that there is no such function.
pub fn call(name: &str, argument: Json) -> Result<Json, FunctionError> {
    match name {
        "empty" => empty(argument),
        "size" => size(argument),
        "inverse" => inverse(argument),
        "head" => head(argument),
        "tail" => tail(argument),
        "toCaseFold" => with_text(name, argument, |text| text.to_lowercase()),
        "toLower" => with_text(name, argument, |text| text.to_lowercase()),
        "toUpper" => with_text(name, argument, |text| text.to_uppercase()),
        "toTitle" => with_text(name, argument, |text| to_title(&text)),
        "escapeUri" => with_text(name, argument, |text| escape_uri(&text)),
        "fromPairs" => from_pairs(argument),
        "toPairs" => to_pairs(argument),
        "removeNulls" => remove_nulls(argument),
        "concat" => concat(argument),
        "not" => not(argument),
        other => Err(format!("no such function: {other}")),
    }
}

fn with_text(
    name: &str,
    argument: Json,
    f: impl FnOnce(String) -> String,
) -> Result<Json, FunctionError> {
    match argument {
        Json::String(text) => Ok(Json::String(f(text))),
        _ => Err(format!("{name} expects a string")),
    }
}

fn empty(argument: Json) -> Result<Json, FunctionError> {
    Ok(Json::Bool(match argument {
        Json::Object(map) => map.is_empty(),
        Json::Array(items) => items.is_empty(),
        Json::String(text) => text.trim().is_empty(),
        Json::Number(number) => number.as_f64() == Some(0.0),
        Json::Bool(_) => return Err("Cannot define emptiness for a boolean".to_string()),
        Json::Null => true,
    }))
}

fn size(argument: Json) -> Result<Json, FunctionError> {
    Ok(match argument {
        Json::Object(map) => Json::from(map.len()),
        Json::Array(items) => Json::from(items.len()),
        // Characters, not bytes: the original counts a `Text`'s length.
        Json::String(text) => Json::from(text.chars().count()),
        Json::Number(number) => Json::Number(number),
        Json::Bool(flag) => Json::from(u8::from(flag)),
        Json::Null => Json::from(0),
    })
}

fn inverse(argument: Json) -> Result<Json, FunctionError> {
    Ok(match argument {
        Json::Object(map) => Json::Object(map),
        Json::Array(mut items) => {
            items.reverse();
            Json::Array(items)
        }
        Json::String(text) => Json::String(text.chars().rev().collect()),
        Json::Number(number) => {
            let value = number.as_f64().unwrap_or(f64::NAN);
            serde_json::Number::from_f64(1.0 / value)
                .map(Json::Number)
                .unwrap_or(Json::Null)
        }
        Json::Bool(flag) => Json::Bool(!flag),
        Json::Null => Json::Null,
    })
}

fn head(argument: Json) -> Result<Json, FunctionError> {
    match argument {
        Json::Array(items) => items
            .into_iter()
            .next()
            .ok_or_else(|| "Empty array".to_string()),
        Json::String(text) => text
            .chars()
            .next()
            .map(|c| Json::String(c.to_string()))
            .ok_or_else(|| "Empty string".to_string()),
        _ => Err("Expected an array or string".to_string()),
    }
}

fn tail(argument: Json) -> Result<Json, FunctionError> {
    match argument {
        Json::Array(items) => {
            if items.is_empty() {
                return Err("Empty array".to_string());
            }
            Ok(Json::Array(items.into_iter().skip(1).collect()))
        }
        Json::String(text) => {
            if text.is_empty() {
                return Err("Empty string".to_string());
            }
            Ok(Json::String(text.chars().skip(1).collect()))
        }
        _ => Err("Expected an array or string".to_string()),
    }
}

/// Title case, one word at a time: the first letter of a word is upper, the
/// rest are lower — Haskell's `Data.Text.toTitle`.
fn to_title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_word_start = true;
    for character in text.chars() {
        if character.is_alphabetic() {
            if at_word_start {
                out.extend(character.to_uppercase());
            } else {
                out.extend(character.to_lowercase());
            }
            at_word_start = false;
        } else {
            out.push(character);
            at_word_start = true;
        }
    }
    out
}

/// Percent-encode everything outside RFC 3986's unreserved set, which is what
/// `escapeURIString isUnreserved` does.
fn escape_uri(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn from_pairs(argument: Json) -> Result<Json, FunctionError> {
    let shape = "Expected an array of shape [ [k1,v1], [k2,v2] ... ] - With String keys.";
    let Json::Array(pairs) = argument else {
        return Err("fromPairs expects an array".to_string());
    };
    let mut out = Map::new();
    for pair in pairs {
        let Json::Array(pair) = pair else {
            return Err(shape.to_string());
        };
        match pair.as_slice() {
            [Json::String(key), value] => {
                out.insert(key.clone(), value.clone());
            }
            _ => return Err(shape.to_string()),
        }
    }
    Ok(Json::Object(out))
}

fn to_pairs(argument: Json) -> Result<Json, FunctionError> {
    let Json::Object(map) = argument else {
        return Err("toPairs expects an object".to_string());
    };
    Ok(Json::Array(
        map.into_iter()
            .map(|(key, value)| Json::Array(vec![Json::String(key), value]))
            .collect(),
    ))
}

fn remove_nulls(argument: Json) -> Result<Json, FunctionError> {
    let Json::Array(items) = argument else {
        return Err("removeNulls expects an array".to_string());
    };
    Ok(Json::Array(
        items.into_iter().filter(|item| !item.is_null()).collect(),
    ))
}

/// Arrays, strings or objects — whichever the whole list is. On objects, a
/// later key wins, which is what folding the reversed list does in the
/// original.
fn concat(argument: Json) -> Result<Json, FunctionError> {
    let Json::Array(items) = argument else {
        return Err("concat expects an array".to_string());
    };
    if items.iter().all(Json::is_array) {
        let mut out = Vec::new();
        for item in items {
            if let Json::Array(inner) = item {
                out.extend(inner);
            }
        }
        return Ok(Json::Array(out));
    }
    if items.iter().all(Json::is_string) {
        let mut out = String::new();
        for item in items {
            if let Json::String(text) = item {
                out.push_str(&text);
            }
        }
        return Ok(Json::String(out));
    }
    if items.iter().all(Json::is_object) {
        let mut out = Map::new();
        for item in items {
            if let Json::Object(inner) = item {
                for (key, value) in inner {
                    out.insert(key, value);
                }
            }
        }
        return Ok(Json::Object(out));
    }
    Err("concat expects an array of arrays, of strings, or of objects".to_string())
}

fn not(argument: Json) -> Result<Json, FunctionError> {
    match argument {
        Json::Bool(flag) => Ok(Json::Bool(!flag)),
        _ => Err("not expects a boolean".to_string()),
    }
}

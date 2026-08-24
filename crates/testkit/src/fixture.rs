//! YAML fixture loading with `!include`.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value as Json, json};

/// Load a fixture YAML into JSON, resolving `!include <file>` (both the real
/// YAML tag and the quoted-string spelling donat-cli produces) relative to
/// the including file.
pub fn load_fixture(path: &Path) -> Result<Json> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading fixture {}", path.display()))?;
    let v: serde_yaml::Value = serde_yaml::from_str(&text)
        .with_context(|| format!("parsing fixture {}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    yaml_to_json(&v, dir)
}

fn yaml_to_json(v: &serde_yaml::Value, dir: &Path) -> Result<Json> {
    use serde_yaml::Value as Y;
    Ok(match v {
        Y::Null => Json::Null,
        Y::Bool(b) => Json::Bool(*b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!(i)
            } else if let Some(u) = n.as_u64() {
                json!(u)
            } else {
                json!(n.as_f64().unwrap())
            }
        }
        Y::String(s) => {
            if let Some(rest) = s.strip_prefix("!include ") {
                load_fixture(&dir.join(rest.trim()))?
            } else {
                Json::String(s.clone())
            }
        }
        Y::Sequence(xs) => Json::Array(
            xs.iter()
                .map(|x| yaml_to_json(x, dir))
                .collect::<Result<_>>()?,
        ),
        Y::Mapping(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                let key = match k {
                    Y::String(s) => s.clone(),
                    other => serde_yaml::to_string(other)?.trim().to_string(),
                };
                out.insert(key, yaml_to_json(val, dir)?);
            }
            Json::Object(out)
        }
        Y::Tagged(t) => {
            if t.tag.to_string().trim_start_matches('!') == "include" {
                let f = t
                    .value
                    .as_str()
                    .ok_or_else(|| anyhow!("!include expects a string"))?;
                load_fixture(&dir.join(f))?
            } else {
                yaml_to_json(&t.value, dir)?
            }
        }
    })
}

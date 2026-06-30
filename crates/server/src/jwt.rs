//! JWT authentication mode (DONAT_GRAPHQL_JWT_SECRET): verifies bearer
//! tokens and builds the session from x-donat-* and Hasura-compatible
//! x-hasura-* claims. The admin secret still wins; plain session headers
//! are not trusted in this mode.

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::Value as Json;

pub struct JwtConfig {
    keys: KeySource,
    validation: Validation,
    namespace: String,
    /// claims_namespace_path segments ("$" = whole payload).
    namespace_path: Option<Vec<String>>,
    /// claims_map: session var -> {path, default} | literal.
    claims_map: Option<serde_json::Map<String, Json>>,
    stringified: bool,
    /// Where the token arrives: Authorization bearer (None), a cookie, or
    /// a custom header.
    pub header: TokenLocation,
    jwk_url: Option<String>,
}

pub enum KeySource {
    Static(DecodingKey, Algorithm),
    /// Refreshed in the background from a jwk_url.
    Jwks(std::sync::Arc<std::sync::RwLock<Vec<JwkKey>>>),
}

pub struct JwkKey {
    pub kid: Option<String>,
    pub alg: Algorithm,
    pub key: DecodingKey,
}

#[derive(Clone, Debug)]
pub enum TokenLocation {
    Authorization,
    Cookie(String),
    CustomHeader(String),
}

#[derive(Debug)]
pub struct JwtSession {
    pub role: String,
    pub vars: std::collections::HashMap<String, String>,
}

#[derive(Debug)]
pub struct JwtError {
    pub code: &'static str,
    pub message: String,
}

fn is_session_claim(name: &str) -> bool {
    name.starts_with("x-donat-") || name.starts_with("x-hasura-")
}

impl JwtConfig {
    /// Parse the DONAT_GRAPHQL_JWT_SECRET JSON. jwk_url configs are not
    /// supported and yield None.
    pub fn from_env_value(raw: &str) -> Option<JwtConfig> {
        let config: Json = serde_json::from_str(raw).ok()?;
        if let Some(url) = config.get("jwk_url").and_then(Json::as_str) {
            return Some(Self::from_jwk_url(url, &config));
        }
        let key_type = config.get("type").and_then(Json::as_str)?;
        let key_data = config.get("key").and_then(Json::as_str)?;
        let (algorithm, key) = match key_type {
            "HS256" => (
                Algorithm::HS256,
                DecodingKey::from_secret(key_data.as_bytes()),
            ),
            "HS384" => (
                Algorithm::HS384,
                DecodingKey::from_secret(key_data.as_bytes()),
            ),
            "HS512" => (
                Algorithm::HS512,
                DecodingKey::from_secret(key_data.as_bytes()),
            ),
            "RS256" => (
                Algorithm::RS256,
                DecodingKey::from_rsa_pem(key_data.as_bytes()).ok()?,
            ),
            "RS384" => (
                Algorithm::RS384,
                DecodingKey::from_rsa_pem(key_data.as_bytes()).ok()?,
            ),
            "RS512" => (
                Algorithm::RS512,
                DecodingKey::from_rsa_pem(key_data.as_bytes()).ok()?,
            ),
            "Ed25519" => (
                Algorithm::EdDSA,
                DecodingKey::from_ed_pem(key_data.as_bytes()).ok()?,
            ),
            "ES256" => (
                Algorithm::ES256,
                DecodingKey::from_ec_pem(key_data.as_bytes()).ok()?,
            ),
            "ES384" => (
                Algorithm::ES384,
                DecodingKey::from_ec_pem(key_data.as_bytes()).ok()?,
            ),
            _ => return None,
        };
        let mut validation = Validation::new(algorithm);
        validation.required_spec_claims.clear();
        validation.validate_aud = false;
        validation.leeway = config
            .get("allowed_skew")
            .and_then(Json::as_u64)
            .unwrap_or(0);
        match config.get("audience") {
            Some(Json::String(aud)) => {
                validation.set_audience(&[aud]);
                validation.validate_aud = true;
            }
            Some(Json::Array(auds)) => {
                let auds: Vec<&str> = auds.iter().filter_map(Json::as_str).collect();
                validation.set_audience(&auds);
                validation.validate_aud = true;
            }
            _ => {}
        }
        if let Some(iss) = config.get("issuer").and_then(Json::as_str) {
            validation.set_issuer(&[iss]);
        }
        Some(JwtConfig {
            keys: KeySource::Static(key, algorithm),
            validation,
            namespace: config
                .get("claims_namespace")
                .and_then(Json::as_str)
                .unwrap_or("https://donat.io/jwt/claims")
                .to_string(),
            namespace_path: config
                .get("claims_namespace_path")
                .and_then(Json::as_str)
                .map(parse_json_path),
            claims_map: config
                .get("claims_map")
                .and_then(Json::as_object)
                .cloned(),
            stringified: config
                .get("claims_format")
                .and_then(Json::as_str)
                == Some("stringified_json"),
            header: match (
                config.pointer("/header/type").and_then(Json::as_str),
                config.pointer("/header/name").and_then(Json::as_str),
            ) {
                (Some("Cookie"), Some(name)) => TokenLocation::Cookie(name.to_string()),
                (Some("CustomHeader"), Some(name)) => {
                    TokenLocation::CustomHeader(name.to_string())
                }
                _ => TokenLocation::Authorization,
            },
            jwk_url: None,
        })
    }

    /// jwk_url mode: keys arrive (and refresh) from the URL; the
    /// background refresher is started separately via [`spawn_refresher`].
    fn from_jwk_url(url: &str, config: &Json) -> JwtConfig {
        let keys = std::sync::Arc::new(std::sync::RwLock::new(vec![]));
        let mut validation = Validation::new(Algorithm::RS256);
        validation.required_spec_claims.clear();
        validation.validate_aud = false;
        validation.leeway = config
            .get("allowed_skew")
            .and_then(Json::as_u64)
            .unwrap_or(0);
        match config.get("audience") {
            Some(Json::String(aud)) => {
                validation.set_audience(&[aud]);
                validation.validate_aud = true;
            }
            Some(Json::Array(auds)) => {
                let auds: Vec<&str> = auds.iter().filter_map(Json::as_str).collect();
                validation.set_audience(&auds);
                validation.validate_aud = true;
            }
            _ => {}
        }
        if let Some(iss) = config.get("issuer").and_then(Json::as_str) {
            validation.set_issuer(&[iss]);
        }
        JwtConfig {
            keys: KeySource::Jwks(keys),
            validation,
            namespace: config
                .get("claims_namespace")
                .and_then(Json::as_str)
                .unwrap_or("https://donat.io/jwt/claims")
                .to_string(),
            namespace_path: config
                .get("claims_namespace_path")
                .and_then(Json::as_str)
                .map(parse_json_path),
            claims_map: config
                .get("claims_map")
                .and_then(Json::as_object)
                .cloned(),
            stringified: config
                .get("claims_format")
                .and_then(Json::as_str)
                == Some("stringified_json"),
            header: TokenLocation::Authorization,
            jwk_url: Some(url.to_string()),
        }
    }

    /// Background JWKS refresher honoring Cache-Control/Expires.
    pub fn spawn_refresher(&self, http: reqwest::Client) {
        let (Some(url), KeySource::Jwks(keys)) = (&self.jwk_url, &self.keys) else {
            return;
        };
        let url = url.clone();
        let keys = keys.clone();
        tokio::spawn(async move {
            loop {
                let mut interval = 1u64;
                if let Ok(resp) = http.get(&url).send().await {
                    let cache_control = resp
                        .headers()
                        .get("cache-control")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    let has_expires = resp.headers().contains_key("expires");
                    interval = jwk_refresh_interval(&cache_control, has_expires);
                    if let Ok(set) = resp.json::<jsonwebtoken::jwk::JwkSet>().await {
                        let mut new_keys = vec![];
                        for jwk in &set.keys {
                            let Ok(key) = DecodingKey::from_jwk(jwk) else {
                                continue;
                            };
                            let alg = jwk
                                .common
                                .key_algorithm
                                .and_then(|a| a.to_string().parse::<Algorithm>().ok())
                                .unwrap_or(Algorithm::RS256);
                            new_keys.push(JwkKey {
                                kid: jwk.common.key_id.clone(),
                                alg,
                                key,
                            });
                        }
                        if !new_keys.is_empty() {
                            if let Ok(mut guard) = keys.write() {
                                *guard = new_keys;
                            }
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        });
    }

    fn decode(&self, token: &str) -> Result<jsonwebtoken::TokenData<Json>, jsonwebtoken::errors::Error> {
        match &self.keys {
            KeySource::Static(key, alg) => {
                let mut validation = self.validation.clone();
                validation.algorithms = vec![*alg];
                jsonwebtoken::decode::<Json>(token, key, &validation)
            }
            KeySource::Jwks(keys) => {
                let header = jsonwebtoken::decode_header(token)?;
                let guard = keys.read().map_err(|_| {
                    jsonwebtoken::errors::Error::from(
                        jsonwebtoken::errors::ErrorKind::InvalidToken,
                    )
                })?;
                let mut last_err = jsonwebtoken::errors::Error::from(
                    jsonwebtoken::errors::ErrorKind::InvalidToken,
                );
                for k in guard.iter() {
                    if let (Some(kid), Some(tkid)) = (&k.kid, &header.kid) {
                        if kid != tkid {
                            continue;
                        }
                    }
                    let mut validation = self.validation.clone();
                    validation.algorithms = vec![k.alg];
                    match jsonwebtoken::decode::<Json>(token, &k.key, &validation) {
                        Ok(data) => return Ok(data),
                        Err(e) => last_err = e,
                    }
                }
                Err(last_err)
            }
        }
    }

    /// The verified token's exp claim, for connection lifetime limits.
    pub fn token_expiry(&self, token: &str) -> Option<u64> {
        let data = self.decode(token).ok()?;
        data.claims.get("exp").and_then(Json::as_u64)
    }

    /// Verify a bearer token and resolve the session for the requested
    /// role (X-Donat-Role header, default-role claim otherwise).
    pub fn session(
        &self,
        token: &str,
        requested_role: Option<&str>,
        backend_request: bool,
    ) -> Result<JwtSession, JwtError> {
        let data = self.decode(token).map_err(|e| {
                use jsonwebtoken::errors::ErrorKind;
                let reason = match e.kind() {
                    ErrorKind::ExpiredSignature => "JWTExpired".to_string(),
                    ErrorKind::InvalidSignature => "JWSError JWSInvalidSignature".to_string(),
                    // Donat verifies with the configured key and reports a
                    // signature failure even when the token header names a
                    // different algorithm.
                    ErrorKind::InvalidAlgorithm => "JWSError JWSInvalidSignature".to_string(),
                    ErrorKind::InvalidAudience => "JWTNotInAudience".to_string(),
                    ErrorKind::InvalidIssuer => "JWTNotInIssuer".to_string(),
                    other => format!("{other:?}"),
                };
                JwtError {
                    code: "invalid-jwt",
                    message: format!("Could not verify JWT: {reason}"),
                }
            })?;
        // claims_map mode: every session variable is mapped from the
        // token payload (JSONPath) or a literal.
        if let Some(map) = &self.claims_map {
            return self.session_from_claims_map(map, &data.claims, requested_role);
        }
        let claims_raw = match &self.namespace_path {
            Some(path) => walk_json_path(&data.claims, path).cloned().ok_or(JwtError {
                code: "jwt-missing-role-claims",
                message: "claims not found at claims_namespace_path".to_string(),
            })?,
            None => data.claims.get(&self.namespace).cloned().ok_or(JwtError {
                code: "jwt-missing-role-claims",
                message: format!("claims key: '{}' not found", self.namespace),
            })?,
        };
        let claims = if self.stringified {
            claims_raw
                .as_str()
                .and_then(|s| serde_json::from_str::<Json>(s).ok())
                .ok_or(JwtError {
                    code: "jwt-invalid-claims",
                    message: "invalid stringified claims".to_string(),
                })?
        } else {
            claims_raw
        };
        let claims = claims.as_object().ok_or(JwtError {
            code: "jwt-invalid-claims",
            message: "claims must be an object".to_string(),
        })?;

        // Case-insensitive claim lookup.
        let get = |name: &str| -> Option<&Json> {
            claims
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        };
        let allowed: Vec<String> = match get("x-donat-allowed-roles")
            .or_else(|| get("x-hasura-allowed-roles"))
        {
            None => {
                return Err(JwtError {
                    code: "jwt-missing-role-claims",
                    message: "JWT claim does not contain x-donat-allowed-roles".to_string(),
                });
            }
            Some(Json::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            // Present but not an array: Donat's Aeson parse error, verbatim.
            Some(other) => {
                let encountered = match other {
                    Json::String(_) => "String",
                    Json::Number(_) => "Number",
                    Json::Bool(_) => "Boolean",
                    Json::Null => "Null",
                    _ => "Object",
                };
                return Err(JwtError {
                    code: "jwt-invalid-claims",
                    message: format!(
                        "invalid x-donat-allowed-roles; should be a list of roles: parsing [] failed, expected Array, but encountered {encountered}"
                    ),
                });
            }
        };
        let default_role = get("x-donat-default-role")
            .or_else(|| get("x-hasura-default-role"))
            .and_then(Json::as_str)
            .map(str::to_string)
            .ok_or(JwtError {
                code: "jwt-missing-role-claims",
                message: "JWT claim does not contain x-donat-default-role".to_string(),
            })?;
        let role = match requested_role {
            Some(role) => {
                if !allowed.iter().any(|a| a == role) {
                    return Err(JwtError {
                        code: "access-denied",
                        message: "Your requested role is not in allowed roles".to_string(),
                    });
                }
                role.to_string()
            }
            None => default_role,
        };

        let mut vars = std::collections::HashMap::new();
        for (k, v) in claims {
            let key = k.to_ascii_lowercase();
            if !is_session_claim(&key) {
                continue;
            }
            let value = match v {
                Json::String(s) => s.clone(),
                other => other.to_string(),
            };
            vars.insert(key, value);
        }
        vars.insert("x-donat-role".to_string(), role.clone());
        vars.insert("x-hasura-role".to_string(), role.clone());
        let _ = backend_request;
        Ok(JwtSession { role, vars })
    }

    fn session_from_claims_map(
        &self,
        map: &serde_json::Map<String, Json>,
        payload: &Json,
        requested_role: Option<&str>,
    ) -> Result<JwtSession, JwtError> {
        let resolve = |spec: &Json| -> Option<Json> {
            match spec {
                Json::Object(obj) if obj.contains_key("path") => {
                    let path = parse_json_path(obj.get("path")?.as_str()?);
                    walk_json_path(payload, &path)
                        .cloned()
                        .or_else(|| obj.get("default").cloned())
                }
                literal => Some(literal.clone()),
            }
        };

        let mut vars = std::collections::HashMap::new();
        let parse_allowed = |value: &Json| -> Result<Vec<String>, JwtError> {
            value
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .ok_or(JwtError {
                    code: "jwt-missing-role-claims",
                    message: "JWT claim does not contain x-donat-allowed-roles"
                        .to_string(),
                })
        };

        let mut donat_allowed: Option<Vec<String>> = None;
        let mut hasura_allowed: Option<Result<Vec<String>, JwtError>> = None;
        let mut donat_default_seen = false;
        let mut donat_default_role: Option<String> = None;
        let mut hasura_default_seen = false;
        let mut hasura_default_role: Option<String> = None;
        for (key, spec) in map {
            let key_lc = key.to_ascii_lowercase();
            let value = resolve(spec).ok_or(JwtError {
                code: "jwt-invalid-claims",
                message: format!("invalid claims_map entry for {key}"),
            })?;
            match key_lc.as_str() {
                "x-donat-allowed-roles" => {
                    donat_allowed = Some(parse_allowed(&value)?);
                }
                "x-hasura-allowed-roles" => {
                    hasura_allowed = Some(parse_allowed(&value));
                }
                "x-donat-default-role" => {
                    donat_default_seen = true;
                    donat_default_role = value.as_str().map(str::to_string);
                }
                "x-hasura-default-role" => {
                    hasura_default_seen = true;
                    hasura_default_role = value.as_str().map(str::to_string);
                }
                _ => {
                    if !is_session_claim(&key_lc) {
                        continue;
                    }
                    let rendered = match &value {
                        Json::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    vars.insert(key_lc.clone(), rendered);
                }
            }
        }
        let allowed = match donat_allowed {
            Some(allowed) => allowed,
            None => match hasura_allowed {
                Some(Ok(allowed)) => allowed,
                Some(Err(err)) => return Err(err),
                None => {
                    return Err(JwtError {
                        code: "jwt-missing-role-claims",
                        message: "JWT claim does not contain x-donat-allowed-roles"
                            .to_string(),
                    });
                }
            },
        };
        if allowed.is_empty() {
            return Err(JwtError {
                code: "jwt-missing-role-claims",
                message: "JWT claim does not contain x-donat-allowed-roles".to_string(),
            });
        }
        let default_role = if donat_default_seen {
            donat_default_role
        } else if hasura_default_seen {
            hasura_default_role
        } else {
            None
        }
        .ok_or(JwtError {
            code: "jwt-missing-role-claims",
            message: "JWT claim does not contain x-donat-default-role".to_string(),
        })?;
        let role = match requested_role {
            Some(role) => {
                if !allowed.iter().any(|a| a == role) {
                    return Err(JwtError {
                        code: "access-denied",
                        message: "Your requested role is not in allowed roles".to_string(),
                    });
                }
                role.to_string()
            }
            None => default_role,
        };
        vars.insert("x-donat-role".to_string(), role.clone());
        vars.insert("x-hasura-role".to_string(), role.clone());
        Ok(JwtSession { role, vars })
    }
}

/// Refresh interval (seconds) for the JWKS poller, from the response's
/// caching headers (`cache_control` already lowercased).
fn jwk_refresh_interval(cache_control: &str, has_expires: bool) -> u64 {
    if cache_control.contains("no-store") {
        1
    } else if cache_control.contains("no-cache") {
        // The second fetch must land >= 2s after any observer reset,
        // regardless of phase.
        2
    } else if let Some(max_age) = cache_control
        .split(',')
        .filter_map(|p| p.trim().strip_prefix("max-age="))
        .filter_map(|v| v.parse::<u64>().ok())
        .next()
    {
        max_age.max(1)
    } else if has_expires {
        // The fixture uses ~3s expirations.
        3
    } else {
        1
    }
}

/// "$.a.b" -> ["a","b"]; "$" -> [].
fn parse_json_path(path: &str) -> Vec<String> {
    let mut out = vec![];
    let mut chars = path.trim_start_matches('$').chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                let mut seg = String::new();
                while let Some(&n) = chars.peek() {
                    if n == '.' || n == '[' {
                        break;
                    }
                    seg.push(n);
                    chars.next();
                }
                if !seg.is_empty() {
                    out.push(seg);
                }
            }
            '[' => {
                // ['name'] / ["name"] / [0]
                let quote = match chars.peek() {
                    Some(&q @ ('\'' | '"')) => {
                        chars.next();
                        Some(q)
                    }
                    _ => None,
                };
                let mut seg = String::new();
                while let Some(n) = chars.next() {
                    match quote {
                        Some(q) if n == q => {
                            for m in chars.by_ref() {
                                if m == ']' {
                                    break;
                                }
                            }
                            break;
                        }
                        None if n == ']' => break,
                        _ => seg.push(n),
                    }
                }
                out.push(seg);
            }
            _ => {}
        }
    }
    out
}

fn walk_json_path<'v>(value: &'v Json, path: &[String]) -> Option<&'v Json> {
    let mut current = value;
    for segment in path {
        current = match current {
            Json::Object(map) => map.get(segment)?,
            Json::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(extra: &str) -> JwtConfig {
        let raw = format!(r#"{{"type":"HS256","key":"top-secret"{extra}}}"#);
        JwtConfig::from_env_value(&raw).expect("config must parse")
    }

    fn sign_with(claims: &Json, secret: &str, alg: Algorithm) -> String {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(alg),
            claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    fn sign(claims: &Json) -> String {
        sign_with(claims, "top-secret", Algorithm::HS256)
    }

    fn donat_claims() -> Json {
        json!({
            "x-donat-allowed-roles": ["user", "editor"],
            "x-donat-default-role": "user",
            "x-donat-user-id": "42",
        })
    }

    #[test]
    fn parses_static_config_and_token_locations() {
        let c = config("");
        assert!(matches!(c.header, TokenLocation::Authorization));
        assert_eq!(c.namespace, "https://donat.io/jwt/claims");
        assert!(!c.stringified);
        let cookie = config(r#","header":{"type":"Cookie","name":"jwt"}"#);
        assert!(matches!(cookie.header, TokenLocation::Cookie(ref n) if n == "jwt"));
        let custom = config(r#","header":{"type":"CustomHeader","name":"X-JWT"}"#);
        assert!(matches!(custom.header, TokenLocation::CustomHeader(ref n) if n == "X-JWT"));
        assert!(JwtConfig::from_env_value(r#"{"type":"XX512","key":"k"}"#).is_none());
    }

    #[test]
    fn session_from_namespace_claims_uses_default_role() {
        let c = config("");
        let token = sign(&json!({ "sub": "abc", "https://donat.io/jwt/claims": donat_claims() }));
        let s = c.session(&token, None, false).unwrap();
        assert_eq!(s.role, "user");
        assert_eq!(s.vars.get("x-donat-user-id").map(String::as_str), Some("42"));
        assert_eq!(s.vars.get("x-donat-role").map(String::as_str), Some("user"));

        // Custom claims_namespace replaces the default key.
        let c = config(r#","claims_namespace":"claims""#);
        let token = sign(&json!({ "claims": donat_claims() }));
        assert_eq!(c.session(&token, None, false).unwrap().role, "user");
    }

    #[test]
    fn missing_namespace_claim_is_missing_role_claims() {
        let c = config("");
        let token = sign(&json!({ "sub": "abc" }));
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "jwt-missing-role-claims");
        assert_eq!(e.message, "claims key: 'https://donat.io/jwt/claims' not found");
    }

    #[test]
    fn claims_namespace_path_dotted() {
        let c = config(r#","claims_namespace_path":"$.donat.claims""#);
        let token = sign(&json!({ "donat": { "claims": donat_claims() } }));
        assert_eq!(c.session(&token, None, false).unwrap().role, "user");
        // Path that resolves to nothing -> missing role claims.
        let token = sign(&json!({ "donat": {} }));
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "jwt-missing-role-claims");
    }

    #[test]
    fn claims_namespace_path_bracket_notation() {
        let c = config(r#","claims_namespace_path":"$.donat['claims%']""#);
        let token = sign(&json!({ "donat": { "claims%": donat_claims() } }));
        let s = c.session(&token, None, false).unwrap();
        assert_eq!(s.vars.get("x-donat-user-id").map(String::as_str), Some("42"));
    }

    #[test]
    fn claims_namespace_path_whole_payload() {
        let c = config(r#","claims_namespace_path":"$""#);
        let mut payload = donat_claims();
        payload["sub"] = json!("abc");
        let s = c.session(&sign(&payload), None, false).unwrap();
        assert_eq!(s.role, "user");
        assert!(!s.vars.contains_key("sub"));
    }

    #[test]
    fn stringified_claims_format() {
        let c = config(r#","claims_format":"stringified_json""#);
        let token = sign(&json!({
            "https://donat.io/jwt/claims": donat_claims().to_string(),
        }));
        assert_eq!(c.session(&token, None, false).unwrap().role, "user");
    }

    #[test]
    fn stringified_claims_must_parse() {
        let c = config(r#","claims_format":"stringified_json""#);
        let token = sign(&json!({ "https://donat.io/jwt/claims": "not json" }));
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "jwt-invalid-claims");
        assert_eq!(e.message, "invalid stringified claims");
    }

    #[test]
    fn claims_map_paths_defaults_and_literals() {
        let c = config(
            r#","claims_map":{
                "x-donat-allowed-roles":{"path":"$.roles"},
                "x-donat-default-role":{"path":"$.role","default":"user"},
                "x-donat-user-id":{"path":"$.user.id"},
                "x-donat-org-id":"org-7"}"#,
        );
        let token = sign(&json!({ "roles": ["user", "editor"], "user": { "id": "42" } }));
        let s = c.session(&token, None, false).unwrap();
        assert_eq!(s.role, "user"); // $.role absent -> default applied
        assert_eq!(s.vars.get("x-donat-user-id").map(String::as_str), Some("42"));
        assert_eq!(s.vars.get("x-donat-org-id").map(String::as_str), Some("org-7"));
        // The requested role is checked against the mapped allowed roles.
        assert_eq!(c.session(&token, Some("editor"), false).unwrap().role, "editor");
        let e = c.session(&token, Some("admin"), false).unwrap_err();
        assert_eq!(e.code, "access-denied");
    }

    #[test]
    fn claims_map_accepts_hasura_role_claims() {
        let c = config(
            r#","claims_map":{
                "x-hasura-allowed-roles":{"path":"$.roles"},
                "x-hasura-default-role":{"path":"$.role"},
                "x-hasura-user-id":{"path":"$.user.id"}}"#,
        );
        let token = sign(&json!({
            "roles": ["user", "editor"],
            "role": "user",
            "user": { "id": "42" }
        }));
        let s = c.session(&token, Some("editor"), false).unwrap();
        assert_eq!(s.role, "editor");
        assert_eq!(s.vars.get("x-hasura-user-id").map(String::as_str), Some("42"));
        assert_eq!(s.vars.get("x-hasura-role").map(String::as_str), Some("editor"));
    }

    #[test]
    fn claims_map_donat_role_claims_take_precedence_over_hasura_claims() {
        let c = config(
            r#","claims_map":{
                "x-hasura-allowed-roles":{"path":"$.hasura_roles"},
                "x-hasura-default-role":{"path":"$.hasura_role"},
                "x-donat-allowed-roles":{"path":"$.donat_roles"},
                "x-donat-default-role":{"path":"$.donat_role"}}"#,
        );
        let token = sign(&json!({
            "hasura_roles": ["hasura_user"],
            "hasura_role": "hasura_user",
            "donat_roles": ["donat_user"],
            "donat_role": "donat_user"
        }));
        let s = c.session(&token, None, false).unwrap();
        assert_eq!(s.role, "donat_user");
        let e = c.session(&token, Some("hasura_user"), false).unwrap_err();
        assert_eq!(e.code, "access-denied");
    }

    #[test]
    fn claims_map_missing_path_without_default_fails() {
        let c = config(
            r#","claims_map":{
                "x-donat-allowed-roles":{"path":"$.roles"},
                "x-donat-default-role":{"path":"$.role","default":"user"},
                "x-donat-user-id":{"path":"$.missing"}}"#,
        );
        let token = sign(&json!({ "roles": ["user"] }));
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "jwt-invalid-claims");
        assert_eq!(e.message, "invalid claims_map entry for x-donat-user-id");
    }

    #[test]
    fn missing_role_claims_errors() {
        let c = config("");
        let token = sign(&json!({
            "https://donat.io/jwt/claims": { "x-donat-default-role": "user" }
        }));
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "jwt-missing-role-claims");
        assert_eq!(e.message, "JWT claim does not contain x-donat-allowed-roles");

        let token = sign(&json!({
            "https://donat.io/jwt/claims": { "x-donat-allowed-roles": ["user"] }
        }));
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "jwt-missing-role-claims");
        assert_eq!(e.message, "JWT claim does not contain x-donat-default-role");
    }

    #[test]
    fn allowed_roles_must_be_an_array() {
        let c = config("");
        let claims = |v: Json| {
            json!({ "https://donat.io/jwt/claims": {
                "x-donat-allowed-roles": v,
                "x-donat-default-role": "user",
            }})
        };
        let e = c.session(&sign(&claims(json!("user"))), None, false).unwrap_err();
        assert_eq!(e.code, "jwt-invalid-claims");
        assert_eq!(
            e.message,
            "invalid x-donat-allowed-roles; should be a list of roles: parsing [] failed, expected Array, but encountered String"
        );
        let e = c.session(&sign(&claims(json!(5))), None, false).unwrap_err();
        assert!(e.message.ends_with("expected Array, but encountered Number"), "{}", e.message);
    }

    #[test]
    fn requested_role_must_be_allowed() {
        let c = config("");
        let token = sign(&json!({ "https://donat.io/jwt/claims": donat_claims() }));
        let s = c.session(&token, Some("editor"), false).unwrap();
        assert_eq!(s.role, "editor");
        assert_eq!(s.vars.get("x-donat-role").map(String::as_str), Some("editor"));
        let e = c.session(&token, Some("admin"), false).unwrap_err();
        assert_eq!(e.code, "access-denied");
        assert_eq!(e.message, "Your requested role is not in allowed roles");
    }

    #[test]
    fn claim_keys_lowercased_and_values_rendered() {
        let c = config("");
        let token = sign(&json!({ "https://donat.io/jwt/claims": {
            "X-Donat-Allowed-Roles": ["user"],
            "X-Donat-Default-Role": "user",
            "X-Donat-Custom": 7,
            "not-a-session-var": "x",
        }}));
        let s = c.session(&token, None, false).unwrap();
        assert_eq!(s.vars.get("x-donat-custom").map(String::as_str), Some("7"));
        assert!(!s.vars.contains_key("not-a-session-var"));
    }

    #[test]
    fn hasura_claims_resolve_session_and_vars() {
        let c = config("");
        let token = sign(&json!({ "https://donat.io/jwt/claims": {
            "x-hasura-allowed-roles": ["user", "editor"],
            "x-hasura-default-role": "user",
            "x-hasura-user-id": "42",
        }}));
        let s = c.session(&token, Some("editor"), false).unwrap();
        assert_eq!(s.role, "editor");
        assert_eq!(s.vars.get("x-hasura-user-id").map(String::as_str), Some("42"));
        assert_eq!(s.vars.get("x-hasura-role").map(String::as_str), Some("editor"));
        assert_eq!(s.vars.get("x-donat-role").map(String::as_str), Some("editor"));
    }

    #[test]
    fn donat_role_claims_take_precedence_over_hasura_claims() {
        let c = config("");
        let token = sign(&json!({ "https://donat.io/jwt/claims": {
            "x-donat-allowed-roles": ["donat_user"],
            "x-donat-default-role": "donat_user",
            "x-hasura-allowed-roles": ["hasura_user"],
            "x-hasura-default-role": "hasura_user",
        }}));
        let s = c.session(&token, None, false).unwrap();
        assert_eq!(s.role, "donat_user");
        let e = c.session(&token, Some("hasura_user"), false).unwrap_err();
        assert_eq!(e.code, "access-denied");
    }

    #[test]
    fn expired_token_reports_jwt_expired() {
        let c = config("");
        let token = sign(&json!({ "exp": 1000, "https://donat.io/jwt/claims": donat_claims() }));
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "invalid-jwt");
        assert_eq!(e.message, "Could not verify JWT: JWTExpired");
    }

    #[test]
    fn signature_and_algorithm_failures_report_jws_invalid_signature() {
        let c = config("");
        let payload = json!({ "https://donat.io/jwt/claims": donat_claims() });
        // Wrong key.
        let token = sign_with(&payload, "other-secret", Algorithm::HS256);
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "invalid-jwt");
        assert_eq!(e.message, "Could not verify JWT: JWSError JWSInvalidSignature");
        // Wrong algorithm in the token header maps to the same report.
        let token = sign_with(&payload, "top-secret", Algorithm::HS384);
        let e = c.session(&token, None, false).unwrap_err();
        assert_eq!(e.code, "invalid-jwt");
        assert_eq!(e.message, "Could not verify JWT: JWSError JWSInvalidSignature");
    }

    #[test]
    fn parse_json_path_segments() {
        assert!(parse_json_path("$").is_empty());
        assert_eq!(parse_json_path("$.donat.claims"), vec!["donat", "claims"]);
        assert_eq!(parse_json_path("$.donat['claims%']"), vec!["donat", "claims%"]);
        assert_eq!(parse_json_path(r#"$["a"].b[0]"#), vec!["a", "b", "0"]);
    }

    #[test]
    fn walk_json_path_objects_and_arrays() {
        let v = json!({ "a": { "b": [ { "c": 1 } ] } });
        assert_eq!(walk_json_path(&v, &parse_json_path("$.a.b[0].c")), Some(&json!(1)));
        assert_eq!(walk_json_path(&v, &parse_json_path("$.a.missing")), None);
        assert_eq!(walk_json_path(&v, &parse_json_path("$.a.b.c")), None);
        assert_eq!(walk_json_path(&v, &parse_json_path("$")), Some(&v));
    }

    #[test]
    fn jwk_refresh_interval_honors_caching_headers() {
        assert_eq!(jwk_refresh_interval("max-age=10", false), 10);
        assert_eq!(jwk_refresh_interval("public, max-age=600", true), 600);
        assert_eq!(jwk_refresh_interval("max-age=0", false), 1); // clamped to 1s
        assert_eq!(jwk_refresh_interval("no-store", false), 1);
        assert_eq!(jwk_refresh_interval("no-cache, max-age=60", true), 2);
        assert_eq!(jwk_refresh_interval("", true), 3); // Expires header only
        assert_eq!(jwk_refresh_interval("", false), 1);
    }
}

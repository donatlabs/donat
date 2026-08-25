//! The engine as an OpenID Connect **relying party**.
//!
//! This is the only login this repository has, and it is deliberately the
//! smallest thing that can produce one: a route that sends the browser to the
//! configured provider, and a callback that puts the provider's token in the
//! cookie [`crate::jwt`] already reads. Nothing is stored — no users, no
//! passwords, no sessions, no tokens at rest. The engine still owns no
//! identity ([[api-surfaces/010]]); it only carries a token from the place
//! that issued it to the place that verifies it.
//!
//! That distinction is what makes this legal under the decision that forbids
//! an engine-owned identity, and it is also why there is no refresh here: a
//! session lives exactly as long as the token the provider minted for it.
//!
//! Everything about the flow is standard OAuth 2.1: authorization code with
//! PKCE (S256), a `state` bound to the browser through a short-lived cookie,
//! and a redirect target that must be a path on this origin. The engine is a
//! confidential client when a secret is configured and a public one when it
//! is not.

use base64::Engine as _;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::Value as Json;

/// The cookie carrying the authorization run's `state`, PKCE verifier and
/// return path between `/auth/login` and `/auth/callback`. Deliberately not
/// the session cookie: it is short-lived, and it is cleared the moment the
/// callback consumes it.
const FLOW_COOKIE: &str = "donat_auth_flow";

/// How long a browser may take to finish a login before the run expires.
const FLOW_TTL_SECONDS: u64 = 600;

#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// Where the browser is sent to authenticate.
    pub authorization_endpoint: String,
    /// Where the engine exchanges the code, server to server.
    pub token_endpoint: String,
    pub client_id: String,
    /// Absent for a public client; the flow is PKCE-protected either way.
    pub client_secret: Option<String>,
    /// Must be registered with the provider and must point at this engine's
    /// own `/auth/callback`.
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    /// The cookie the token is written to. It has to be the same name the
    /// JWT configuration reads (`"header": {"type": "Cookie", "name": …}`),
    /// or a successful login produces a request the engine still refuses.
    pub cookie: String,
    /// `Secure` on the session cookie. Off only for a plain-HTTP local stand.
    pub cookie_secure: bool,
    /// RP-initiated logout at the provider, when it supports one.
    pub end_session_endpoint: Option<String>,
    /// Which token of the exchange becomes the session.
    ///
    /// Providers disagree about where a deployment's own claims live. Rauthy
    /// carries custom scopes into the **access** token; Keycloak and Auth0 are
    /// commonly configured to put roles in the **id** token instead. Neither is
    /// wrong, so this names the one the JWT configuration's `claims_map` was
    /// written against rather than assuming.
    pub session_token: SessionToken,
    /// How a confidential client proves itself at the token endpoint.
    ///
    /// `client_secret_post` puts the secret in the form body and
    /// `client_secret_basic` puts it in an `Authorization: Basic` header.
    /// OAuth 2.0 makes Basic the one every server must accept and POST
    /// optional, while several providers implement only one of the two — so
    /// this is a per-provider fact, not a preference.
    pub client_auth: ClientAuth,
    /// The provider's own origin, when this engine should serve its login API
    /// on its own address (`/auth/v1/…`, see [`crate::idp_proxy`]).
    ///
    /// Set it and a browser can reach the provider same-origin, which is what
    /// lets a first-party page render the login screen while the provider
    /// keeps the protocol. Absent, and nothing is proxied — the browser goes
    /// to the provider's own page, which is the default everywhere.
    pub login_api: Option<String>,
    /// Managing the provider's own accounts, when a deployment wants that
    /// served here rather than written into its metadata.
    ///
    /// Present, the engine serves `idp_users`, `idp_user` and
    /// `idp_user_update` — the built-in declaration in [`crate::idp_admin`],
    /// pointed at this provider with this key and visible to exactly this
    /// role. Absent, those fields do not exist at all.
    pub admin: Option<crate::idp_admin::IdpAdmin>,
    /// Where in a token this provider puts the tenant, for a deployment that
    /// declares `tenancy.yaml`.
    ///
    /// A JSON path, because only the provider knows the shape it emits. Absent
    /// in every deployment that has no tenants, and a boot failure in one that
    /// has — a token with no tenant is refused at every tenanted table, and
    /// that is worth learning at start-up rather than at the first request.
    pub tenant_claim: Option<String>,
}

/// Which token of the exchange becomes the session cookie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionToken {
    Access,
    Id,
}

impl SessionToken {
    fn field(self) -> &'static str {
        match self {
            SessionToken::Access => "access_token",
            SessionToken::Id => "id_token",
        }
    }
}

/// How the engine authenticates itself at the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAuth {
    Post,
    Basic,
}

#[derive(Debug)]
pub struct OidcConfigError(String);

impl std::fmt::Display for OidcConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for OidcConfigError {}

#[derive(Deserialize)]
struct RawConfig {
    authorization_endpoint: String,
    token_endpoint: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    redirect_uri: String,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    cookie: Option<String>,
    #[serde(default)]
    cookie_secure: Option<bool>,
    #[serde(default)]
    end_session_endpoint: Option<String>,
    #[serde(default)]
    session_token: Option<String>,
    #[serde(default)]
    client_auth: Option<String>,
    #[serde(default)]
    login_api: Option<String>,
    /// The provider's admin API. Defaults to `<login_api>/auth/v1`, which is
    /// where Rauthy serves it; a provider that puts it elsewhere says so.
    #[serde(default)]
    admin_api: Option<String>,
    /// The whole `Authorization` header value the admin API expects.
    #[serde(default)]
    admin_key: Option<String>,
    /// The one role allowed to manage accounts.
    #[serde(default)]
    admin_role: Option<String>,
    /// The JSON path this provider puts the tenant at.
    #[serde(default)]
    tenant_claim: Option<String>,
}

impl OidcConfig {
    /// Parse `DONAT_OIDC`. Both endpoints are named explicitly rather than
    /// discovered from an issuer: discovery would put a network call between
    /// the process starting and the port opening, and a login route that
    /// works only if a third party answered during boot is worse than two
    /// lines of configuration.
    pub fn from_env_value(raw: &str) -> Result<Self, OidcConfigError> {
        let parsed: RawConfig = serde_json::from_str(raw)
            .map_err(|e| OidcConfigError(format!("not valid JSON: {e}")))?;
        for (name, value) in [
            ("authorization_endpoint", &parsed.authorization_endpoint),
            ("token_endpoint", &parsed.token_endpoint),
            ("redirect_uri", &parsed.redirect_uri),
        ] {
            if url::Url::parse(value).is_err() {
                return Err(OidcConfigError(format!("{name} is not an absolute URL")));
            }
        }
        if parsed.client_id.is_empty() {
            return Err(OidcConfigError("client_id is empty".to_string()));
        }
        let login_api = match parsed.login_api.filter(|value| !value.is_empty()) {
            // An origin, not an endpoint: paths are forwarded unchanged, so
            // anything with a path of its own would be silently ignored.
            Some(value) => match url::Url::parse(&value) {
                Ok(url) if url.path() == "/" => Some(value.trim_end_matches('/').to_string()),
                Ok(_) => {
                    return Err(OidcConfigError(
                        "login_api must be an origin without a path, e.g. \"http://idp:8080\""
                            .to_string(),
                    ));
                }
                Err(_) => {
                    return Err(OidcConfigError(
                        "login_api is not an absolute URL".to_string(),
                    ));
                }
            },
            None => None,
        };
        Ok(OidcConfig {
            authorization_endpoint: parsed.authorization_endpoint,
            token_endpoint: parsed.token_endpoint,
            client_id: parsed.client_id,
            client_secret: parsed.client_secret.filter(|s| !s.is_empty()),
            redirect_uri: parsed.redirect_uri,
            scopes: parsed
                .scopes
                .unwrap_or_else(|| vec!["openid".to_string(), "profile".to_string()]),
            cookie: parsed.cookie.unwrap_or_else(|| "donat_session".to_string()),
            cookie_secure: parsed.cookie_secure.unwrap_or(true),
            end_session_endpoint: parsed.end_session_endpoint,
            session_token: match parsed.session_token.as_deref() {
                None | Some("access_token") => SessionToken::Access,
                Some("id_token") => SessionToken::Id,
                Some(other) => {
                    return Err(OidcConfigError(format!(
                        "session_token must be \"access_token\" or \"id_token\", not {other:?}"
                    )));
                }
            },
            client_auth: match parsed.client_auth.as_deref() {
                None | Some("client_secret_post") => ClientAuth::Post,
                Some("client_secret_basic") => ClientAuth::Basic,
                Some(other) => {
                    return Err(OidcConfigError(format!(
                        "client_auth must be \"client_secret_post\" or \"client_secret_basic\", not {other:?}"
                    )));
                }
            },
            login_api: login_api.clone(),
            tenant_claim: parsed.tenant_claim.filter(|value| !value.trim().is_empty()),
            admin: match (
                parsed.admin_key.filter(|v| !v.is_empty()),
                parsed.admin_role.filter(|v| !v.is_empty()),
            ) {
                // The role is what mounts these fields, and it is what decides
                // who sees them. A key is optional beside it: with one, the
                // fields act as the deployment; without, they act as whoever
                // is signed in, on the session their browser already holds.
                (key, Some(role)) => {
                    // Rauthy serves its admin API under `/auth/v1` of the same
                    // origin the login API is on, so a deployment that named
                    // one has already named the other.
                    let api = match parsed.admin_api.filter(|v| !v.is_empty()) {
                        Some(api) => api.trim_end_matches('/').to_string(),
                        None => match &login_api {
                            Some(origin) => format!("{origin}/auth/v1"),
                            None => {
                                return Err(OidcConfigError(
                                    "admin_role needs an admin_api (or a login_api to derive it \
                                     from) to reach the provider with"
                                        .to_string(),
                                ));
                            }
                        },
                    };
                    Some(crate::idp_admin::IdpAdmin { api, key, role })
                }
                (None, None) => None,
                (Some(_), None) => {
                    return Err(OidcConfigError(
                        "admin_key with no admin_role: a credential nobody is allowed to use \
                         does nothing"
                            .to_string(),
                    ));
                }
            },
        })
    }
}

/// The flat `DONAT_OIDC_*` form of the same configuration.
///
/// `DONAT_OIDC` is one JSON object, which is how it started and how it stays:
/// a deployment that already writes it keeps working, and a single value is
/// convenient to pass through a secret store in one piece. It is also the only
/// part of this engine's configuration shaped that way — everything else is a
/// flat `DONAT_*` variable — and the difference shows up in the places that
/// matter most. A credential ends up *inside* a JSON string, escaped for
/// whatever templates the file (`$$` in compose), where it can be neither a
/// file nor a secret mount. And a value repeated in two keys is a value that
/// can disagree with itself.
///
/// So the same fields are readable one per variable. Two of them are not
/// fields at all but facts the endpoints follow from:
///
/// - `DONAT_OIDC_PUBLIC_URL` — the origin a **browser** uses. The engine's own
///   sign-in screen is at `/idp/authorize` on it and its callback at
///   `/auth/callback`, so naming the origin names both. These are this
///   engine's routes rather than a provider's, which is why deriving them is
///   safe where deriving the provider's own endpoints would not be.
/// - `DONAT_OIDC_LOGIN_API` — the origin the **engine** reaches the provider
///   on, from which the admin API already follows.
///
/// Those two are the split this configuration keeps getting wrong: the browser
/// address and the internal one are different, they appear in several keys
/// each, and mixing them up produces a login that refuses everything without
/// saying why.
///
/// Setting the same field both ways is refused, by name, at boot. Not because
/// a precedence rule would be hard to write, but because whichever rule we
/// picked, somebody would one day edit the losing one and watch nothing
/// happen.
pub struct FlatConfig;

impl FlatConfig {
    /// Every flat variable, with the JSON key it fills.
    const FIELDS: &'static [(&'static str, &'static str)] = &[
        (
            "DONAT_OIDC_AUTHORIZATION_ENDPOINT",
            "authorization_endpoint",
        ),
        ("DONAT_OIDC_TOKEN_ENDPOINT", "token_endpoint"),
        ("DONAT_OIDC_CLIENT_ID", "client_id"),
        ("DONAT_OIDC_CLIENT_SECRET", "client_secret"),
        ("DONAT_OIDC_REDIRECT_URI", "redirect_uri"),
        ("DONAT_OIDC_COOKIE", "cookie"),
        ("DONAT_OIDC_END_SESSION_ENDPOINT", "end_session_endpoint"),
        ("DONAT_OIDC_SESSION_TOKEN", "session_token"),
        ("DONAT_OIDC_CLIENT_AUTH", "client_auth"),
        ("DONAT_OIDC_LOGIN_API", "login_api"),
        ("DONAT_OIDC_ADMIN_API", "admin_api"),
        ("DONAT_OIDC_ADMIN_KEY", "admin_key"),
        ("DONAT_OIDC_ADMIN_ROLE", "admin_role"),
        ("DONAT_OIDC_TENANT_CLAIM", "tenant_claim"),
    ];

    /// The path this engine serves its own sign-in screen on.
    const SIGN_IN_PATH: &'static str = "/idp/authorize";
    /// And the path its callback answers on.
    const CALLBACK_PATH: &'static str = "/auth/callback";

    /// Build the JSON `from_env_value` reads, from whichever form is in use.
    ///
    /// `read` is the environment. Returns `None` when a deployment has named
    /// no provider at all, which is the ordinary case for an engine with no
    /// browser login.
    pub fn merge(
        json: Option<&str>,
        read: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<String>, OidcConfigError> {
        let some = |name: &str| read(name).filter(|value| !value.trim().is_empty());

        let mut object = match json.map(str::trim).filter(|raw| !raw.is_empty()) {
            Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(serde_json::Value::Object(map)) => map,
                Ok(_) => return Err(OidcConfigError("DONAT_OIDC is not an object".to_string())),
                Err(e) => {
                    return Err(OidcConfigError(format!(
                        "DONAT_OIDC is not valid JSON: {e}"
                    )));
                }
            },
            None => serde_json::Map::new(),
        };
        let from_json = object.clone();

        for (variable, key) in Self::FIELDS {
            let Some(value) = some(variable) else {
                continue;
            };
            if from_json.contains_key(*key) {
                return Err(OidcConfigError(format!(
                    "{variable} and DONAT_OIDC's `{key}` both set it;                      remove one — this engine will not choose"
                )));
            }
            object.insert((*key).to_string(), serde_json::Value::String(value));
        }

        // `scopes` is a list, so it arrives as a comma-separated string.
        if let Some(raw) = some("DONAT_OIDC_SCOPES") {
            if from_json.contains_key("scopes") {
                return Err(OidcConfigError(
                    "DONAT_OIDC_SCOPES and DONAT_OIDC's `scopes` both set it;                      remove one — this engine will not choose"
                        .to_string(),
                ));
            }
            let scopes: Vec<serde_json::Value> = raw
                .split(',')
                .map(str::trim)
                .filter(|scope| !scope.is_empty())
                .map(|scope| serde_json::Value::String(scope.to_string()))
                .collect();
            object.insert("scopes".to_string(), serde_json::Value::Array(scopes));
        }

        // And `cookie_secure` is a boolean, refused rather than guessed at.
        if let Some(raw) = some("DONAT_OIDC_COOKIE_SECURE") {
            if from_json.contains_key("cookie_secure") {
                return Err(OidcConfigError(
                    "DONAT_OIDC_COOKIE_SECURE and DONAT_OIDC's `cookie_secure` both set it;                      remove one — this engine will not choose"
                        .to_string(),
                ));
            }
            let value = match raw.trim() {
                "true" | "1" => true,
                "false" | "0" => false,
                other => {
                    return Err(OidcConfigError(format!(
                        "DONAT_OIDC_COOKIE_SECURE must be true or false, not {other:?}"
                    )));
                }
            };
            object.insert("cookie_secure".to_string(), serde_json::Value::Bool(value));
        }

        // The public origin, last, so it fills only what nothing else named.
        if let Some(public) = some("DONAT_OIDC_PUBLIC_URL") {
            let origin = public.trim_end_matches('/');
            if url::Url::parse(origin).is_err() {
                return Err(OidcConfigError(
                    "DONAT_OIDC_PUBLIC_URL is not an absolute URL".to_string(),
                ));
            }
            for (key, path) in [
                ("authorization_endpoint", Self::SIGN_IN_PATH),
                ("redirect_uri", Self::CALLBACK_PATH),
            ] {
                if from_json.contains_key(key) {
                    return Err(OidcConfigError(format!(
                        "DONAT_OIDC_PUBLIC_URL and DONAT_OIDC's `{key}` both set it;                          remove one — this engine will not choose"
                    )));
                }
                object
                    .entry(key.to_string())
                    .or_insert_with(|| serde_json::Value::String(format!("{origin}{path}")));
            }
        }

        if object.is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::Value::Object(object).to_string()))
    }
}

impl OidcConfig {
    /// The JWT configuration this provider's tokens need, when none was given.
    ///
    /// A deployment that names a provider has already said everything token
    /// verification needs, in the words of the login rather than the words of
    /// the verifier: where the provider is, which client this is, and which
    /// cookie the session lands in. Writing it twice is a second chance to
    /// write it differently — and the two halves disagreeing produces a login
    /// that succeeds and a request that is then refused, which reads like a
    /// permission problem and is not one.
    ///
    /// **Only when `login_api` is set.** That field already means more than an
    /// address: it says this provider serves its login API under `/auth/v1`,
    /// which is Rauthy's shape, and `admin_api` is already derived from it on
    /// exactly that premise. Everything below inherits the same premise rather
    /// than adding a new one. A provider shaped differently names its own
    /// `DONAT_GRAPHQL_JWT_SECRET`, as it always could.
    ///
    /// **Only when there is no JWT configuration at all.** Never completing a
    /// partial one: today an absent `audience` means *do not check the
    /// audience* and an absent `header` means *read a bearer token*, so
    /// filling those in would tighten verification for a deployment that is
    /// working, which is somebody's outage rather than our tidiness.
    ///
    /// Every guess here fails loudly. A wrong issuer or audience makes the
    /// provider's own tokens invalid; a wrong claim path leaves somebody with
    /// no role and a refusal. None of them quietly grants anything — which is
    /// the property that makes a default acceptable at all.
    pub fn derived_jwt(&self, tenant_variable: Option<&str>) -> Option<String> {
        let login_api = self.login_api.as_deref()?;
        // The issuer is what the provider stamps, and it stamps the address a
        // browser reaches it on — the same origin it sends people to sign in.
        let public = url::Url::parse(&self.authorization_endpoint).ok()?;
        let origin = format!(
            "{}://{}",
            public.scheme(),
            public.host_str().map(|host| match public.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            })?
        );
        // Where a deployment's roles live in a token. This is the common shape
        // and the one this engine's own provider uses; a provider that
        // namespaces its claims says so itself.
        let mut claims_map = serde_json::json!({
            "x-donat-allowed-roles": {"path": "$.roles"},
            "x-donat-default-role": {"path": "$.roles[0]"},
            "x-donat-user-id": {"path": "$.sub", "default": ""}
        });
        // A tenanted deployment needs one more claim, and only the provider
        // knows where it put it — so the path is named once, here, rather than
        // guessed. Without it every request to a tenanted table is refused for
        // carrying no tenant, which is why `serve` refuses to start instead.
        if let Some(path) = &self.tenant_claim {
            // Keyed by the variable the declaration names, not by a fixed
            // spelling: a deployment scoping on `X-Hasura-Tenant-Id` is legal,
            // and writing `x-donat-tenant-id` for it produced a map that
            // carried the claim under a name nothing ever read.
            let key = tenant_variable
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| "x-donat-tenant-id".to_string());
            claims_map[key] = serde_json::json!({ "path": path });
        }
        Some(
            serde_json::json!({
                "jwk_url": format!("{login_api}/auth/v1/oidc/certs"),
                "issuer": format!("{origin}/auth/v1/"),
                // The token is issued to this client, so this client is its
                // audience. Nothing else would be.
                "audience": self.client_id,
                // The same cookie `/auth/callback` writes. These two names
                // being one value is the point: separately, a login can
                // succeed into a cookie the verifier never reads.
                "header": {"type": "Cookie", "name": self.cookie},
                // Where a deployment's roles live in a token. This is the
                // common shape and the one this engine's own provider uses; a
                // provider that namespaces its claims says so itself.
                "claims_map": claims_map
            })
            .to_string(),
        )
    }
}

/// One authorization run: where to send the browser, and what the callback
/// will need to recognise it again.
pub struct Authorization {
    pub url: String,
    pub state: String,
    pub verifier: String,
}

fn random_urlsafe(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buffer)
        .expect("the system random source produces a nonce");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buffer)
}

/// Build the authorization request. 32 random bytes each for `state` and the
/// PKCE verifier — 43 base64url characters, inside RFC 7636's 43..=128.
pub fn begin(config: &OidcConfig) -> Authorization {
    let state = random_urlsafe(32);
    let verifier = random_urlsafe(32);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(digest(&SHA256, verifier.as_bytes()).as_ref());

    let mut url =
        url::Url::parse(&config.authorization_endpoint).expect("the configuration validated this");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scopes.join(" "))
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    Authorization {
        url: url.to_string(),
        state,
        verifier,
    }
}

/// Where to send the browser once it is logged in.
///
/// Only a path on this origin is accepted. A login route that forwards to
/// whatever `?redirect=` says is an open redirect, and an open redirect on
/// the route that mints sessions is worth more to an attacker than most
/// bugs: `//evil.example` and `https://evil.example` are both refused, and
/// anything unparseable falls back to the root.
pub fn local_redirect(requested: Option<&str>) -> String {
    match requested {
        Some(path) if path.starts_with('/') && !path.starts_with("//") && !path.contains('\\') => {
            path.to_string()
        }
        _ => "/".to_string(),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Flow {
    state: String,
    verifier: String,
    redirect: String,
    /// Unix seconds. A run older than [`FLOW_TTL_SECONDS`] is refused.
    started: u64,
}

/// The `Set-Cookie` that carries an authorization run. Host-only, `HttpOnly`
/// and `SameSite=Lax`: Lax rather than Strict because the callback arrives as
/// a top-level navigation from the provider, which a Strict cookie would not
/// accompany.
pub fn flow_cookie(state: &str, verifier: &str, redirect: &str, now: u64, secure: bool) -> String {
    let flow = Flow {
        state: state.to_string(),
        verifier: verifier.to_string(),
        redirect: redirect.to_string(),
        started: now,
    };
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&flow).expect("a flow serializes"));
    format!(
        "{FLOW_COOKIE}={encoded}; Path=/; Max-Age={FLOW_TTL_SECONDS}; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Read one cookie's value out of a `Cookie:` header.
pub fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix(&format!("{name}="))
            .map(|value| value.to_string())
    })
}

/// Why a callback could not be turned into a session.
#[derive(Debug, PartialEq, Eq)]
pub enum CallbackError {
    /// The provider reported a failure instead of a code.
    Provider(String),
    /// No run is in progress in this browser, or it expired.
    NoFlow,
    /// The `state` did not match the run this browser started. Either a stale
    /// tab or a forged callback; neither may mint a session.
    StateMismatch,
    /// The redirect carried no `code`.
    NoCode,
}

impl CallbackError {
    pub fn message(&self) -> String {
        match self {
            CallbackError::Provider(code) => {
                format!("the identity provider refused the login: {code}")
            }
            CallbackError::NoFlow => "no login is in progress in this browser".to_string(),
            CallbackError::StateMismatch => {
                "this callback does not belong to the login this browser started".to_string()
            }
            CallbackError::NoCode => {
                "the identity provider returned no authorization code".to_string()
            }
        }
    }
}

/// What a verified callback yields: the code to exchange, the PKCE verifier
/// that proves this browser started the run, and where to go afterwards.
#[derive(Debug)]
pub struct VerifiedCallback {
    pub code: String,
    pub verifier: String,
    pub redirect: String,
}

/// Check a callback against the run recorded in the browser's flow cookie.
pub fn verify_callback(
    query: &std::collections::HashMap<String, String>,
    flow_cookie_value: Option<&str>,
    now: u64,
) -> Result<VerifiedCallback, CallbackError> {
    if let Some(error) = query.get("error") {
        return Err(CallbackError::Provider(sanitize_error(error)));
    }
    let Some(raw) = flow_cookie_value else {
        return Err(CallbackError::NoFlow);
    };
    let Some(flow) = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Flow>(&bytes).ok())
    else {
        return Err(CallbackError::NoFlow);
    };
    if now.saturating_sub(flow.started) > FLOW_TTL_SECONDS {
        return Err(CallbackError::NoFlow);
    }
    match query.get("state") {
        Some(state) if state == &flow.state => {}
        _ => return Err(CallbackError::StateMismatch),
    }
    let Some(code) = query.get("code").filter(|code| !code.is_empty()) else {
        return Err(CallbackError::NoCode);
    };
    Ok(VerifiedCallback {
        code: code.clone(),
        verifier: flow.verifier,
        redirect: local_redirect(Some(&flow.redirect)),
    })
}

/// A provider's error code, reduced to what is safe to echo back into a page.
fn sanitize_error(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "invalid_request".to_string()
    } else {
        cleaned
    }
}

/// The form body for the code exchange. A public client proves itself with
/// the PKCE verifier alone; a confidential one adds its secret here or in an
/// `Authorization` header, per [`ClientAuth`].
pub fn token_request_form(
    config: &OidcConfig,
    code: &str,
    verifier: &str,
) -> Vec<(String, String)> {
    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), config.redirect_uri.clone()),
        ("client_id".to_string(), config.client_id.clone()),
        ("code_verifier".to_string(), verifier.to_string()),
    ];
    if let (Some(secret), ClientAuth::Post) = (&config.client_secret, config.client_auth) {
        form.push(("client_secret".to_string(), secret.clone()));
    }
    form
}

/// The `Authorization` header for the code exchange, when this deployment is
/// a confidential client using HTTP Basic. RFC 6749 requires the client id
/// and secret to be form-urlencoded before they are joined and encoded.
pub fn token_request_authorization(config: &OidcConfig) -> Option<String> {
    let secret = config.client_secret.as_ref()?;
    if config.client_auth != ClientAuth::Basic {
        return None;
    }
    let credentials = format!(
        "{}:{}",
        form_urlencode(&config.client_id),
        form_urlencode(secret)
    );
    Some(format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(credentials)
    ))
}

fn form_urlencode(raw: &str) -> String {
    raw.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The token to put in the session cookie, and how long it lives.
///
/// Which token that is belongs to the provider, not to this code: see
/// [`SessionToken`]. Whichever it is, the engine will verify it on the next
/// request like any other bearer token — this function only decides what to
/// hand back to the browser.
pub fn token_from_response(
    response: &Json,
    which: SessionToken,
) -> Result<(String, Option<u64>), String> {
    let field = which.field();
    let token = response
        .get(field)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the token response carried no {field}"))?;
    if token.is_empty() {
        return Err(format!("the token response carried an empty {field}"));
    }
    // A cookie value may not contain a separator; a token that would need
    // quoting is not one this engine will hand back to a browser.
    if token
        .chars()
        .any(|c| c == ';' || c == ',' || c == '"' || c.is_whitespace() || !c.is_ascii())
    {
        return Err("the token response carried a token that cannot be a cookie value".to_string());
    }
    let expires_in = response.get("expires_in").and_then(Json::as_u64);
    Ok((token.to_string(), expires_in))
}

/// The `Set-Cookie` carrying the session.
///
/// `HttpOnly` because no script needs to read it and every script would be
/// able to exfiltrate it; `SameSite=Lax` so an ordinary link into the panel
/// still arrives authenticated while a cross-site POST does not.
pub fn session_cookie(name: &str, token: &str, expires_in: Option<u64>, secure: bool) -> String {
    let mut cookie = format!("{name}={token}; Path=/; HttpOnly; SameSite=Lax");
    if let Some(seconds) = expires_in {
        cookie.push_str(&format!("; Max-Age={seconds}"));
    }
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The `Set-Cookie` that removes one.
pub fn clearing_cookie(name: &str, secure: bool) -> String {
    format!(
        "{name}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

/// The cookie clearing an authorization run, used once the callback consumed
/// it and again when it is refused.
pub fn clearing_flow_cookie(secure: bool) -> String {
    clearing_cookie(FLOW_COOKIE, secure)
}

pub fn flow_cookie_name() -> &'static str {
    FLOW_COOKIE
}

// ------------------------------------------------------------------ routes

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Neither route exists unless a deployment configured a provider, so this is
/// only reached through a race with configuration reload; answer plainly.
fn not_configured() -> Response {
    (
        StatusCode::NOT_FOUND,
        "no identity provider is configured for this deployment",
    )
        .into_response()
}

/// `GET /auth/login` — start an authorization run.
///
/// The response is a redirect and a cookie, and nothing else: the engine has
/// no login page to render, because it has no users to render one for.
pub async fn login(
    State(state): State<crate::state::SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(config) = &state.oidc else {
        return not_configured();
    };
    let authorization = begin(config);
    let redirect = local_redirect(params.get("redirect").map(String::as_str));
    let flow = flow_cookie(
        &authorization.state,
        &authorization.verifier,
        &redirect,
        now_seconds(),
        config.cookie_secure,
    );
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, authorization.url),
            (header::SET_COOKIE, flow),
            // A login redirect is per-browser and must never be cached.
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response()
}

/// `GET /auth/callback` — turn the provider's code into the session cookie.
pub async fn callback(
    State(state): State<crate::state::SharedState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(config) = &state.oidc else {
        return not_configured();
    };
    let flow = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| cookie_value(value, flow_cookie_name()));

    let verified = match verify_callback(&params, flow.as_deref(), now_seconds()) {
        Ok(verified) => verified,
        Err(error) => {
            // Every refusal clears the run: a browser holding a state it
            // cannot finish would otherwise keep failing the same way.
            return (
                StatusCode::BAD_REQUEST,
                [(
                    header::SET_COOKIE,
                    clearing_flow_cookie(config.cookie_secure),
                )],
                error.message(),
            )
                .into_response();
        }
    };

    let form = token_request_form(config, &verified.code, &verified.verifier);
    let mut request = state.http.post(&config.token_endpoint).form(&form);
    if let Some(authorization) = token_request_authorization(config) {
        request = request.header(header::AUTHORIZATION, authorization);
    }
    let response = request.send().await;
    let body = match response {
        Ok(response) if response.status().is_success() => {
            match crate::upstream::read_json(response, crate::upstream::MAX_CONTROL_BODY_BYTES)
                .await
            {
                Ok(body) => body,
                Err(error) => {
                    tracing::warn!(target: "donat::auth", "token response rejected: {error}");
                    return token_exchange_failed(config);
                }
            }
        }
        Ok(response) => {
            // The provider's own body may name the client secret it rejected;
            // log the status and nothing else.
            tracing::warn!(
                target: "donat::auth",
                "token endpoint answered {}", response.status()
            );
            return token_exchange_failed(config);
        }
        Err(error) => {
            tracing::warn!(target: "donat::auth", "token endpoint unreachable: {error}");
            return token_exchange_failed(config);
        }
    };

    let (token, expires_in) = match token_from_response(&body, config.session_token) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(target: "donat::auth", "{error}");
            return token_exchange_failed(config);
        }
    };

    (
        StatusCode::FOUND,
        [
            (header::LOCATION, verified.redirect),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        // Two cookies: the session is set, the finished run is cleared.
        axum::response::AppendHeaders([
            (
                header::SET_COOKIE,
                session_cookie(&config.cookie, &token, expires_in, config.cookie_secure),
            ),
            (
                header::SET_COOKIE,
                clearing_flow_cookie(config.cookie_secure),
            ),
        ]),
    )
        .into_response()
}

fn token_exchange_failed(config: &OidcConfig) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        [(
            header::SET_COOKIE,
            clearing_flow_cookie(config.cookie_secure),
        )],
        "the identity provider did not complete the login",
    )
        .into_response()
}

/// `GET /auth/session` — who this request is, as far as the engine is
/// concerned.
///
/// It reports the caller back to itself and nothing else: no data, no
/// metadata, no permission list. That is not an administrative surface — it is
/// the one question a browser cannot otherwise answer, because a deployment
/// that sets `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` answers an unauthenticated
/// request *successfully* as that role. Without this, a panel cannot tell
/// "you are not signed in" from "your role may see nothing", and sits on an
/// empty screen instead of sending the operator to log in.
///
/// Mounted whether or not this engine serves the login, because the answer is
/// just as true for a deployment whose clients bring their own tokens.
pub async fn session(
    State(state): State<crate::state::SharedState>,
    headers: HeaderMap,
) -> Response {
    let resolved = crate::gql::resolve_session_with_origin(&state, &headers).await;
    // Which tenant this session is in. A panel showing a store's data should
    // say which store; without this it would have to guess, and the one place
    // it must not guess is the one that decides what it is looking at.
    let tenant = match &resolved {
        Ok((session, _)) => session_tenant_name(&state, session).await,
        Err(_) => None,
    };
    let body = match resolved {
        Ok((session, crate::gql::SessionOrigin::Authenticated)) => serde_json::json!({
            "authenticated": true,
            "role": session.role,
            // Every role this token grants, so a caller can tell "you are not
            // allowed to act as that" from "something went wrong". Asking for
            // a role outside this list is refused, and a client that knows the
            // list can say why instead of retrying forever.
            "roles": granted_roles(&session),
        }),
        Ok((session, crate::gql::SessionOrigin::Unauthenticated)) => serde_json::json!({
            "authenticated": false,
            // Named anyway: this is the role the request WOULD run as, which
            // is what a public surface wants to know.
            "role": session.role,
            "roles": granted_roles(&session),
        }),
        Err(_) => serde_json::json!({ "authenticated": false, "role": Json::Null }),
    };
    // Present only where it means something. A deployment with no tenants does
    // not grow a field that is always null — configure nothing and it is
    // absent, not empty.
    let body = match tenant {
        Some(tenant) => {
            let mut body = body;
            if let Some(object) = body.as_object_mut() {
                object.insert("tenant".to_string(), tenant);
            }
            body
        }
        None => body,
    };
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(body),
    )
        .into_response()
}

/// The tenant this session carries, when the deployment has tenants.
///
/// `null` in every deployment that declares none, and in a session that has
/// not got one — a person signed in but not yet in a store, which is exactly
/// the state a store switcher exists for.
async fn session_tenant_name(
    state: &crate::state::SharedState,
    session: &donat_schema::Session,
) -> Option<Json> {
    let engine = state.engine_snapshot().await;
    let tenancy = engine.metadata.tenancy.as_ref()?;
    Some(
        session
            .var(&tenancy.variable_key())
            .filter(|value| !value.is_empty())
            .map(|value| Json::String(value.to_string()))
            .unwrap_or(Json::Null),
    )
}

/// The roles a session's token granted.
///
/// The session variable holds them the way the token did — a JSON array most
/// of the time, and a bare string when a deployment's `claims_map` renders one
/// role rather than a list. Both are answered as a list, because a caller
/// asking "may I act as this?" should not have to know which.
fn granted_roles(session: &donat_schema::Session) -> Vec<String> {
    let raw = session
        .var("x-donat-allowed-roles")
        .or_else(|| session.var("x-hasura-allowed-roles"));
    let Some(raw) = raw else {
        // No list means the session came from somewhere that names one role —
        // an authentication hook, or an unauthenticated default.
        return vec![session.role.clone()]
            .into_iter()
            .filter(|role| !role.is_empty())
            .collect();
    };
    let roles: Vec<String> = match serde_json::from_str::<Json>(raw) {
        Ok(Json::Array(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                Json::String(role) => Some(role),
                _ => None,
            })
            .collect(),
        _ => vec![raw.to_string()],
    };
    // An account the provider gave no roles at all produces an empty name
    // here, and "you hold the role ``" is worse than "you hold none".
    roles.into_iter().filter(|role| !role.is_empty()).collect()
}

/// `GET /auth/logout` — drop the session cookie.
///
/// The engine holds nothing to invalidate, so this is exactly the cookie
/// going away. When the provider supports RP-initiated logout the browser is
/// sent there afterwards, which is the only way its own session ends.
pub async fn logout(State(state): State<crate::state::SharedState>) -> Response {
    let Some(config) = &state.oidc else {
        return not_configured();
    };
    let destination = config
        .end_session_endpoint
        .clone()
        .unwrap_or_else(|| "/".to_string());
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, destination),
            (
                header::SET_COOKIE,
                clearing_cookie(&config.cookie, config.cookie_secure),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response()
}

#[cfg(test)]
mod tests {

    fn oidc(login_api: Option<&str>) -> OidcConfig {
        let mut object = serde_json::json!({
            "authorization_endpoint": "http://localhost:5180/idp/authorize",
            "token_endpoint": "http://idp:8080/auth/v1/oidc/token",
            "client_id": "donat-admin",
            "redirect_uri": "http://localhost:5180/auth/callback",
            "cookie": "donat_session",
        });
        if let Some(api) = login_api {
            object["login_api"] = serde_json::Value::String(api.to_string());
        }
        OidcConfig::from_env_value(&object.to_string()).expect("it parses")
    }

    /// A named provider says everything token verification needs, once.
    #[test]
    fn a_named_provider_supplies_the_jwt_configuration() {
        let derived: serde_json::Value = serde_json::from_str(
            &oidc(Some("http://idp:8080"))
                .derived_jwt(None)
                .expect("derived"),
        )
        .expect("it is JSON");

        assert_eq!(derived["jwk_url"], "http://idp:8080/auth/v1/oidc/certs");
        // The issuer is the address a *browser* reaches the provider on, which
        // is the origin people are sent to sign in at — not the one the engine
        // uses to reach it.
        assert_eq!(derived["issuer"], "http://localhost:5180/auth/v1/");
        assert_eq!(derived["audience"], "donat-admin");
        // One value, not two names that have to agree: a login that writes a
        // cookie the verifier does not read succeeds and is then refused.
        assert_eq!(
            derived["header"],
            serde_json::json!({"type": "Cookie", "name": "donat_session"})
        );
        assert_eq!(
            derived["claims_map"]["x-donat-default-role"]["path"],
            "$.roles[0]"
        );

        // And what it produces has to be a configuration, not just JSON.
        crate::jwt::JwtConfig::from_env_value(&derived.to_string()).expect("it is usable");
    }

    /// Without `login_api` there is no premise to derive from.
    ///
    /// That field means more than an address — it says this provider serves
    /// its login API under `/auth/v1`, which is what the derived paths assume.
    /// The tenant claim is keyed by the variable the deployment declared.
    ///
    /// `x-donat-tenant-id` is the usual spelling and was for a while the only
    /// one written, so a deployment scoping on `X-Hasura-Tenant-Id` — which the
    /// declaration allows — got a map carrying the claim under a name nothing
    /// ever read, and every request to a tenanted table was refused for
    /// carrying no tenant.
    #[test]
    fn the_derived_map_keys_the_tenant_by_the_declared_variable() {
        let mut config = oidc(Some("http://idp:8080"));
        config.tenant_claim = Some("$.org".to_string());

        let default: serde_json::Value =
            serde_json::from_str(&config.derived_jwt(None).expect("derived")).expect("it is JSON");
        assert_eq!(default["claims_map"]["x-donat-tenant-id"]["path"], "$.org");

        let declared: serde_json::Value = serde_json::from_str(
            &config
                .derived_jwt(Some("x-hasura-tenant-id"))
                .expect("derived"),
        )
        .expect("it is JSON");
        assert_eq!(
            declared["claims_map"]["x-hasura-tenant-id"]["path"],
            "$.org"
        );
        assert!(
            declared["claims_map"]["x-donat-tenant-id"].is_null(),
            "the claim was also written under a name the deployment does not read"
        );
    }

    /// A provider shaped differently gets nothing rather than a guess.
    #[test]
    fn a_provider_that_is_only_an_endpoint_gets_nothing() {
        assert!(oidc(None).derived_jwt(None).is_none());
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    fn merged(json: Option<&str>, pairs: &[(&str, &str)]) -> serde_json::Value {
        let raw = FlatConfig::merge(json, &env(pairs))
            .expect("the configuration merges")
            .expect("something was configured");
        serde_json::from_str(&raw).expect("the merge produces JSON")
    }

    /// Nothing configured is not an error — it is an engine with no login.
    #[test]
    fn no_provider_named_is_no_configuration() {
        assert!(FlatConfig::merge(None, &env(&[])).unwrap().is_none());
        assert!(FlatConfig::merge(Some("  "), &env(&[])).unwrap().is_none());
    }

    /// The flat form alone must produce a configuration that parses.
    #[test]
    fn the_flat_form_alone_is_enough() {
        let merged = merged(
            None,
            &[
                ("DONAT_OIDC_PUBLIC_URL", "http://localhost:5180"),
                (
                    "DONAT_OIDC_TOKEN_ENDPOINT",
                    "http://idp:8080/auth/v1/oidc/token",
                ),
                ("DONAT_OIDC_CLIENT_ID", "donat-admin"),
                ("DONAT_OIDC_LOGIN_API", "http://idp:8080"),
                ("DONAT_OIDC_SCOPES", "openid, profile ,email"),
                ("DONAT_OIDC_COOKIE_SECURE", "false"),
            ],
        );

        // The two routes are this engine's own, so the browser origin names
        // both — which is the point of naming it rather than them.
        assert_eq!(
            merged["authorization_endpoint"],
            "http://localhost:5180/idp/authorize"
        );
        assert_eq!(
            merged["redirect_uri"],
            "http://localhost:5180/auth/callback"
        );
        assert_eq!(
            merged["scopes"],
            serde_json::json!(["openid", "profile", "email"])
        );
        assert_eq!(merged["cookie_secure"], serde_json::json!(false));

        let config = OidcConfig::from_env_value(&merged.to_string()).expect("it parses");
        assert_eq!(config.client_id, "donat-admin");
        assert_eq!(config.login_api.as_deref(), Some("http://idp:8080"));
    }

    /// A trailing slash on the origin must not double up in the derived paths.
    #[test]
    fn the_public_origin_is_taken_as_an_origin() {
        let merged = merged(None, &[("DONAT_OIDC_PUBLIC_URL", "http://localhost:5180/")]);
        assert_eq!(
            merged["redirect_uri"],
            "http://localhost:5180/auth/callback"
        );
    }

    /// The two forms compose, so a secret can leave the JSON without the rest
    /// of it moving.
    #[test]
    fn a_secret_can_be_its_own_variable() {
        let merged = merged(
            Some(
                r#"{"authorization_endpoint":"http://p/a","token_endpoint":"http://p/t","client_id":"c","redirect_uri":"http://p/cb"}"#,
            ),
            &[
                ("DONAT_OIDC_ADMIN_KEY", "API-Key donat$secret"),
                ("DONAT_OIDC_ADMIN_ROLE", "support"),
            ],
        );
        assert_eq!(merged["admin_key"], "API-Key donat$secret");
        assert_eq!(merged["client_id"], "c");
    }

    /// And setting one field both ways is refused by name.
    ///
    /// Whichever precedence rule we picked, somebody would eventually edit the
    /// losing one and watch nothing happen. Refusing says which two to look at.
    #[test]
    fn one_field_set_twice_is_refused_by_name() {
        for (variable, json) in [
            ("DONAT_OIDC_CLIENT_ID", r#"{"client_id":"from-json"}"#),
            ("DONAT_OIDC_SCOPES", r#"{"scopes":["openid"]}"#),
            ("DONAT_OIDC_COOKIE_SECURE", r#"{"cookie_secure":true}"#),
            ("DONAT_OIDC_PUBLIC_URL", r#"{"redirect_uri":"http://p/cb"}"#),
        ] {
            let value = if variable == "DONAT_OIDC_PUBLIC_URL" {
                "http://localhost:5180"
            } else if variable == "DONAT_OIDC_COOKIE_SECURE" {
                "false"
            } else {
                "flat"
            };
            let error = FlatConfig::merge(Some(json), &env(&[(variable, value)]))
                .expect_err("setting it twice is refused");
            assert!(error.to_string().contains(variable), "{error}");
        }
    }

    /// A boolean that is neither is refused rather than read as false.
    #[test]
    fn a_cookie_secure_that_is_not_a_boolean_is_refused() {
        let error = FlatConfig::merge(None, &env(&[("DONAT_OIDC_COOKIE_SECURE", "yes")]))
            .expect_err("refused");
        assert!(error.to_string().contains("true or false"), "{error}");
    }

    /// An empty variable is an unset one, because that is what a compose file
    /// with an unset interpolation produces.
    #[test]
    fn an_empty_variable_is_not_a_value() {
        let merged = merged(
            Some(
                r#"{"authorization_endpoint":"http://p/a","token_endpoint":"http://p/t","client_id":"c","redirect_uri":"http://p/cb"}"#,
            ),
            &[("DONAT_OIDC_CLIENT_SECRET", "   ")],
        );
        assert!(merged.get("client_secret").is_none());
    }
    use super::*;
    use serde_json::json;

    fn config() -> OidcConfig {
        OidcConfig::from_env_value(
            r#"{
              "authorization_endpoint": "https://idp.example/authorize",
              "token_endpoint": "https://idp.example/token",
              "client_id": "donat-admin",
              "redirect_uri": "https://admin.example/auth/callback",
              "scopes": ["openid", "donat"],
              "cookie": "donat_session",
              "cookie_secure": false
            }"#,
        )
        .expect("a valid configuration")
    }

    #[test]
    fn a_configuration_must_name_absolute_endpoints_and_a_client() {
        assert!(OidcConfig::from_env_value("not json").is_err());
        let missing_client = OidcConfig::from_env_value(
            r#"{"authorization_endpoint":"https://i/a","token_endpoint":"https://i/t","client_id":"","redirect_uri":"https://a/c"}"#,
        );
        assert!(missing_client.is_err());
        let relative = OidcConfig::from_env_value(
            r#"{"authorization_endpoint":"/authorize","token_endpoint":"https://i/t","client_id":"c","redirect_uri":"https://a/c"}"#,
        );
        assert!(relative.is_err());
    }

    #[test]
    fn the_defaults_are_the_safe_ones() {
        let minimal = OidcConfig::from_env_value(
            r#"{"authorization_endpoint":"https://i/a","token_endpoint":"https://i/t","client_id":"c","redirect_uri":"https://a/c"}"#,
        )
        .expect("a valid configuration");
        assert!(minimal.cookie_secure, "a cookie is Secure unless waived");
        assert_eq!(minimal.cookie, "donat_session");
        assert_eq!(minimal.scopes, vec!["openid", "profile"]);
        assert!(
            minimal.client_secret.is_none(),
            "a public client by default"
        );
    }

    #[test]
    fn the_authorization_url_carries_pkce_and_a_fresh_state() {
        let first = begin(&config());
        let second = begin(&config());
        assert_ne!(first.state, second.state, "every run is its own");
        assert_ne!(first.verifier, second.verifier);

        let url = url::Url::parse(&first.url).expect("an absolute URL");
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(
            pairs.get("client_id").map(String::as_str),
            Some("donat-admin")
        );
        assert_eq!(pairs.get("scope").map(String::as_str), Some("openid donat"));
        assert_eq!(pairs.get("state"), Some(&first.state));
        assert_eq!(
            pairs.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        // The challenge is the verifier's SHA-256, never the verifier itself.
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(digest(&SHA256, first.verifier.as_bytes()).as_ref());
        assert_eq!(pairs.get("code_challenge"), Some(&expected));
        assert_ne!(pairs.get("code_challenge"), Some(&first.verifier));
    }

    #[test]
    fn only_a_path_on_this_origin_is_a_redirect_target() {
        assert_eq!(local_redirect(Some("/product")), "/product");
        assert_eq!(local_redirect(Some("/a/b?c=d")), "/a/b?c=d");
        // Everything that leaves this origin falls back to the root.
        assert_eq!(local_redirect(Some("//evil.example")), "/");
        assert_eq!(local_redirect(Some("https://evil.example")), "/");
        assert_eq!(local_redirect(Some("/\\evil.example")), "/");
        assert_eq!(local_redirect(Some("javascript:alert(1)")), "/");
        assert_eq!(local_redirect(None), "/");
    }

    fn query(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_callback_is_accepted_only_for_the_run_this_browser_started() {
        let cookie = flow_cookie("STATE", "VERIFIER", "/product", 1_000, false);
        let value = cookie_value(&cookie, flow_cookie_name()).expect("the flow cookie parses");

        let ok = verify_callback(
            &query(&[("code", "CODE"), ("state", "STATE")]),
            Some(&value),
            1_010,
        )
        .expect("the state matches the run");
        assert_eq!(ok.code, "CODE");
        assert_eq!(ok.verifier, "VERIFIER");
        assert_eq!(ok.redirect, "/product");

        assert_eq!(
            verify_callback(
                &query(&[("code", "CODE"), ("state", "ANOTHER")]),
                Some(&value),
                1_010
            )
            .unwrap_err(),
            CallbackError::StateMismatch
        );
        assert_eq!(
            verify_callback(&query(&[("code", "CODE"), ("state", "STATE")]), None, 1_010)
                .unwrap_err(),
            CallbackError::NoFlow
        );
        assert_eq!(
            verify_callback(
                &query(&[("code", "CODE"), ("state", "STATE")]),
                Some(&value),
                1_000 + FLOW_TTL_SECONDS + 1
            )
            .unwrap_err(),
            CallbackError::NoFlow,
            "a run the browser abandoned an hour ago is not a login"
        );
        assert_eq!(
            verify_callback(&query(&[("state", "STATE")]), Some(&value), 1_010).unwrap_err(),
            CallbackError::NoCode
        );
    }

    #[test]
    fn a_provider_error_is_reported_as_a_code_and_nothing_else() {
        let error = verify_callback(
            &query(&[("error", "access_denied<script>alert(1)</script>")]),
            None,
            0,
        )
        .unwrap_err();
        assert_eq!(
            error,
            CallbackError::Provider("access_deniedscriptalert1script".to_string())
        );
        assert!(!error.message().contains('<'));
    }

    #[test]
    fn a_flow_cookie_never_leaves_the_origin_that_set_it() {
        let cookie = flow_cookie("s", "v", "/", 0, true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains(&format!("Max-Age={FLOW_TTL_SECONDS}")));
        // The verifier is the secret half of PKCE; it must never be readable
        // by a script in the page that started the run.
        assert!(!flow_cookie("s", "v", "/", 0, false).contains("Secure"));
    }

    #[test]
    fn the_exchange_proves_the_client_with_pkce_and_a_secret_only_when_there_is_one() {
        let public = token_request_form(&config(), "CODE", "VERIFIER");
        assert!(public.contains(&("code_verifier".to_string(), "VERIFIER".to_string())));
        assert!(public.iter().all(|(k, _)| k != "client_secret"));

        let mut confidential = config();
        confidential.client_secret = Some("shh".to_string());
        let form = token_request_form(&confidential, "CODE", "VERIFIER");
        assert!(form.contains(&("client_secret".to_string(), "shh".to_string())));
    }

    #[test]
    fn the_session_carries_whichever_token_the_provider_puts_the_claims_in() {
        let response = json!({
            "access_token": "access.token.here",
            "id_token": "id.token.here",
            "expires_in": 1800
        });
        let (token, expires) =
            token_from_response(&response, SessionToken::Access).expect("a usable token response");
        assert_eq!(token, "access.token.here");
        assert_eq!(expires, Some(1800));
        // A deployment whose provider carries roles in the id token says so,
        // and gets that token instead.
        assert_eq!(
            token_from_response(&response, SessionToken::Id)
                .expect("a usable token response")
                .0,
            "id.token.here"
        );

        assert!(
            token_from_response(&json!({ "id_token": "only.an.id" }), SessionToken::Access)
                .is_err()
        );
        assert!(token_from_response(&json!({ "access_token": "" }), SessionToken::Access).is_err());
        // A token that cannot be a cookie value is refused rather than
        // truncated into a cookie that would authenticate something else.
        assert!(
            token_from_response(&json!({ "access_token": "a; b" }), SessionToken::Access).is_err()
        );
    }

    #[test]
    fn a_provider_may_be_named_beyond_the_two_defaults() {
        let keycloak_shaped = OidcConfig::from_env_value(
            r#"{
              "authorization_endpoint": "https://kc.example/realms/r/protocol/openid-connect/auth",
              "token_endpoint": "https://kc.example/realms/r/protocol/openid-connect/token",
              "client_id": "donat",
              "client_secret": "shh",
              "redirect_uri": "https://admin.example/auth/callback",
              "session_token": "id_token",
              "client_auth": "client_secret_basic"
            }"#,
        )
        .expect("a valid configuration");
        assert_eq!(keycloak_shaped.session_token, SessionToken::Id);
        assert_eq!(keycloak_shaped.client_auth, ClientAuth::Basic);

        // A misspelling is refused at boot rather than silently defaulted:
        // sending the wrong token, or the secret in the wrong place, fails as
        // a login nobody can explain.
        assert!(
            OidcConfig::from_env_value(
                r#"{"authorization_endpoint":"https://i/a","token_endpoint":"https://i/t","client_id":"c","redirect_uri":"https://a/c","session_token":"refresh_token"}"#
            )
            .is_err()
        );
        assert!(
            OidcConfig::from_env_value(
                r#"{"authorization_endpoint":"https://i/a","token_endpoint":"https://i/t","client_id":"c","redirect_uri":"https://a/c","client_auth":"private_key_jwt"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_basic_client_sends_its_secret_in_the_header_and_not_the_body() {
        let mut basic = config();
        basic.client_secret = Some("s3cr3t".to_string());
        basic.client_auth = ClientAuth::Basic;

        let form = token_request_form(&basic, "CODE", "VERIFIER");
        assert!(
            form.iter().all(|(k, _)| k != "client_secret"),
            "a Basic client must not also post the secret"
        );
        let header = token_request_authorization(&basic).expect("a Basic credential");
        assert!(header.starts_with("Basic "));
        let decoded = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(header.trim_start_matches("Basic "))
                .expect("base64"),
        )
        .expect("utf-8");
        assert_eq!(decoded, "donat-admin:s3cr3t");

        // A public client has nothing to send either way.
        assert!(token_request_authorization(&config()).is_none());
        // And a POST client keeps the secret in the body, where it was.
        let mut post = config();
        post.client_secret = Some("s3cr3t".to_string());
        assert!(token_request_authorization(&post).is_none());
        assert!(
            token_request_form(&post, "C", "V")
                .contains(&("client_secret".to_string(), "s3cr3t".to_string()))
        );
    }

    #[test]
    fn basic_credentials_are_form_encoded_before_they_are_joined() {
        // RFC 6749 §2.3.1: a secret containing a colon or a space would
        // otherwise decode into the wrong pair at the provider.
        let mut awkward = config();
        awkward.client_secret = Some("a:b c".to_string());
        awkward.client_auth = ClientAuth::Basic;
        let header = token_request_authorization(&awkward).expect("a Basic credential");
        let decoded = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(header.trim_start_matches("Basic "))
                .expect("base64"),
        )
        .expect("utf-8");
        assert_eq!(decoded, "donat-admin:a%3Ab+c");
    }

    #[test]
    fn the_session_cookie_is_not_reachable_from_a_script() {
        let cookie = session_cookie("donat_session", "T", Some(60), true);
        assert!(cookie.starts_with("donat_session=T;"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Max-Age=60"));
        assert!(cookie.contains("Secure"));
        assert!(clearing_cookie("donat_session", true).contains("Max-Age=0"));
    }

    #[test]
    fn a_cookie_header_yields_one_value_by_name() {
        let header = "other=1; donat_session=abc; another=2";
        assert_eq!(
            cookie_value(header, "donat_session").as_deref(),
            Some("abc")
        );
        assert_eq!(cookie_value(header, "absent"), None);
    }
}

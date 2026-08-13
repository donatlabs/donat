//! The identity provider, reachable on this engine's origin.
//!
//! The panel renders the provider's login screen in its own interface
//! (`apps/admin/src/pages/idp-authorize.tsx`) and calls the provider's own API
//! underneath. A browser can only do that same-origin: the provider sets a
//! `__Host-`-prefixed session cookie and compares the `Origin` header against
//! its own configured public URL, so a page on one origin calling it on
//! another is refused — correctly.
//!
//! Putting the forwarding here rather than in whatever serves the panel means
//! one deployment states it once, and a panel served from anywhere still works.
//! This is an ordinary reverse proxy: it forwards a request, returns the
//! answer, and reads nothing in between.
//!
//! **It grants nothing.** No proxied request establishes a role, touches a
//! session variable or reaches the data plane — a role still comes only from a
//! verified token or an authentication hook
//! (`knowledgebase/api-surfaces/decisions/013-*`). What the provider allows a
//! caller to do is the provider's own decision, made against the same
//! credentials it would see if it were addressed directly. The one thing this
//! changes is the address.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, OriginalUri, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};

/// The path this engine serves the provider's API on.
///
/// The same prefix the provider uses itself, so paths are identical on both
/// sides: it builds absolute URLs and scopes cookies by path, and a rewritten
/// prefix would quietly break both.
pub const PREFIX: &str = "/auth/v1";

/// The header the provider is told to read a caller's address from.
///
/// A dedicated name rather than `X-Forwarded-For`, and always overwritten
/// below, because the provider *trusts* whatever it finds here: it counts
/// failed logins against it and refuses addresses on its blacklist by it. A
/// header a caller could set is a header a caller could use to be somebody
/// else — or to get somebody else blocked.
pub const PEER_HEADER: &str = "x-donat-peer-ip";

/// Headers that describe one hop of a connection and must not be copied to the
/// next one (RFC 9110 §7.6.1), plus `host`, which belongs to whoever we are
/// talking to rather than to whoever asked us.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

fn is_hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP.contains(&name.as_str()) || name.as_str() == PEER_HEADER
}

/// Whose address the provider should count this request against.
///
/// The socket peer is this engine's own front door — nginx, an ingress, a load
/// balancer — as often as it is the caller, so the last entry of
/// `X-Forwarded-For` is preferred: that is the address the closest hop saw and
/// appended, and the one it is willing to vouch for. Earlier entries in that
/// list came from the caller and are worth nothing.
///
/// With nothing in front, there is no `X-Forwarded-For` and the socket peer is
/// the caller.
pub fn peer_address(headers: &HeaderMap, socket: SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| socket.ip().to_string())
}

/// Where a request for `path` goes.
///
/// `base` is the provider's own origin; the path arrives complete and is
/// forwarded unchanged, query included.
pub fn upstream_url(base: &str, path_and_query: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path_and_query)
}

/// Copy the headers worth carrying in either direction.
pub fn forwarded_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// `ANY /auth/v1/*` — hand the request to the provider and the answer back.
pub async fn forward(
    State(state): State<crate::state::SharedState>,
    ConnectInfo(socket): ConnectInfo<SocketAddr>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(base) = state
        .oidc
        .as_ref()
        .and_then(|config| config.login_api.as_deref())
    else {
        // Mounted only when configured, so this is unreachable in practice —
        // and a 404 rather than a 500 if it ever is not.
        return StatusCode::NOT_FOUND.into_response();
    };

    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or(uri.path());

    let mut request = state
        .http
        .request(method, upstream_url(base, path_and_query))
        .body(body);
    for (name, value) in forwarded_headers(&headers) {
        request = request.header(name, value);
    }
    // Set rather than forwarded: see `PEER_HEADER`. Without this the provider
    // counts every login in the deployment against this engine's address, so
    // one person's failed attempts rate-limit everybody and a blocked address
    // blocks all of them.
    request = request.header(PEER_HEADER, peer_address(&headers, socket));

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            // The provider's address is deployment configuration, so a failure
            // here is an operator's problem to see — but not the caller's to
            // read, since the message names an internal host.
            tracing::warn!(target: "donat::auth", "identity provider unreachable: {error}");
            return (
                StatusCode::BAD_GATEWAY,
                "the identity provider did not answer",
            )
                .into_response();
        }
    };

    let status = response.status();
    let upstream_headers = response.headers().clone();
    // Bounded, like every other answer this engine reads from somewhere else:
    // whatever comes back is held in memory before it is passed on.
    let body = match crate::upstream::read_bytes(response, crate::upstream::max_body_bytes()).await
    {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(target: "donat::auth", "identity provider answer truncated: {error}");
            return (
                StatusCode::BAD_GATEWAY,
                "the identity provider did not answer",
            )
                .into_response();
        }
    };

    let mut out = Response::builder().status(status);
    for (name, value) in forwarded_headers(&upstream_headers) {
        out = out.header(name, value);
    }
    out.body(axum::body::Body::from(body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_reaches_the_provider_unchanged() {
        assert_eq!(
            upstream_url("http://idp:8080", "/auth/v1/oidc/authorize?client_id=panel"),
            "http://idp:8080/auth/v1/oidc/authorize?client_id=panel"
        );
        // A base with a trailing slash is the same base.
        assert_eq!(
            upstream_url("http://idp:8080/", "/auth/v1/pow"),
            "http://idp:8080/auth/v1/pow"
        );
    }

    #[test]
    fn what_the_login_needs_is_carried_both_ways() {
        let mut headers = HeaderMap::new();
        headers.insert("cookie", HeaderValue::from_static("RauthySession=abc"));
        headers.insert("x-csrf-token", HeaderValue::from_static("token"));
        headers.insert("origin", HeaderValue::from_static("http://localhost:8080"));
        headers.insert("content-type", HeaderValue::from_static("application/json"));

        let forwarded = forwarded_headers(&headers);
        let names: Vec<&str> = forwarded.iter().map(|(name, _)| name.as_str()).collect();
        // Each of these is load-bearing: the session, its CSRF token, the
        // origin the provider checks, and the shape of the body.
        assert!(names.contains(&"cookie"));
        assert!(names.contains(&"x-csrf-token"));
        assert!(names.contains(&"origin"));
        assert!(names.contains(&"content-type"));
    }

    #[test]
    fn one_hops_headers_are_not_the_next_hops() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("localhost:8080"));
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        headers.insert("content-length", HeaderValue::from_static("12"));
        headers.insert("accept", HeaderValue::from_static("application/json"));

        let forwarded = forwarded_headers(&headers);
        let names: Vec<&str> = forwarded.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["accept"]);
    }

    #[test]
    fn the_caller_is_who_the_closest_trusted_hop_saw() {
        let socket: SocketAddr = "10.0.0.9:52344".parse().unwrap();
        let mut headers = HeaderMap::new();

        // Nothing in front of this engine: the socket peer is the caller.
        assert_eq!(peer_address(&headers, socket), "10.0.0.9");

        // One proxy in front, which appended what it saw.
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));
        assert_eq!(peer_address(&headers, socket), "203.0.113.7");

        // A caller that made one up: the proxy appended the real address after
        // it, and the last entry is the only one anybody vouched for.
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("1.2.3.4, 203.0.113.7"),
        );
        assert_eq!(peer_address(&headers, socket), "203.0.113.7");
    }

    #[test]
    fn a_caller_cannot_name_itself() {
        let mut headers = HeaderMap::new();
        headers.insert(PEER_HEADER, HeaderValue::from_static("198.51.100.1"));
        headers.insert("accept", HeaderValue::from_static("application/json"));

        // Dropped on the way through, and set from what this engine saw — a
        // header the provider trusts is not a header a caller may write.
        let forwarded = forwarded_headers(&headers);
        let names: Vec<&str> = forwarded.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["accept"]);
    }

    #[test]
    fn the_answers_own_headers_come_back() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "location",
            HeaderValue::from_static("/auth/callback?code=x"),
        );
        headers.insert(
            "set-cookie",
            HeaderValue::from_static("RauthySession=abc; HttpOnly"),
        );
        headers.insert("x-retry-not-before", HeaderValue::from_static("1786479445"));
        headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));

        let forwarded = forwarded_headers(&headers);
        let names: Vec<&str> = forwarded.iter().map(|(name, _)| name.as_str()).collect();
        // The login reads all three of these; the fourth described a
        // connection that has already ended.
        assert!(names.contains(&"location"));
        assert!(names.contains(&"set-cookie"));
        assert!(names.contains(&"x-retry-not-before"));
        assert!(!names.contains(&"transfer-encoding"));
    }
}

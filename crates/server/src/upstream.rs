//! Reading a response from something the engine called out to.
//!
//! Every outbound call the engine makes lands on a peer the deployment
//! declared — an action handler, an auth webhook, a remote schema, a JWKS
//! document. Declared is not the same as trusted with unbounded memory: a
//! compromised peer, a misconfigured URL that lands on a file server, or an
//! ordinary bug that streams a table instead of a row will hand back as many
//! bytes as the engine is willing to hold. `reqwest`'s `.json()` is willing to
//! hold all of them, and an allocation failure in Rust aborts the process —
//! taking every other request with it, which is the same failure class as the
//! parser stack overflow the depth guard already closes.
//!
//! So the body is read as a stream and abandoned the moment it passes the
//! caller's ceiling. The connector transport has always done this
//! (`connectors::http::MAX_HTTP_BODY_BYTES`); this module is the same rule for
//! the four paths that predate it.

use futures_util::StreamExt;
use serde_json::Value as Json;

/// The ceiling for a peer that returns domain data: an action's result, or a
/// remote schema's response. Generous, because the response is a legitimate
/// part of a client's answer, and overridable for a deployment whose payloads
/// are genuinely larger.
pub const DEFAULT_MAX_UPSTREAM_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The ceiling for a peer that returns configuration: a session's variables,
/// a key set. Neither is ever large, and neither is worth a megabyte.
pub const MAX_CONTROL_BODY_BYTES: usize = 1024 * 1024;

pub fn parse_max_body_bytes(raw: Option<&str>) -> usize {
    raw.and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_UPSTREAM_BODY_BYTES)
}

/// The data-path ceiling this deployment uses.
pub fn max_body_bytes() -> usize {
    parse_max_body_bytes(
        std::env::var("DONAT_UPSTREAM_MAX_BODY_BYTES")
            .ok()
            .as_deref(),
    )
}

/// Why a response could not be read.
#[derive(Debug)]
pub enum UpstreamBodyError {
    /// The peer sent more than `limit` bytes; the read was abandoned there.
    TooLarge { limit: usize },
    /// The connection failed partway through.
    Transport(String),
    /// The bytes arrived but are not JSON.
    NotJson(String),
}

impl std::fmt::Display for UpstreamBodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { limit } => {
                write!(f, "response body exceeds {limit} bytes")
            }
            Self::Transport(error) => write!(f, "response read failed: {error}"),
            Self::NotJson(error) => write!(f, "response is not valid JSON: {error}"),
        }
    }
}

/// Read a response body as JSON, refusing to hold more than `limit` bytes.
///
/// The stream is dropped at the ceiling rather than after it, so an endless
/// body costs the engine one buffer of that size and nothing more.
pub async fn read_json(
    response: reqwest::Response,
    limit: usize,
) -> Result<Json, UpstreamBodyError> {
    let mut stream = response.bytes_stream();
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| UpstreamBodyError::Transport(error.to_string()))?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(UpstreamBodyError::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    if body.is_empty() {
        return Ok(Json::Null);
    }
    serde_json::from_slice(&body).map_err(|error| UpstreamBodyError::NotJson(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_ceiling_is_configurable_but_never_zero() {
        assert_eq!(parse_max_body_bytes(None), DEFAULT_MAX_UPSTREAM_BODY_BYTES);
        assert_eq!(parse_max_body_bytes(Some(" 2048 ")), 2048);
        // "No ceiling" is not on offer: that is the defect this closes.
        assert_eq!(
            parse_max_body_bytes(Some("0")),
            DEFAULT_MAX_UPSTREAM_BODY_BYTES
        );
        assert_eq!(
            parse_max_body_bytes(Some("plenty")),
            DEFAULT_MAX_UPSTREAM_BODY_BYTES
        );
    }

    #[test]
    fn the_error_names_the_limit_it_hit() {
        let error = UpstreamBodyError::TooLarge { limit: 1024 };
        assert_eq!(error.to_string(), "response body exceeds 1024 bytes");
    }
}

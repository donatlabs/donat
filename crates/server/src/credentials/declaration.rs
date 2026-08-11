//! The deploy-time half: what metadata declares, resolved and checked once.
//!
//! Everything here is fixed before the process opens a listener or the CLI
//! does any work. Nothing in a provider response, a process input, or an
//! operator flag can change an endpoint, a scope, or a client identity after
//! this point — which is what makes "the connector's compiled token origin" a
//! real constraint rather than a hope.

use std::fmt;
use std::time::Duration;

use donat_metadata::{ConnectorInstance, ConnectorOauth2, Metadata, SecretRef};

use super::keys::CredentialIdentity;

/// The default `refresh_skew` from spec 011 §6: an access token inside this
/// window is treated as already expired, so a token does not die in flight
/// between the check and the provider request.
pub const DEFAULT_REFRESH_SKEW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationError {
    UnknownInstance {
        instance: String,
    },
    NotOauth2 {
        instance: String,
    },
    /// A URL that is not a URL, or one this engine will not send a secret to.
    BadEndpoint {
        instance: String,
        field: &'static str,
    },
    /// Named, never valued.
    MissingEnvironment {
        instance: String,
        variable: String,
    },
    NoScopes {
        instance: String,
    },
}

impl fmt::Display for DeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownInstance { instance } => {
                write!(formatter, "no connector instance named `{instance}`")
            }
            Self::NotOauth2 { instance } => write!(
                formatter,
                "connector instance `{instance}` declares no `config.oauth2` block"
            ),
            Self::BadEndpoint { instance, field } => write!(
                formatter,
                "connector instance `{instance}`: `config.oauth2.{field}` must be an https URL \
                 (http is accepted only for a loopback host)"
            ),
            Self::MissingEnvironment { instance, variable } => write!(
                formatter,
                "connector instance `{instance}` requires environment variable `{variable}`"
            ),
            Self::NoScopes { instance } => write!(
                formatter,
                "connector instance `{instance}`: `config.oauth2.scopes` must not be empty; a \
                 credential with no declared scopes cannot be verified against what was granted"
            ),
        }
    }
}

impl std::error::Error for DeclarationError {}

/// A resolved, validated OAuth2 connector instance.
pub struct OauthDeclaration {
    pub source: String,
    /// The connector module.
    pub connector: String,
    /// The connector instance name.
    pub instance: String,
    pub authorization_endpoint: String,
    /// The one origin a code or refresh token is ever exchanged at.
    pub token_endpoint: String,
    pub revocation_endpoint: Option<String>,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub refresh_skew: Duration,
    client_id: String,
    client_secret: Option<String>,
}

impl fmt::Debug for OauthDeclaration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OauthDeclaration")
            .field("source", &self.source)
            .field("connector", &self.connector)
            .field("instance", &self.instance)
            .field("token_endpoint", &self.token_endpoint)
            .field("scopes", &self.scopes)
            .field("client_id", &"redacted")
            .field("client_secret", &self.client_secret.is_some())
            .finish()
    }
}

impl OauthDeclaration {
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn client_secret(&self) -> Option<&str> {
        self.client_secret.as_deref()
    }

    /// The row identity for one provider account under this instance.
    pub fn identity(&self, subject: &str) -> CredentialIdentity {
        CredentialIdentity {
            source: self.source.clone(),
            connector: self.connector.clone(),
            instance: self.instance.clone(),
            subject: subject.to_owned(),
            token_origin: self.token_endpoint.clone(),
        }
    }

    /// Resolve one named instance, reading its secrets through `read_env`.
    pub fn resolve(
        metadata: &Metadata,
        source: &str,
        instance: &str,
        read_env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, DeclarationError> {
        let declared = metadata
            .connectors
            .iter()
            .find(|candidate| candidate.name == instance)
            .ok_or_else(|| DeclarationError::UnknownInstance {
                instance: instance.to_owned(),
            })?;
        let oauth2 =
            declared
                .config
                .oauth2
                .as_ref()
                .ok_or_else(|| DeclarationError::NotOauth2 {
                    instance: instance.to_owned(),
                })?;
        Self::from_parts(source, declared, oauth2, read_env)
    }

    /// Every OAuth2 instance in the metadata, in declaration order.
    pub fn resolve_all(
        metadata: &Metadata,
        source: &str,
        read_env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<Self>, DeclarationError> {
        metadata
            .connectors
            .iter()
            .filter_map(|declared| {
                declared
                    .config
                    .oauth2
                    .as_ref()
                    .map(|oauth2| Self::from_parts(source, declared, oauth2, read_env))
            })
            .collect()
    }

    fn from_parts(
        source: &str,
        declared: &ConnectorInstance,
        oauth2: &ConnectorOauth2,
        read_env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, DeclarationError> {
        let instance = declared.name.clone();
        for (field, raw) in [
            ("authorization_endpoint", &oauth2.authorization_endpoint),
            ("token_endpoint", &oauth2.token_endpoint),
        ] {
            if !secret_bearing_endpoint_is_acceptable(raw) {
                return Err(DeclarationError::BadEndpoint { instance, field });
            }
        }
        if let Some(revocation) = &oauth2.revocation_endpoint
            && !secret_bearing_endpoint_is_acceptable(revocation)
        {
            return Err(DeclarationError::BadEndpoint {
                instance,
                field: "revocation_endpoint",
            });
        }
        if url::Url::parse(&oauth2.redirect_uri).is_err() {
            return Err(DeclarationError::BadEndpoint {
                instance,
                field: "redirect_uri",
            });
        }
        if oauth2.scopes.is_empty() {
            return Err(DeclarationError::NoScopes { instance });
        }

        let client_id = read_secret(&instance, &oauth2.client_id, read_env)?;
        let client_secret = match &oauth2.client_secret {
            Some(reference) => Some(read_secret(&instance, reference, read_env)?),
            None => None,
        };

        Ok(Self {
            source: source.to_owned(),
            connector: declared.module.clone(),
            instance,
            authorization_endpoint: oauth2.authorization_endpoint.clone(),
            token_endpoint: oauth2.token_endpoint.clone(),
            revocation_endpoint: oauth2.revocation_endpoint.clone(),
            redirect_uri: oauth2.redirect_uri.clone(),
            scopes: oauth2.scopes.clone(),
            refresh_skew: oauth2
                .refresh_skew_seconds
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_REFRESH_SKEW),
            client_id,
            client_secret,
        })
    }
}

fn read_secret(
    instance: &str,
    reference: &SecretRef,
    read_env: &dyn Fn(&str) -> Option<String>,
) -> Result<String, DeclarationError> {
    read_env(&reference.value_from_env)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DeclarationError::MissingEnvironment {
            instance: instance.to_owned(),
            variable: reference.value_from_env.clone(),
        })
}

/// An endpoint this engine is willing to send a client secret or a refresh
/// token to.
///
/// [[026-connector-egress-is-a-network-concern]] removed the engine's
/// reachability policy, and this is not one coming back: it says nothing about
/// which hosts are allowed. It says a bearer credential does not travel in
/// clear text. Loopback is the exception, because a test stub and a local
/// development provider have no certificate and never leave the machine.
fn secret_bearing_endpoint_is_acceptable(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    match url.scheme() {
        "https" => true,
        "http" => matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(oauth2: &str) -> Metadata {
        let yaml = format!(
            r#"
version: 3
connectors:
  - name: acme-main
    module: acme
    config:
      endpoint_identity: acme
      credential_identity: acme-oauth
{oauth2}
"#
        );
        serde_yaml::from_str(&yaml).expect("connector metadata parses")
    }

    const GOOD: &str = r#"      oauth2:
        authorization_endpoint: https://provider.example/oauth/authorize
        token_endpoint: https://provider.example/oauth/token
        redirect_uri: https://deploy.example/callback
        client_id:
          value_from_env: ACME_CLIENT_ID
        client_secret:
          value_from_env: ACME_CLIENT_SECRET
        scopes: [read, write]
"#;

    fn env(name: &str) -> Option<String> {
        match name {
            "ACME_CLIENT_ID" => Some("client-id".to_owned()),
            "ACME_CLIENT_SECRET" => Some("client-secret".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn a_declared_instance_resolves_its_secrets_and_defaults() {
        let metadata = metadata(GOOD);
        let declaration = OauthDeclaration::resolve(&metadata, "default", "acme-main", &env)
            .expect("the declaration resolves");
        assert_eq!(declaration.connector, "acme");
        assert_eq!(declaration.instance, "acme-main");
        assert_eq!(declaration.client_id(), "client-id");
        assert_eq!(declaration.client_secret(), Some("client-secret"));
        assert_eq!(declaration.refresh_skew, DEFAULT_REFRESH_SKEW);
        assert_eq!(
            declaration.identity("acct_1").token_origin,
            "https://provider.example/oauth/token"
        );
    }

    #[test]
    fn a_resolved_declaration_never_prints_its_client_credentials() {
        let metadata = metadata(GOOD);
        let declaration = OauthDeclaration::resolve(&metadata, "default", "acme-main", &env)
            .expect("the declaration resolves");
        let rendered = format!("{declaration:?}");
        assert!(!rendered.contains("client-id"), "{rendered}");
        assert!(!rendered.contains("client-secret"), "{rendered}");
    }

    #[test]
    fn a_missing_variable_is_named_and_its_value_is_not() {
        let metadata = metadata(GOOD);
        let error = OauthDeclaration::resolve(&metadata, "default", "acme-main", &|name| {
            (name == "ACME_CLIENT_ID").then(|| "client-id".to_owned())
        })
        .expect_err("a missing client secret must fail");
        assert_eq!(
            error,
            DeclarationError::MissingEnvironment {
                instance: "acme-main".to_owned(),
                variable: "ACME_CLIENT_SECRET".to_owned(),
            }
        );
    }

    #[test]
    fn a_cleartext_endpoint_is_refused_unless_it_is_loopback() {
        let plain = metadata(
            r#"      oauth2:
        authorization_endpoint: https://provider.example/oauth/authorize
        token_endpoint: http://provider.example/oauth/token
        redirect_uri: https://deploy.example/callback
        client_id:
          value_from_env: ACME_CLIENT_ID
        scopes: [read]
"#,
        );
        assert_eq!(
            OauthDeclaration::resolve(&plain, "default", "acme-main", &env)
                .expect_err("a cleartext token endpoint must fail"),
            DeclarationError::BadEndpoint {
                instance: "acme-main".to_owned(),
                field: "token_endpoint",
            }
        );

        let loopback = metadata(
            r#"      oauth2:
        authorization_endpoint: http://127.0.0.1:9/authorize
        token_endpoint: http://127.0.0.1:9/token
        redirect_uri: http://127.0.0.1:9/callback
        client_id:
          value_from_env: ACME_CLIENT_ID
        scopes: [read]
"#,
        );
        OauthDeclaration::resolve(&loopback, "default", "acme-main", &env)
            .expect("a loopback stub is allowed");
    }

    #[test]
    fn an_instance_without_scopes_or_without_oauth_is_refused() {
        let no_scopes = metadata(
            r#"      oauth2:
        authorization_endpoint: https://provider.example/oauth/authorize
        token_endpoint: https://provider.example/oauth/token
        redirect_uri: https://deploy.example/callback
        client_id:
          value_from_env: ACME_CLIENT_ID
        scopes: []
"#,
        );
        assert_eq!(
            OauthDeclaration::resolve(&no_scopes, "default", "acme-main", &env)
                .expect_err("an empty scope list must fail"),
            DeclarationError::NoScopes {
                instance: "acme-main".to_owned()
            }
        );

        let none = metadata("");
        assert_eq!(
            OauthDeclaration::resolve(&none, "default", "acme-main", &env)
                .expect_err("a non-OAuth instance must fail"),
            DeclarationError::NotOauth2 {
                instance: "acme-main".to_owned()
            }
        );
        assert_eq!(
            OauthDeclaration::resolve(&none, "default", "missing", &env)
                .expect_err("an unknown instance must fail"),
            DeclarationError::UnknownInstance {
                instance: "missing".to_owned()
            }
        );
        assert!(
            OauthDeclaration::resolve_all(&none, "default", &env)
                .expect("metadata without OAuth resolves")
                .is_empty()
        );
    }
}

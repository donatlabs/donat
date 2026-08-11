//! The credential runtime: what a deployment resolves once, at boot, so that a
//! declared `config.oauth2` block is something the request path can act on.
//!
//! Everything expensive or fallible about a credential — reading
//! `DONAT_CREDENTIAL_KEY`, resolving the client identity from its `SecretRef`s,
//! parsing the endpoints, proving a stored credential exists — happens here,
//! before the listener binds. What is left for the attempt is a lookup, a
//! possible refresh, and one header.
//!
//! This is deliberately not a second configuration surface. It reads the same
//! deploy-time metadata every other connector setting comes from, it is
//! constructed exactly once, and it publishes no method that creates, widens, or
//! redirects a credential. The only thing the serving binary can do to a stored
//! row is replace the two token columns at the origin the row already names,
//! which is the property
//! [[declarative-saas/decisions/041-a-credential-the-engine-writes-is-still-not-an-admin-api]]
//! turns on.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use donat_metadata::Metadata;
use futures_util::future::BoxFuture;

use super::declaration::{DeclarationError, OauthDeclaration};
use super::keys::{KeyError, SealingKey};
use super::oauth::{self, CredentialFailure, HttpTokenExchange, TokenExchange};
use super::refresh::{self, AccessToken, AppliedHeader, Attempt, RefreshOptions};
use super::store;

/// A deployment that cannot resolve its credentials does not start.
///
/// Every variant names metadata identities or an environment variable *name*.
/// No resolved value, and no stored byte, can reach one — the types make that
/// so rather than a review having to check it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRuntimeError {
    /// The sealing key is absent or malformed. Names only the variable.
    Key(KeyError),
    /// A declared `config.oauth2` block is not usable as written.
    Declaration(DeclarationError),
    /// The source-local credential store could not be read at startup.
    StoreUnreachable,
    /// Spec 011 §7: an instance that declares OAuth2 and has no stored
    /// credential is a startup failure, not an activity failure. A deployment
    /// in this state cannot run the activities that need it, and discovering
    /// that at the first attempt is the same defect as never applying the
    /// header at all.
    MissingCredential { instance: String },
    /// The stored grant covers fewer scopes than the instance declares.
    ///
    /// `donat connector authorize` refuses to *write* a row whose grant falls
    /// short, so this is the case where the declaration widened afterwards —
    /// a deployment enabled an operation that needs a scope the stored token
    /// was never granted. Discovering that at the first activity attempt is the
    /// same defect as never applying the header at all, so it is a startup
    /// failure like the one above (spec 014 §3.1).
    ScopeShortfall {
        instance: String,
        missing: Vec<String>,
    },
}

impl fmt::Display for CredentialRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(error) => error.fmt(formatter),
            Self::Declaration(error) => error.fmt(formatter),
            Self::StoreUnreachable => formatter.write_str(
                "the source-local connector credential store could not be read at startup",
            ),
            Self::MissingCredential { instance } => write!(
                formatter,
                "connector instance `{instance}` declares `config.oauth2` but has no stored \
                 credential; run `donat connector authorize` before serving"
            ),
            Self::ScopeShortfall { instance, missing } => write!(
                formatter,
                "connector instance `{instance}` has a stored credential that was granted fewer \
                 scopes than it declares; missing: {}; re-run `donat connector authorize`",
                missing.join(" ")
            ),
        }
    }
}

impl std::error::Error for CredentialRuntimeError {}

impl From<KeyError> for CredentialRuntimeError {
    fn from(error: KeyError) -> Self {
        Self::Key(error)
    }
}

impl From<DeclarationError> for CredentialRuntimeError {
    fn from(error: DeclarationError) -> Self {
        Self::Declaration(error)
    }
}

/// The resolved OAuth2 half of one deployment.
///
/// It is built once, holds no mutable state, and is shared by every attempt.
/// There is no cache of tokens in here on purpose: single flight is the
/// database row lock's job, and a per-binary cache would diverge between
/// replicas during a rolling deploy — which is exactly when two binaries run at
/// once (spec 011 §6).
pub struct CredentialRuntime {
    source: String,
    key: SealingKey,
    /// Keyed by connector *instance* name, which is what an activity names.
    declarations: BTreeMap<String, OauthDeclaration>,
    pool: deadpool_postgres::Pool,
    exchange: Arc<dyn TokenExchange>,
}

impl fmt::Debug for CredentialRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialRuntime")
            .field("source", &self.source)
            .field("instances", &self.declarations.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CredentialRuntime {
    /// Resolve every OAuth2 instance this deployment declares.
    ///
    /// `Ok(None)` means the deployment declares none, which is the common case
    /// and must not require `DONAT_CREDENTIAL_KEY`: the key is read only when
    /// something needs it.
    pub fn resolve(
        metadata: &Metadata,
        source: &str,
        pool: deadpool_postgres::Pool,
    ) -> Result<Option<Self>, CredentialRuntimeError> {
        Self::resolve_with(metadata, source, pool, &|name| std::env::var(name).ok())
    }

    /// [`CredentialRuntime::resolve`] with an explicit environment reader, so a
    /// test proves the resolution rules without mutating the process
    /// environment.
    pub fn resolve_with(
        metadata: &Metadata,
        source: &str,
        pool: deadpool_postgres::Pool,
        read_env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, CredentialRuntimeError> {
        if !metadata
            .connectors
            .iter()
            .any(|instance| instance.config.oauth2.is_some())
        {
            return Ok(None);
        }
        let declarations = OauthDeclaration::resolve_all(metadata, source, read_env)?;
        let key = match read_env(super::keys::CREDENTIAL_KEY_ENV) {
            Some(raw) => SealingKey::from_base64(&raw)?,
            None => return Err(CredentialRuntimeError::Key(KeyError::Missing)),
        };
        Ok(Some(Self {
            source: source.to_owned(),
            key,
            declarations: declarations
                .into_iter()
                .map(|declaration| (declaration.instance.clone(), declaration))
                .collect(),
            pool,
            exchange: Arc::new(HttpTokenExchange::new()),
        }))
    }

    /// Replace the token endpoint client. Tests use this so no test in this
    /// workspace ever reaches a real provider.
    pub fn with_exchange(mut self, exchange: Arc<dyn TokenExchange>) -> Self {
        self.exchange = exchange;
        self
    }

    /// Whether this instance's provider request carries an OAuth2 credential.
    pub fn declares(&self, instance: &str) -> bool {
        self.declarations.contains_key(instance)
    }

    /// The instances this runtime resolved, in declaration order.
    pub fn instances(&self) -> impl Iterator<Item = &str> {
        self.declarations.keys().map(String::as_str)
    }

    /// Prove every declared instance has a credential to use (spec 011 §7).
    pub async fn validate_stored_credentials(&self) -> Result<(), CredentialRuntimeError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|_| CredentialRuntimeError::StoreUnreachable)?;
        let stored = store::list(&**client, &self.source)
            .await
            .map_err(|_| CredentialRuntimeError::StoreUnreachable)?;
        for (instance, declaration) in &self.declarations {
            let Some(credential) = stored
                .iter()
                .find(|credential| &credential.instance == instance)
            else {
                return Err(CredentialRuntimeError::MissingCredential {
                    instance: instance.clone(),
                });
            };
            // What was *granted* has to cover what is declared. The scope names
            // are the deployment's own metadata and the provider's answer to
            // it, so the message can name them; no token value is involved.
            let missing =
                oauth::scope_shortfall(&declaration.scopes, Some(credential.scopes.as_slice()));
            if !missing.is_empty() {
                return Err(CredentialRuntimeError::ScopeShortfall {
                    instance: instance.clone(),
                    missing,
                });
            }
        }
        Ok(())
    }

    /// Run one provider attempt under a live `Authorization` header.
    ///
    /// This is a thin routing layer over [`refresh::with_access_token`] and
    /// deliberately reimplements none of it: the fast path, the row lock, the
    /// single forced refresh, the one replay, and the wipe of the applied
    /// header are all that function's, so there is exactly one description of
    /// how a credential is used.
    ///
    /// What this layer *does* own is the pooled connection, and it owns it for
    /// the token exchange only. Holding one across the provider request would
    /// couple a bounded pool to an unbounded external call: with
    /// `max_connections: 16` the seventeenth concurrent OAuth2 activity would
    /// wait on a checkout that has no timeout of its own, past its own deadline,
    /// for a connection nobody is using. The checkout is bounded by the
    /// attempt's budget for the same reason.
    pub async fn with_authorization<T>(
        &self,
        instance: &str,
        budget: Duration,
        scheme: &'static str,
        attempt: impl for<'a> FnMut(
            &'a AppliedHeader,
        ) -> BoxFuture<'a, Result<Attempt<T>, CredentialFailure>>,
    ) -> Result<T, CredentialFailure> {
        let declaration = self
            .declarations
            .get(instance)
            .ok_or(oauth::INSTANCE_NOT_DECLARED)?;
        let tokens = PooledTokens {
            runtime: self,
            declaration,
            budget,
        };
        refresh::with_access_token(&tokens, scheme, attempt).await
    }

    /// One pooled connection, checked out inside the attempt's own budget.
    ///
    /// `deadpool`'s wait timeout is a deployment setting and defaults to none,
    /// so without this bound a saturated pool turns into an unbounded wait that
    /// no deadline in this call graph can interrupt.
    async fn checkout(
        &self,
        budget: Duration,
    ) -> Result<deadpool_postgres::Client, CredentialFailure> {
        match tokio::time::timeout(budget, self.pool.get()).await {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(_)) | Err(_) => Err(oauth::DATABASE_UNAVAILABLE),
        }
    }

    /// The one provider account this instance holds.
    ///
    /// It is read per use rather than pinned at boot, so re-authorizing an
    /// instance takes effect without a restart. Spec 011 §9 keeps one instance
    /// to one account; more than one stored subject is therefore an operator
    /// error the attempt must name rather than pick between.
    async fn subject(
        &self,
        client: &tokio_postgres::Client,
        declaration: &OauthDeclaration,
    ) -> Result<String, CredentialFailure> {
        let mut subjects = store::subjects(
            client,
            &self.source,
            &declaration.connector,
            &declaration.instance,
        )
        .await
        .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;
        match subjects.len() {
            0 => Err(oauth::NO_CREDENTIAL),
            1 => Ok(subjects.remove(0)),
            _ => Err(oauth::AMBIGUOUS_CREDENTIAL),
        }
    }
}

/// The pool-backed half of one attempt's credential.
///
/// It exists so a connection's life is one token acquisition rather than one
/// whole provider attempt: each `acquire` checks a connection out, resolves the
/// account, runs the fast path or the locked exchange, and gives the connection
/// back before the caller touches the provider.
struct PooledTokens<'a> {
    runtime: &'a CredentialRuntime,
    declaration: &'a OauthDeclaration,
    budget: Duration,
}

impl refresh::AccessTokenSource for PooledTokens<'_> {
    fn acquire(&self, force: bool) -> BoxFuture<'_, Result<AccessToken, CredentialFailure>> {
        Box::pin(async move {
            let mut client = self.runtime.checkout(self.budget).await?;
            let subject = self.runtime.subject(&client, self.declaration).await?;
            refresh::access_token(
                &mut client,
                &self.runtime.key,
                self.declaration,
                &subject,
                self.runtime.exchange.as_ref(),
                RefreshOptions {
                    budget: self.budget,
                    force,
                    ..RefreshOptions::default()
                },
            )
            .await
            // `client` is dropped here, which is what returns it to the pool:
            // the provider request the caller is about to make runs with no
            // connection checked out at all.
        })
    }
}

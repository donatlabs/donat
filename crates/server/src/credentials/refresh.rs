//! Refresh at use.
//!
//! An access token is refreshed on the attempt that needs it, inside that
//! attempt's own deadline, and never by a background loop. Spec 011 §6 is
//! explicit about why: a proactive refresher is one more thing that has to be
//! drained on `SIGTERM` (see
//! [[operations/decisions/001-bounded-and-drainable-by-default]]), and it would
//! still not remove the need for the on-use path, because an access token can
//! expire between the loop's last pass and the request.
//!
//! Two properties are load-bearing here, and both are the transaction's doing
//! rather than the process's:
//!
//! **Single flight.** The refresh runs under `SELECT … FOR UPDATE` on that one
//! credential row. A second claimer blocks on the lock, and under READ
//! COMMITTED its `SELECT … FOR UPDATE` re-reads the row the first writer
//! committed — so it finds a fresh token and performs no second exchange. This
//! holds across connections, across processes, and across binaries, which no
//! in-process cache does.
//!
//! **Rotation.** A provider that returns a new refresh token invalidates the
//! old one at that instant. The new value is therefore committed before it is
//! used, and the value handed to the caller is read back from the row the
//! database wrote (`RETURNING`). A crash between the exchange and the commit
//! rolls the transaction back and loses one attempt; it never leaves a stored
//! token nobody can use, and it never marks the credential unusable.

use std::fmt;
use std::time::Duration;

use chrono::Utc;
use futures_util::future::BoxFuture;

use super::declaration::OauthDeclaration;
use super::keys::{CredentialIdentity, SealingKey, SecretBytes};
use super::oauth::{
    self, CredentialErrorClass, CredentialFailure, GrantRequest, TokenExchange, TokenRequest,
};
use super::store;

/// When a provider does not say how long an access token lives. RFC 6749 makes
/// `expires_in` optional; an hour is the common default and, being a floor
/// rather than a promise, only costs one extra refresh if it is wrong.
const ASSUMED_ACCESS_TOKEN_LIFETIME: Duration = Duration::from_secs(3600);

const REFRESH_INTERRUPTED: CredentialFailure = CredentialFailure::new(
    CredentialErrorClass::Transport,
    "credential_refresh_interrupted",
    "the credential refresh was interrupted before it committed; the stored credential is \
     unchanged",
);

const STILL_UNAUTHORIZED: CredentialFailure = CredentialFailure::new(
    CredentialErrorClass::Authentication,
    "provider_rejected_refreshed_token",
    "the provider rejected the request again after the access token was refreshed",
);

/// A live access token, and the header it makes.
///
/// It exists for one attempt. It has no `Display`, its `Debug` is a constant,
/// and the bytes are wiped when it is dropped — which is what "drop the applied
/// header after the attempt" means in practice.
pub struct AccessToken {
    token: SecretBytes,
    pub expires_at: chrono::DateTime<Utc>,
    pub subject: String,
    /// How many times this credential has rotated, after this call.
    pub rotation_count: i64,
    /// Whether this call performed a token exchange.
    pub refreshed: bool,
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccessToken")
            .field("token", &"redacted")
            .field("expires_at", &self.expires_at)
            .field("subject", &self.subject)
            .field("rotation_count", &self.rotation_count)
            .field("refreshed", &self.refreshed)
            .finish()
    }
}

impl AccessToken {
    /// The `Authorization` header value for one attempt, in the scheme the
    /// connector this token is for publishes.
    ///
    /// It is built on demand rather than stored, so there is no second copy of
    /// the token sitting in this struct waiting to be logged.
    ///
    /// The scheme is the *connector's*, not this module's. RFC 6750's `Bearer`
    /// is what every provider in this workspace but one publishes, and Zoho CRM
    /// publishes `Zoho-oauthtoken` — so the caller names it and the connector's
    /// own auth plan refuses a header in any other shape, which is what keeps
    /// the two halves from drifting apart.
    pub fn authorization_header(&self, scheme: &str) -> Result<AppliedHeader, CredentialFailure> {
        let token = self
            .token
            .expose_str()
            .map_err(|_| oauth::SEALED_VALUE_UNOPENABLE)?;
        Ok(AppliedHeader(SecretBytes::new(
            format!("{scheme} {token}").into_bytes(),
        )))
    }
}

/// One applied `Authorization` header. Same rules as the token itself.
pub struct AppliedHeader(SecretBytes);

impl AppliedHeader {
    pub fn expose(&self) -> &str {
        self.0
            .expose_str()
            .expect("an applied header is built from UTF-8")
    }
}

impl fmt::Debug for AppliedHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AppliedHeader(redacted)")
    }
}

/// How a refresh is allowed to behave.
#[derive(Debug, Clone, Copy)]
pub struct RefreshOptions {
    /// The exchange runs under the same call, byte, and deadline budget as the
    /// operation that needed it.
    pub budget: Duration,
    /// Refresh even if the stored access token still looks fresh. This is what
    /// a mid-attempt `401` means: the provider disagrees with our expiry.
    pub force: bool,
    /// Fault injection for `oauth_rotation_is_atomic`: abort the transaction
    /// after the provider exchange and before the commit, which is exactly
    /// what a crash at that instant does. Aborting is always safe, so this
    /// cannot make a production path less correct — it can only lose an
    /// attempt.
    pub abort_before_commit: bool,
}

impl Default for RefreshOptions {
    fn default() -> Self {
        Self {
            budget: Duration::from_secs(20),
            force: false,
            abort_before_commit: false,
        }
    }
}

/// Get a usable access token for one provider account, refreshing if needed.
pub async fn access_token(
    client: &mut tokio_postgres::Client,
    key: &SealingKey,
    declaration: &OauthDeclaration,
    subject: &str,
    exchange: &dyn TokenExchange,
    options: RefreshOptions,
) -> Result<AccessToken, CredentialFailure> {
    let identity = declaration.identity(subject);

    if !options.force {
        // The fast path takes no lock at all: a comfortably fresh token is the
        // overwhelmingly common case and must not serialize attempts.
        let row = store::read(&*client, &identity)
            .await
            .map_err(|_| oauth::DATABASE_UNAVAILABLE)?
            .ok_or(oauth::NO_CREDENTIAL)?;
        check_usable(&row, declaration)?;
        if row.access_expires_at > Utc::now() + declaration.refresh_skew {
            let token = key
                .open(&identity, &row.access_token_sealed)
                .map_err(|_| oauth::SEALED_VALUE_UNOPENABLE)?;
            return Ok(AccessToken {
                token,
                expires_at: row.access_expires_at,
                subject: subject.to_owned(),
                rotation_count: row.rotation_count,
                refreshed: false,
            });
        }
    }

    refresh_locked(client, key, declaration, &identity, exchange, options).await
}

/// Where one attempt's access token comes from.
///
/// A whole attempt needs a token at most twice — once to try, once after the
/// provider disagrees with our expiry — and needs a *database* for neither the
/// provider request nor the replay. Handing [`with_access_token`] an acquirer
/// rather than a connection is what keeps that true: the caller decides how a
/// connection is obtained and, more importantly, when it goes back.
///
/// The row lock is still the single-flight mechanism and it is still held across
/// the token endpoint call — one bounded exchange
/// ([[declarative-saas/decisions/061-a-locked-row-is-held-for-a-bounded-exchange-and-a-grant-may-not-narrow-under-it]]).
/// It is simply not held across a provider request nobody can bound.
pub trait AccessTokenSource: Sync {
    /// A usable access token. `force` is the mid-attempt `401`: refresh even
    /// though the stored token still looks fresh.
    fn acquire(&self, force: bool) -> BoxFuture<'_, Result<AccessToken, CredentialFailure>>;
}

/// Run one provider attempt with a live header, replaying once if the provider
/// says the token is not good after all.
///
/// This is the seam the connector executor calls. It owns the header's whole
/// life: the attempt borrows it, and it is dropped — and wiped — when this
/// function returns.
///
/// `authorization_scheme` is the *connector's*, not this module's: RFC 6750's
/// `Bearer` is what every provider in this workspace but one publishes, and Zoho
/// CRM publishes `Zoho-oauthtoken`. The caller reads it off the connector's own
/// declared auth plan, and that plan refuses a header in any other shape — which
/// is what keeps the two halves from drifting apart
/// ([[declarative-saas/decisions/064-a-credentials-scheme-and-its-username-are-the-providers]]).
pub async fn with_access_token<T>(
    tokens: &dyn AccessTokenSource,
    authorization_scheme: &str,
    mut attempt: impl for<'a> FnMut(
        &'a AppliedHeader,
    ) -> BoxFuture<'a, Result<Attempt<T>, CredentialFailure>>,
) -> Result<T, CredentialFailure> {
    let token = tokens.acquire(false).await?;
    {
        let header = token.authorization_header(authorization_scheme)?;
        match attempt(&header).await? {
            Attempt::Done(value) => return Ok(value),
            Attempt::Unauthorized => {}
        }
    }
    drop(token);

    // Exactly one refresh and exactly one replay. A second `Unauthorized` is
    // the provider's answer, not a reason to keep asking.
    let refreshed = tokens.acquire(true).await?;
    let header = refreshed.authorization_header(authorization_scheme)?;
    match attempt(&header).await? {
        Attempt::Done(value) => Ok(value),
        Attempt::Unauthorized => Err(STILL_UNAUTHORIZED),
    }
}

/// What one provider attempt concluded.
#[derive(Debug)]
pub enum Attempt<T> {
    Done(T),
    /// The provider answered `401`. The caller refreshes once and replays.
    Unauthorized,
}

fn check_usable(
    row: &store::CredentialRow,
    declaration: &OauthDeclaration,
) -> Result<(), CredentialFailure> {
    if row.unusable_reason.is_some() {
        return Err(oauth::CREDENTIAL_UNUSABLE);
    }
    // The stored row remembers where it was minted. An instance that now
    // points somewhere else is not the instance this credential belongs to,
    // and the sealed bytes would not open under the new AAD anyway — failing
    // here says why instead of reporting a decryption failure.
    if !oauth::same_endpoint(&declaration.token_endpoint, &row.identity.token_origin) {
        return Err(oauth::FOREIGN_TOKEN_ORIGIN);
    }
    Ok(())
}

async fn refresh_locked(
    client: &mut tokio_postgres::Client,
    key: &SealingKey,
    declaration: &OauthDeclaration,
    identity: &CredentialIdentity,
    exchange: &dyn TokenExchange,
    options: RefreshOptions,
) -> Result<AccessToken, CredentialFailure> {
    let transaction = client
        .transaction()
        .await
        .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;

    let row = store::lock(&transaction, identity)
        .await
        .map_err(|_| oauth::DATABASE_UNAVAILABLE)?
        .ok_or(oauth::NO_CREDENTIAL)?;
    check_usable(&row, declaration)?;

    // Re-checked *after* the lock, against the clock as it is now: this is
    // where the second concurrent claimer discovers the first one's work.
    if !options.force && row.access_expires_at > Utc::now() + declaration.refresh_skew {
        let token = key
            .open(identity, &row.access_token_sealed)
            .map_err(|_| oauth::SEALED_VALUE_UNOPENABLE)?;
        transaction
            .commit()
            .await
            .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;
        return Ok(AccessToken {
            token,
            expires_at: row.access_expires_at,
            subject: identity.subject.clone(),
            rotation_count: row.rotation_count,
            refreshed: false,
        });
    }

    let sealed_refresh = row
        .refresh_token_sealed
        .as_ref()
        .ok_or(oauth::NO_REFRESH_TOKEN)?;
    let refresh_token = key
        .open(identity, sealed_refresh)
        .map_err(|_| oauth::SEALED_VALUE_UNOPENABLE)?;

    let grant = exchange
        .exchange(TokenRequest {
            token_endpoint: &declaration.token_endpoint,
            client_id: declaration.client_id(),
            client_secret: declaration.client_secret(),
            grant: GrantRequest::Refresh {
                refresh_token: refresh_token
                    .expose_str()
                    .map_err(|_| oauth::SEALED_VALUE_UNOPENABLE)?,
            },
            scopes: &row.scopes,
            budget: options.budget,
        })
        .await;

    let grant = match grant {
        Ok(grant) => grant,
        Err(failure) if failure.is_invalid_grant() => {
            // Permanent for this row: mark it, keep it, and never come back.
            // The mark is committed, because a failure nobody records is a
            // failure the next attempt repeats.
            store::mark_unusable(&transaction, identity, oauth::INVALID_GRANT)
                .await
                .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;
            transaction
                .commit()
                .await
                .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;
            return Err(oauth::INVALID_GRANT_FAILURE);
        }
        Err(failure) => return Err(failure),
    };

    if !oauth::same_endpoint(&declaration.token_endpoint, &grant.issued_by) {
        return Err(oauth::FOREIGN_TOKEN_ORIGIN);
    }

    // A refresh is the same grant arriving again, and it is held to the rule
    // `authorize::complete` applies to the first one: what the provider granted
    // has to cover what the instance declares. Writing a narrower set instead
    // would leave the deployment dispatching operations that grant no longer
    // authorizes — opaque provider `403`s now, and a startup `ScopeShortfall`
    // at the next restart, neither of which names the refresh that caused it.
    //
    // The row is marked rather than merely left alone: only an operator can
    // fix it, and a provider that narrowed once narrows again, so an unmarked
    // row would exchange (and, at a rotating provider, burn) its refresh token
    // on every attempt.
    let missing = oauth::scope_shortfall(&declaration.scopes, grant.granted_scopes.as_deref());
    if !missing.is_empty() {
        store::mark_unusable(&transaction, identity, oauth::SCOPE_SHORTFALL)
            .await
            .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;
        transaction
            .commit()
            .await
            .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;
        return Err(oauth::SCOPE_SHORTFALL_FAILURE);
    }

    if options.abort_before_commit {
        // Dropping the transaction rolls it back, which is what a crash here
        // does. The row keeps the refresh token it had.
        drop(transaction);
        return Err(REFRESH_INTERRUPTED);
    }

    let scopes = grant
        .granted_scopes
        .clone()
        .unwrap_or_else(|| row.scopes.clone());
    let expires_at = Utc::now()
        + chrono::Duration::from_std(grant.expires_in.unwrap_or(ASSUMED_ACCESS_TOKEN_LIFETIME))
            .unwrap_or_else(|_| chrono::Duration::seconds(3600));
    let sealed_access = key.seal(identity, grant.access_token.expose());
    let sealed_refresh = grant
        .refresh_token
        .as_ref()
        .map(|token| key.seal(identity, token.expose()));

    let stored_access = store::rotate(
        &transaction,
        identity,
        &sealed_access,
        expires_at,
        sealed_refresh.as_deref(),
        &scopes,
    )
    .await
    .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;

    transaction
        .commit()
        .await
        .map_err(|_| oauth::DATABASE_UNAVAILABLE)?;

    // Only now — after the commit — is the token opened and handed out. The
    // bytes come from the row, so what the caller uses is what is stored.
    let token = key
        .open(identity, &stored_access)
        .map_err(|_| oauth::SEALED_VALUE_UNOPENABLE)?;
    Ok(AccessToken {
        token,
        expires_at,
        subject: identity.subject.clone(),
        rotation_count: row.rotation_count + 1,
        refreshed: true,
    })
}

//! The source-local credential table.
//!
//! Credential state lives in the same Postgres source as the Process that uses
//! the connector instance, so a credential and the work that needs it commit
//! against the same database and there is no second store to keep in step.
//!
//! Nothing in this module opens a sealed value. It moves opaque byte strings
//! in and out of columns, which is why its types can be `Debug`-printed in a
//! diagnostic without leaking anything — the sealed columns are still redacted,
//! because a ciphertext in a log is a ciphertext an attacker gets to keep.

use std::fmt;

use chrono::{DateTime, Utc};
use tokio_postgres::{GenericClient, Row};

use super::keys::CredentialIdentity;

/// One stored credential, exactly as the columns hold it.
pub struct CredentialRow {
    pub identity: CredentialIdentity,
    pub access_token_sealed: Vec<u8>,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_token_sealed: Option<Vec<u8>>,
    pub scopes: Vec<String>,
    pub rotated_at: DateTime<Utc>,
    pub rotation_count: i64,
    /// Set once the provider refused the refresh token. The row is kept.
    pub unusable_reason: Option<String>,
}

impl fmt::Debug for CredentialRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialRow")
            .field("identity", &self.identity)
            .field("access_token_sealed", &"redacted")
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_token_sealed", &self.refresh_token_sealed.is_some())
            .field("scopes", &self.scopes)
            .field("rotated_at", &self.rotated_at)
            .field("rotation_count", &self.rotation_count)
            .field("unusable_reason", &self.unusable_reason)
            .finish()
    }
}

impl CredentialRow {
    fn from_row(row: &Row) -> Self {
        Self {
            identity: CredentialIdentity {
                source: row.get("source"),
                connector: row.get("connector"),
                instance: row.get("instance"),
                subject: row.get("subject"),
                token_origin: row.get("token_origin"),
            },
            access_token_sealed: row.get("access_token"),
            access_expires_at: row.get("access_expires_at"),
            refresh_token_sealed: row.get("refresh_token"),
            scopes: row.get("scopes"),
            rotated_at: row.get("rotated_at"),
            rotation_count: row.get("rotation_count"),
            unusable_reason: row.get("unusable_reason"),
        }
    }
}

/// What an operator is allowed to see. There is no column here that a secret
/// could hide in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSummary {
    pub connector: String,
    pub instance: String,
    pub subject: String,
    pub scopes: Vec<String>,
    pub token_origin: String,
    pub access_expires_at: DateTime<Utc>,
    pub rotated_at: DateTime<Utc>,
    pub rotation_count: i64,
    pub unusable_reason: Option<String>,
}

const COLUMNS: &str = "source, connector, instance, subject, access_token, access_expires_at, \
                       refresh_token, scopes, token_origin, rotated_at, rotation_count, \
                       unusable_reason";

/// Read one credential without taking a lock. This is the fast path: an access
/// token that is comfortably fresh needs no transaction at all.
pub async fn read<C: GenericClient>(
    client: &C,
    identity: &CredentialIdentity,
) -> Result<Option<CredentialRow>, tokio_postgres::Error> {
    let row = client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM donat.connector_credential \
                 WHERE source = $1 AND connector = $2 AND instance = $3 AND subject = $4"
            ),
            &[
                &identity.source,
                &identity.connector,
                &identity.instance,
                &identity.subject,
            ],
        )
        .await?;
    Ok(row.as_ref().map(CredentialRow::from_row))
}

/// Read one credential *and hold it* until the surrounding transaction ends.
///
/// This is the entire single-flight mechanism. Under READ COMMITTED a
/// `SELECT … FOR UPDATE` that blocks on another transaction's lock re-reads
/// the row after that transaction commits, so the second claimer observes the
/// first one's refresh and does not perform a second exchange. It works across
/// processes and across binaries, which an in-process cache does not.
///
/// The surrounding transaction must be READ COMMITTED (Postgres' default): a
/// REPEATABLE READ transaction would abort here with a serialization failure
/// instead of seeing the new token.
pub async fn lock<C: GenericClient>(
    transaction: &C,
    identity: &CredentialIdentity,
) -> Result<Option<CredentialRow>, tokio_postgres::Error> {
    let row = transaction
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM donat.connector_credential \
                 WHERE source = $1 AND connector = $2 AND instance = $3 AND subject = $4 \
                 FOR UPDATE"
            ),
            &[
                &identity.source,
                &identity.connector,
                &identity.instance,
                &identity.subject,
            ],
        )
        .await?;
    Ok(row.as_ref().map(CredentialRow::from_row))
}

/// Write the first credential for one identity, or replace it wholesale after
/// a fresh authorization.
///
/// A re-authorization resets `rotation_count` and clears any unusable mark:
/// this is a new grant, not a rotation of the old one.
#[allow(clippy::too_many_arguments)]
pub async fn upsert<C: GenericClient>(
    client: &C,
    identity: &CredentialIdentity,
    access_token_sealed: &[u8],
    access_expires_at: DateTime<Utc>,
    refresh_token_sealed: Option<&[u8]>,
    scopes: &[String],
) -> Result<(), tokio_postgres::Error> {
    let refresh: Option<&[u8]> = refresh_token_sealed;
    client
        .execute(
            "INSERT INTO donat.connector_credential (
                 source, connector, instance, subject,
                 access_token, access_expires_at, refresh_token, scopes,
                 token_origin, rotated_at, rotation_count
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now(), 0)
             ON CONFLICT (source, connector, instance, subject) DO UPDATE SET
                 access_token = excluded.access_token,
                 access_expires_at = excluded.access_expires_at,
                 refresh_token = excluded.refresh_token,
                 scopes = excluded.scopes,
                 token_origin = excluded.token_origin,
                 rotated_at = now(),
                 rotation_count = 0,
                 unusable_reason = NULL,
                 unusable_at = NULL",
            &[
                &identity.source,
                &identity.connector,
                &identity.instance,
                &identity.subject,
                &access_token_sealed,
                &access_expires_at,
                &refresh,
                &scopes,
                &identity.token_origin,
            ],
        )
        .await?;
    Ok(())
}

/// Commit a rotation and hand back exactly the bytes the row now holds.
///
/// `RETURNING` is the point: the value the caller goes on to use is the value
/// the database wrote, not a copy of what the provider said. Combined with the
/// caller committing before it opens them, that is what makes "the new refresh
/// token is committed before it is used" structural.
///
/// A `None` `refresh_token_sealed` leaves the stored one in place: a provider
/// that does not rotate simply keeps the token it already issued.
pub async fn rotate<C: GenericClient>(
    transaction: &C,
    identity: &CredentialIdentity,
    access_token_sealed: &[u8],
    access_expires_at: DateTime<Utc>,
    refresh_token_sealed: Option<&[u8]>,
    scopes: &[String],
) -> Result<Vec<u8>, tokio_postgres::Error> {
    let refresh: Option<&[u8]> = refresh_token_sealed;
    let row = transaction
        .query_one(
            "UPDATE donat.connector_credential SET
                 access_token = $5,
                 access_expires_at = $6,
                 refresh_token = COALESCE($7, refresh_token),
                 scopes = $8,
                 rotated_at = now(),
                 rotation_count = rotation_count + 1
             WHERE source = $1 AND connector = $2 AND instance = $3 AND subject = $4
             RETURNING access_token",
            &[
                &identity.source,
                &identity.connector,
                &identity.instance,
                &identity.subject,
                &access_token_sealed,
                &access_expires_at,
                &refresh,
                &scopes,
            ],
        )
        .await?;
    Ok(row.get(0))
}

/// Mark a credential the provider refused. The row is kept so an operator can
/// see what happened; it is never refreshed again.
pub async fn mark_unusable<C: GenericClient>(
    transaction: &C,
    identity: &CredentialIdentity,
    reason: &str,
) -> Result<(), tokio_postgres::Error> {
    transaction
        .execute(
            "UPDATE donat.connector_credential
             SET unusable_reason = $5, unusable_at = now()
             WHERE source = $1 AND connector = $2 AND instance = $3 AND subject = $4",
            &[
                &identity.source,
                &identity.connector,
                &identity.instance,
                &identity.subject,
                &reason,
            ],
        )
        .await?;
    Ok(())
}

/// Everything an operator may list, for one source.
pub async fn list<C: GenericClient>(
    client: &C,
    source: &str,
) -> Result<Vec<CredentialSummary>, tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT connector, instance, subject, scopes, token_origin, access_expires_at, \
                    rotated_at, rotation_count, unusable_reason
             FROM donat.connector_credential
             WHERE source = $1
             ORDER BY connector, instance, subject",
            &[&source],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| CredentialSummary {
            connector: row.get("connector"),
            instance: row.get("instance"),
            subject: row.get("subject"),
            scopes: row.get("scopes"),
            token_origin: row.get("token_origin"),
            access_expires_at: row.get("access_expires_at"),
            rotated_at: row.get("rotated_at"),
            rotation_count: row.get("rotation_count"),
            unusable_reason: row.get("unusable_reason"),
        })
        .collect())
}

/// The provider accounts stored for one connector instance.
///
/// The request path needs this because an activity names an *instance*, and the
/// credential row is keyed by instance plus the provider's own account. Spec
/// 011 §9 keeps one instance to one account, so the expected answer is exactly
/// one; returning the list rather than an `Option` lets the caller say which of
/// "none" and "more than one" it found.
pub async fn subjects<C: GenericClient>(
    client: &C,
    source: &str,
    connector: &str,
    instance: &str,
) -> Result<Vec<String>, tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT subject FROM donat.connector_credential
             WHERE source = $1 AND connector = $2 AND instance = $3
             ORDER BY subject",
            &[&source, &connector, &instance],
        )
        .await?;
    Ok(rows.iter().map(|row| row.get("subject")).collect())
}

/// Delete one credential. Returns whether a row was there.
pub async fn delete<C: GenericClient>(
    client: &C,
    identity: &CredentialIdentity,
) -> Result<bool, tokio_postgres::Error> {
    let deleted = client
        .execute(
            "DELETE FROM donat.connector_credential
             WHERE source = $1 AND connector = $2 AND instance = $3 AND subject = $4",
            &[
                &identity.source,
                &identity.connector,
                &identity.instance,
                &identity.subject,
            ],
        )
        .await?;
    Ok(deleted > 0)
}

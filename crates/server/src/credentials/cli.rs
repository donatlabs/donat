//! The three deploy-time commands.
//!
//! This is the whole authorization surface of the product. There is no HTTP
//! route that starts, completes, or displays an authorization, and there is no
//! runtime API that lists or mutates a credential — which is the same rule the
//! rest of the engine follows: configuration happens at deploy time, by an
//! operator with database access, not over the wire.
//!
//! `authorize` writes. `list` and `revoke` are the read-only companions, and
//! `revoke` only ever deletes.

use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use donat_metadata::Metadata;

use super::authorize;
use super::declaration::OauthDeclaration;
use super::keys::SealingKey;
use super::oauth::HttpTokenExchange;
use super::store;

/// How long a token exchange run from a terminal gets.
const CLI_EXCHANGE_BUDGET: Duration = Duration::from_secs(30);

/// How long `--listen` waits for the provider to redirect the operator back.
const CLI_LISTEN_TIMEOUT: Duration = Duration::from_secs(300);

/// Which connector instance a command is about.
#[derive(Debug, Clone)]
pub struct ConnectorTarget {
    pub source: String,
    pub instance: String,
    /// The module name, when the operator spelled it. Checked against the
    /// instance's declared module rather than trusted: two names that disagree
    /// mean the operator is thinking of a different instance.
    pub connector: Option<String>,
}

async fn connect(database_url: &str) -> Result<tokio_postgres::Client> {
    let (client, connection) = tokio_postgres::connect(database_url, crate::pgtls::connector())
        .await
        .context("connecting to the source database")?;
    // The connection task ends when the client is dropped at the end of the
    // command; a CLI process exits immediately afterwards.
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

fn load_metadata(metadata_dir: &Path) -> Result<Metadata> {
    donat_metadata::load_metadata_dir(metadata_dir)
        .with_context(|| format!("failed to load metadata from {}", metadata_dir.display()))
}

fn resolve(metadata: &Metadata, target: &ConnectorTarget) -> Result<OauthDeclaration> {
    let declaration =
        OauthDeclaration::resolve(metadata, &target.source, &target.instance, &|name| {
            std::env::var(name).ok()
        })?;
    if let Some(connector) = &target.connector
        && connector != &declaration.connector
    {
        bail!(
            "connector instance `{}` uses module `{}`, not `{connector}`",
            declaration.instance,
            declaration.connector
        );
    }
    Ok(declaration)
}

/// `donat connector authorize`.
///
/// It prints a URL, waits, and writes one sealed row. It prints the subject,
/// the granted scopes, and the expiry; it prints no token, and neither does
/// any error it can produce.
pub async fn authorize(
    database_url: &str,
    metadata_dir: &Path,
    target: &ConnectorTarget,
    subject: Option<&str>,
    listen: Option<u16>,
) -> Result<()> {
    let key = SealingKey::from_env()?;
    let metadata = load_metadata(metadata_dir)?;
    let declaration = resolve(&metadata, target)?;

    let request = authorize::begin(&declaration);
    println!("Open this URL, approve the access, and come back:\n");
    println!("  {}\n", request.url);

    let redirected_url = match listen {
        Some(port) => {
            println!(
                "Waiting on http://127.0.0.1:{port} for the provider to redirect you back \
                 (loopback only, one request)…"
            );
            authorize::capture_redirect(&declaration, port, CLI_LISTEN_TIMEOUT).await?
        }
        None => {
            print!("Paste the full address you were redirected to: ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .context("reading the pasted redirect")?;
            line
        }
    };

    let redirected = authorize::parse_redirect(&declaration, &request, &redirected_url)?;
    let exchange = HttpTokenExchange::new();
    let written = authorize::complete(
        &mut connect(database_url).await?,
        &key,
        &declaration,
        &exchange,
        &redirected,
        subject,
        CLI_EXCHANGE_BUDGET,
    )
    .await?;

    println!("\nauthorized");
    println!("  connector      {}", declaration.connector);
    println!("  instance       {}", declaration.instance);
    println!("  subject        {}", written.subject);
    println!("  scopes         {}", written.scopes.join(" "));
    println!("  access expires {}", written.access_expires_at);
    println!(
        "  refresh token  {}",
        if written.has_refresh_token {
            "stored"
        } else {
            "not issued by the provider; re-authorize before the access token expires"
        }
    );
    Ok(())
}

/// `donat connector credentials list`.
///
/// Read-only, and there is no column here a secret could hide in. It exits
/// non-zero when an instance that declares OAuth2 has no credential, because
/// that deployment cannot run the activities that need it and should not be
/// discovered at the first attempt.
pub async fn list(database_url: &str, metadata_dir: Option<&Path>, source: &str) -> Result<()> {
    let client = connect(database_url).await?;
    let stored = store::list(&client, source)
        .await
        .context("reading the credential store")?;

    if stored.is_empty() {
        println!("no stored credentials for source `{source}`");
    }
    for credential in &stored {
        println!("{} / {}", credential.connector, credential.instance);
        println!("  subject        {}", credential.subject);
        println!("  scopes         {}", credential.scopes.join(" "));
        println!("  token origin   {}", credential.token_origin);
        println!("  access expires {}", credential.access_expires_at);
        println!(
            "  rotated        {} ({}x)",
            credential.rotated_at, credential.rotation_count
        );
        if let Some(reason) = &credential.unusable_reason {
            println!("  UNUSABLE       {reason} — re-authorize this instance");
        }
    }

    // Metadata is optional for a listing, and required to answer "is anything
    // missing". Without it the command reports what is stored and says nothing
    // about what should be.
    let Some(metadata_dir) = metadata_dir else {
        return Ok(());
    };
    let metadata = load_metadata(metadata_dir)?;
    let missing: Vec<&str> = metadata
        .connectors
        .iter()
        .filter(|instance| instance.config.oauth2.is_some())
        .filter(|instance| {
            !stored
                .iter()
                .any(|credential| credential.instance == instance.name)
        })
        .map(|instance| instance.name.as_str())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    for instance in &missing {
        eprintln!(
            "connector instance `{instance}` declares OAuth2 but has no credential in source \
             `{source}`; run `donat connector authorize`"
        );
    }
    bail!(
        "{} configured connector instance(s) have no credential",
        missing.len()
    );
}

/// `donat connector credentials revoke`.
///
/// Tells the provider first, when it publishes a revocation endpoint, then
/// deletes the row. A provider that refuses is reported and the row still
/// goes: an operator revoking a credential wants it out of the deployment
/// whatever the provider does with its own copy.
pub async fn revoke(
    database_url: &str,
    metadata_dir: &Path,
    target: &ConnectorTarget,
    subject: &str,
) -> Result<()> {
    let key = SealingKey::from_env()?;
    let metadata = load_metadata(metadata_dir)?;
    let declaration = resolve(&metadata, target)?;
    let identity = declaration.identity(subject);

    let client = connect(database_url).await?;
    let row = store::read(&client, &identity)
        .await
        .context("reading the credential store")?
        .ok_or_else(|| {
            anyhow!(
                "connector instance `{}` has no credential for subject `{subject}` in source `{}`",
                declaration.instance,
                declaration.source
            )
        })?;

    if declaration.revocation_endpoint.is_some()
        && let Some(sealed) = &row.refresh_token_sealed
    {
        let refresh = key.open(&identity, sealed)?;
        let token = refresh.expose_str()?;
        match authorize::revoke_at_provider(&declaration, token, CLI_EXCHANGE_BUDGET).await {
            Ok(()) => println!("provider revocation accepted"),
            Err(failure) => eprintln!("provider revocation failed ({failure}); deleting anyway"),
        }
    }

    let deleted = store::delete(&client, &identity)
        .await
        .context("deleting the credential")?;
    if deleted {
        println!(
            "revoked {} / {} for subject {subject}",
            declaration.connector, declaration.instance
        );
    }
    Ok(())
}

//! How the engine reaches a Postgres that expects TLS.
//!
//! Every connection the engine opens — the request pools, `migrate`,
//! `validate`, the event loop, the reconciler — goes through
//! [`connector`]. Before it existed each of those hard-coded
//! `tokio_postgres::NoTls`, which is not "TLS is terminated elsewhere" but
//! "TLS is impossible": a URL with `sslmode=require` fails to connect at all,
//! and that is the default posture of every managed Postgres there is.
//!
//! The mode still comes from the URL, because that is where libpq puts it and
//! where a deployment already expects to set it. `sslmode=disable` keeps a
//! plaintext socket, the default `prefer` uses TLS when the server offers it,
//! and `require`/`verify-ca`/`verify-full` now work instead of failing.
//!
//! Roots come from the host's trust store, which is what a managed provider's
//! public CA is in. A deployment behind a private CA names its bundle in
//! `DONAT_PG_SSL_ROOT_CERT`; that replaces the host store rather than adding
//! to it, so a certificate outside the named bundle is refused.

use std::sync::Arc;

use rustls::{ClientConfig, RootCertStore};

/// The TLS connector every Postgres connection in the process shares.
///
/// Built once: assembling a root store means reading and parsing the host's
/// certificate bundle, which is not something to repeat per connection.
pub fn connector() -> tokio_postgres_rustls::MakeRustlsConnect {
    static SHARED: std::sync::OnceLock<Arc<ClientConfig>> = std::sync::OnceLock::new();
    let config = SHARED.get_or_init(|| Arc::new(build_config()));
    tokio_postgres_rustls::MakeRustlsConnect::new(config.as_ref().clone())
}

fn build_config() -> ClientConfig {
    // `ring` is the provider reqwest's rustls already installs. Installing it
    // explicitly means the process never depends on which dependency happened
    // to run first, and a second call is a no-op we deliberately ignore.
    let _ = rustls::crypto::ring::default_provider().install_default();
    ClientConfig::builder()
        .with_root_certificates(root_store())
        .with_no_client_auth()
}

fn root_store() -> RootCertStore {
    if let Some(path) = std::env::var_os("DONAT_PG_SSL_ROOT_CERT") {
        let path = std::path::PathBuf::from(path);
        match load_pem_roots(&path) {
            Ok(store) => {
                tracing::info!(
                    target: "donat::pg",
                    path = %path.display(),
                    roots = store.len(),
                    "Postgres TLS roots loaded from DONAT_PG_SSL_ROOT_CERT"
                );
                return store;
            }
            // Falling back to the host store here would silently accept a
            // different set of certificates than the deployment named, so the
            // connection is left to fail with an unverifiable peer instead.
            Err(error) => {
                tracing::error!(
                    target: "donat::pg",
                    path = %path.display(),
                    %error,
                    "cannot read DONAT_PG_SSL_ROOT_CERT; no Postgres TLS root will be trusted"
                );
                return RootCertStore::empty();
            }
        }
    }

    let mut store = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for error in &native.errors {
        tracing::debug!(target: "donat::pg", %error, "skipping a host certificate");
    }
    for certificate in native.certs {
        let _ = store.add(certificate);
    }
    if store.is_empty() {
        // An image without a certificate bundle would otherwise trust nothing
        // and fail every TLS connection with an opaque error.
        tracing::warn!(
            target: "donat::pg",
            "no host certificate store; falling back to the bundled Mozilla roots"
        );
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    store
}

fn load_pem_roots(path: &std::path::Path) -> std::io::Result<RootCertStore> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut store = RootCertStore::empty();
    let mut added = 0;
    for certificate in rustls_pemfile_certs(&mut reader)? {
        if store.add(certificate).is_ok() {
            added += 1;
        }
    }
    if added == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the file contains no certificate",
        ));
    }
    Ok(store)
}

/// Read every `CERTIFICATE` block out of a PEM file.
///
/// `rustls-pemfile` is not a dependency of this workspace, and a root bundle
/// is a simple enough format to read directly: base64 between two markers.
fn rustls_pemfile_certs(
    reader: &mut impl std::io::BufRead,
) -> std::io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use base64::Engine;

    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";

    let mut contents = String::new();
    reader.read_to_string(&mut contents)?;

    let mut certificates = Vec::new();
    let mut current: Option<String> = None;
    for line in contents.lines() {
        let line = line.trim();
        if line == BEGIN {
            current = Some(String::new());
        } else if line == END {
            let Some(body) = current.take() else { continue };
            let der = base64::engine::general_purpose::STANDARD
                .decode(body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            certificates.push(rustls::pki_types::CertificateDer::from(der));
        } else if let Some(body) = current.as_mut() {
            body.push_str(line);
        }
    }
    Ok(certificates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A root bundle is read as every certificate it holds, not just the first
    /// — a managed provider's bundle routinely carries several.
    #[test]
    fn reads_every_certificate_in_a_bundle() {
        // Two syntactically valid PEM blocks. The bytes need not be a real
        // certificate: this asserts the framing, which is what the reader owns.
        let pem = format!(
            "# a comment providers like to include\n\
             -----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n\
             -----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n",
            body = "AQID"
        );
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(pem));
        let certificates = rustls_pemfile_certs(&mut reader).expect("the bundle parses");
        assert_eq!(certificates.len(), 2);
        assert_eq!(certificates[0].as_ref(), &[1, 2, 3]);
    }

    /// A named bundle that holds no certificate is an error, not an empty
    /// trust store that silently rejects every connection later.
    #[test]
    fn a_bundle_without_certificates_is_an_error() {
        let path = std::env::temp_dir().join(format!("donat-pg-roots-{}.pem", std::process::id()));
        let mut file = std::fs::File::create(&path).expect("the test bundle is created");
        writeln!(file, "no certificate here").expect("the test bundle is written");
        drop(file);

        let error = load_pem_roots(&path).expect_err("an empty bundle is refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&path);
    }

    /// Building the connector must not panic on a host with no certificate
    /// store: it falls back to the bundled roots.
    #[test]
    fn a_connector_is_always_buildable() {
        let _ = connector();
    }
}

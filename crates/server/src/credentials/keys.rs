//! The sealing key, the bytes it protects, and the identity it binds them to.
//!
//! Token columns are sealed with AES-256-GCM. The key is 32 bytes, base64, in
//! `DONAT_CREDENTIAL_KEY`, resolved at boot exactly like any other secret: it
//! is named in the environment and never in metadata, and a missing or
//! malformed value stops the credential path from working at all while
//! reporting nothing but the variable's name.
//!
//! The additional authenticated data binds a sealed value to the exact row it
//! belongs to — `source`, `connector`, `instance`, `subject`, `token_origin`.
//! An attacker (or an accident) that moves sealed bytes into another row does
//! not move a working credential, because the AAD no longer matches and the
//! open fails closed. The fields are length-framed rather than concatenated,
//! so `("ab", "c")` and `("a", "bc")` are different AADs.

use std::fmt;

use base64::Engine as _;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

/// The environment variable the key comes from. Named in errors; its value
/// never is.
pub const CREDENTIAL_KEY_ENV: &str = "DONAT_CREDENTIAL_KEY";

/// Which row a sealed value belongs to. This is the AAD, and it is also the
/// primary key of `donat.connector_credential` plus the token origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialIdentity {
    /// Metadata source the connector instance belongs to.
    pub source: String,
    /// The connector module (`module:` in metadata).
    pub connector: String,
    /// The connector instance name (`name:` in metadata).
    pub instance: String,
    /// The provider's own account identity. Never a Donat user identity, and
    /// never part of a permission decision.
    pub subject: String,
    /// The token endpoint this credential was minted at.
    pub token_origin: String,
}

impl CredentialIdentity {
    /// The length-framed additional authenticated data for this row.
    ///
    /// The domain tag is first so that these bytes can never be confused with
    /// another AAD this workspace might one day construct, and so a version
    /// bump is a one-line change with a visible failure mode (nothing opens
    /// until it is re-sealed).
    fn aad(&self) -> Vec<u8> {
        const DOMAIN: &[u8] = b"donat.connector_credential.v1";
        let mut framed = Vec::with_capacity(
            DOMAIN.len()
                + self.source.len()
                + self.connector.len()
                + self.instance.len()
                + self.subject.len()
                + self.token_origin.len()
                + 6 * 4,
        );
        for field in [
            DOMAIN,
            self.source.as_bytes(),
            self.connector.as_bytes(),
            self.instance.as_bytes(),
            self.subject.as_bytes(),
            self.token_origin.as_bytes(),
        ] {
            framed.extend_from_slice(&(field.len() as u32).to_be_bytes());
            framed.extend_from_slice(field);
        }
        framed
    }
}

/// Plaintext that has been opened, or is about to be sealed.
///
/// It has no `Display`, its `Debug` is a constant, and it wipes itself when it
/// is dropped. The only way to see the bytes is to ask for them by name.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// The bytes as a string, when the caller knows they are one (a token is).
    pub fn expose_str(&self) -> Result<&str, SealError> {
        std::str::from_utf8(&self.0).map_err(|_| SealError::Unopenable)
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes(redacted)")
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        // Overwrite before the allocation goes back to the allocator. A
        // `write_volatile` per byte is what stops the optimizer from noticing
        // that nobody reads the zeroes.
        for byte in &mut self.0 {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

/// The key could not be read. Its message names the variable and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    Missing,
    Malformed,
}

impl fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(
                formatter,
                "{CREDENTIAL_KEY_ENV} is not set; OAuth2 connector credentials cannot be sealed \
                 or opened without it"
            ),
            Self::Malformed => write!(
                formatter,
                "{CREDENTIAL_KEY_ENV} must be exactly 32 bytes, base64-encoded"
            ),
        }
    }
}

impl std::error::Error for KeyError {}

/// A sealed value did not open. There is deliberately one variant: which part
/// failed — nonce, tag, AAD, key — is information about the key material, and
/// the caller's correct response is the same in every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealError {
    Unopenable,
}

impl fmt::Display for SealError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a stored connector credential did not open under its own identity; it was sealed \
             with a different key or belongs to a different row",
        )
    }
}

impl std::error::Error for SealError {}

/// The AES-256-GCM key that protects the token columns.
pub struct SealingKey {
    key: LessSafeKey,
    random: SystemRandom,
}

impl fmt::Debug for SealingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SealingKey(redacted)")
    }
}

impl SealingKey {
    /// Resolve the key from the environment, as every other secret is
    /// resolved. Callers do this once, at boot or at the start of a CLI
    /// command, so a deployment that cannot open its credentials finds out
    /// before it tries to use one.
    pub fn from_env() -> Result<Self, KeyError> {
        let raw = std::env::var(CREDENTIAL_KEY_ENV).map_err(|_| KeyError::Missing)?;
        Self::from_base64(&raw)
    }

    pub fn from_base64(raw: &str) -> Result<Self, KeyError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(KeyError::Missing);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|_| KeyError::Malformed)?;
        if bytes.len() != 32 {
            return Err(KeyError::Malformed);
        }
        let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| KeyError::Malformed)?;
        Ok(Self {
            key: LessSafeKey::new(key),
            random: SystemRandom::new(),
        })
    }

    /// A fresh random key, base64-encoded, for tests and for the operator who
    /// has to produce one.
    pub fn generate_base64_for_tests() -> String {
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .expect("the system random source produces 32 bytes");
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// Seal one value for one row. The result is `nonce || ciphertext || tag`.
    ///
    /// A fresh nonce per write is not an optimization: AES-GCM loses
    /// confidentiality *and* integrity if one is reused under the same key, and
    /// a rotating refresh token is written over and over under the same key.
    pub fn seal(&self, identity: &CredentialIdentity, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        self.random
            .fill(&mut nonce)
            .expect("the system random source produces a nonce");
        let mut sealed = Vec::with_capacity(NONCE_LEN + plaintext.len() + AES_256_GCM.tag_len());
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(plaintext);
        let tag = self
            .key
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(identity.aad()),
                &mut sealed[NONCE_LEN..],
            )
            .expect("AES-256-GCM seals a bounded token");
        sealed.extend_from_slice(tag.as_ref());
        sealed
    }

    /// Open one stored value under the row it claims to belong to.
    pub fn open(
        &self,
        identity: &CredentialIdentity,
        sealed: &[u8],
    ) -> Result<SecretBytes, SealError> {
        if sealed.len() < NONCE_LEN + AES_256_GCM.tag_len() {
            return Err(SealError::Unopenable);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&sealed[..NONCE_LEN]);
        let mut buffer = sealed[NONCE_LEN..].to_vec();
        let plaintext = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(identity.aad()),
                &mut buffer,
            )
            .map_err(|_| SealError::Unopenable)?;
        Ok(SecretBytes::new(plaintext.to_vec()))
    }
}

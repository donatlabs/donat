//! Sealing a token before it reaches a column.

#[cfg(test)]
mod tests {
    use super::super::{CredentialIdentity, SealError, SealingKey};

    const SENTINEL: &str = "sentinel-refresh-token-do-not-log-5f3a";

    fn identity() -> CredentialIdentity {
        CredentialIdentity {
            source: "default".to_owned(),
            connector: "acme".to_owned(),
            instance: "acme-main".to_owned(),
            subject: "acct_123".to_owned(),
            token_origin: "https://provider.example/oauth/token".to_owned(),
        }
    }

    fn key() -> SealingKey {
        SealingKey::from_base64(&SealingKey::generate_base64_for_tests())
            .expect("a generated key is well formed")
    }

    #[test]
    fn a_key_is_thirty_two_bytes_of_base64_and_says_only_its_variable_name() {
        let too_short = SealingKey::from_base64("c2hvcnQ=").expect_err("8 bytes is not a key");
        assert!(too_short.to_string().contains("DONAT_CREDENTIAL_KEY"));
        assert!(!too_short.to_string().contains("c2hvcnQ"));

        let not_base64 =
            SealingKey::from_base64("not base64 !!").expect_err("garbage is not a key");
        assert!(not_base64.to_string().contains("DONAT_CREDENTIAL_KEY"));
        assert!(!not_base64.to_string().contains("not base64"));

        let empty = SealingKey::from_base64("   ").expect_err("an empty value is not a key");
        assert!(empty.to_string().contains("DONAT_CREDENTIAL_KEY"));
    }

    #[test]
    fn two_writes_of_one_plaintext_produce_different_ciphertext() {
        let key = key();
        let identity = identity();
        let first = key.seal(&identity, SENTINEL.as_bytes());
        let second = key.seal(&identity, SENTINEL.as_bytes());
        assert_ne!(first, second, "each write must use a fresh nonce");
        assert!(
            !first
                .windows(SENTINEL.len())
                .any(|w| w == SENTINEL.as_bytes())
        );
        assert_eq!(
            key.open(&identity, &first)
                .expect("sealed bytes open")
                .expose(),
            SENTINEL.as_bytes()
        );
        assert_eq!(
            key.open(&identity, &second)
                .expect("sealed bytes open")
                .expose(),
            SENTINEL.as_bytes()
        );
    }

    /// The whole point of the AAD: a row lifted into another identity is not a
    /// credential any more, it is unopenable bytes.
    #[test]
    fn a_sealed_row_cannot_be_replayed_under_another_identity() {
        let key = key();
        let sealed = key.seal(&identity(), SENTINEL.as_bytes());

        for foreign in [
            CredentialIdentity {
                source: "other".to_owned(),
                ..identity()
            },
            CredentialIdentity {
                connector: "other".to_owned(),
                ..identity()
            },
            CredentialIdentity {
                instance: "other".to_owned(),
                ..identity()
            },
            CredentialIdentity {
                subject: "other".to_owned(),
                ..identity()
            },
            CredentialIdentity {
                token_origin: "https://attacker.example/token".to_owned(),
                ..identity()
            },
        ] {
            let error = key
                .open(&foreign, &sealed)
                .expect_err("a foreign identity must not open the row");
            assert!(matches!(error, SealError::Unopenable));
        }
    }

    /// Concatenating the five fields without framing would make
    /// `("ab", "c")` and `("a", "bc")` the same AAD, and two different rows
    /// interchangeable.
    #[test]
    fn the_aad_fields_cannot_be_slid_into_each_other() {
        let key = key();
        let left = CredentialIdentity {
            connector: "ab".to_owned(),
            instance: "c".to_owned(),
            ..identity()
        };
        let right = CredentialIdentity {
            connector: "a".to_owned(),
            instance: "bc".to_owned(),
            ..identity()
        };
        let sealed = key.seal(&left, SENTINEL.as_bytes());
        assert!(key.open(&right, &sealed).is_err());
    }

    #[test]
    fn a_truncated_or_tampered_row_does_not_open() {
        let key = key();
        let identity = identity();
        let sealed = key.seal(&identity, SENTINEL.as_bytes());

        assert!(key.open(&identity, &sealed[..sealed.len() - 1]).is_err());
        assert!(key.open(&identity, &[]).is_err());
        assert!(key.open(&identity, &sealed[..8]).is_err());

        let mut flipped = sealed.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 0x01;
        assert!(key.open(&identity, &flipped).is_err());

        let mut nonce_changed = sealed.clone();
        nonce_changed[0] ^= 0x01;
        assert!(key.open(&identity, &nonce_changed).is_err());
    }

    #[test]
    fn a_different_key_does_not_open_the_row() {
        let sealed = key().seal(&identity(), SENTINEL.as_bytes());
        assert!(key().open(&identity(), &sealed).is_err());
    }

    /// Neither the key nor a token may be printable, in any form, from any
    /// type this module hands out.
    #[test]
    fn nothing_here_renders_a_secret() {
        let key = key();
        let identity = identity();
        assert!(!format!("{key:?}").contains("SealingKey { key:"));
        assert_eq!(format!("{key:?}"), "SealingKey(redacted)");

        let opened = key
            .open(&identity, &key.seal(&identity, SENTINEL.as_bytes()))
            .expect("sealed bytes open");
        assert!(!format!("{opened:?}").contains(SENTINEL));
        assert_eq!(format!("{opened:?}"), "SecretBytes(redacted)");

        let error = SealError::Unopenable;
        assert!(!format!("{error} {error:?}").contains(SENTINEL));
    }
}

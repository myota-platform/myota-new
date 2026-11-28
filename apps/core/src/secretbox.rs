//! AES-256-GCM for the TOTP seed at rest.
//!
//! `account_mfa.secret_ref` is documented as "a reference into the secret store,
//! never the secret itself: this table is read by an admin console and backed up,
//! and a TOTP seed in either is a second factor that is no longer a factor."
//!
//! There is no external secret manager on this cluster, so the reference is an
//! encrypted envelope: `local:v1:<base64(nonce || ciphertext || tag)>`, with the key
//! held only by `core` in its environment. A copy of the database alone therefore
//! yields no working second factor, which is the property the column promises. The
//! `local:` prefix is the seam a real KMS slots into: the column already names a
//! provider, so moving to one changes this module and no schema.
//!
//! GCM rather than CBC or CTR because it authenticates: a flipped bit in a stored
//! seed fails to decrypt instead of producing a different seed that silently never
//! matches a code.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// The scheme prefix stored alongside the ciphertext.
const PREFIX: &str = "local:v1:";

/// Something that should have decrypted and did not.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The value is not one this module produced.
    #[error("secret: the stored reference is not a local:v1 envelope")]
    NotAnEnvelope,
    /// The value is an envelope but the key or the bytes are wrong.
    #[error("secret: the envelope could not be opened")]
    Undecryptable,
}

/// Seals a plaintext under `key`.
#[must_use]
pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> String {
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    // Encryption cannot fail for a valid key and a reasonable size; if it ever did,
    // storing the plaintext would be the wrong repair, so the process stops here.
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .expect("AES-GCM encryption with a 32-byte key cannot fail");
    let mut envelope = Vec::with_capacity(nonce.len() + ciphertext.len());
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&ciphertext);
    format!("{PREFIX}{}", BASE64.encode(envelope))
}

/// Opens a sealed value.
pub fn open(key: &[u8; 32], reference: &str) -> Result<Vec<u8>, SecretError> {
    let encoded = reference
        .strip_prefix(PREFIX)
        .ok_or(SecretError::NotAnEnvelope)?;
    let envelope = BASE64
        .decode(encoded)
        .map_err(|_| SecretError::Undecryptable)?;
    // A 12-byte nonce and a 16-byte tag are the minimum for an empty plaintext.
    if envelope.len() < 12 + 16 {
        return Err(SecretError::Undecryptable);
    }
    let (nonce_bytes, ciphertext) = envelope.split_at(12);
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| SecretError::Undecryptable)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [3_u8; 32];

    #[test]
    fn a_sealed_value_opens_to_the_original() {
        let sealed = seal(&KEY, b"JBSWY3DPEHPK3PXP");
        assert!(sealed.starts_with("local:v1:"), "{sealed}");
        assert_eq!(open(&KEY, &sealed).expect("opens"), b"JBSWY3DPEHPK3PXP");
    }

    #[test]
    fn the_plaintext_is_not_visible_in_the_envelope() {
        let sealed = seal(&KEY, b"a-recognisable-secret");
        assert!(!sealed.contains("a-recognisable-secret"), "{sealed}");
        // Nor is its base64 form.
        assert!(
            !sealed.contains(&BASE64.encode(b"a-recognisable-secret")),
            "{sealed}"
        );
    }

    #[test]
    fn sealing_the_same_plaintext_twice_differs() {
        let first = seal(&KEY, b"same");
        let second = seal(&KEY, b"same");
        assert_ne!(
            first, second,
            "a fresh nonce per seal is what makes that true"
        );
        assert_eq!(
            open(&KEY, &first).expect("opens"),
            open(&KEY, &second).expect("opens")
        );
    }

    #[test]
    fn another_key_cannot_open_it() {
        let sealed = seal(&KEY, b"secret");
        assert!(matches!(
            open(&[4_u8; 32], &sealed),
            Err(SecretError::Undecryptable)
        ));
    }

    #[test]
    fn a_tampered_envelope_is_refused_rather_than_decrypted_to_nonsense() {
        let sealed = seal(&KEY, b"secret");
        let mut envelope = BASE64
            .decode(&sealed["local:v1:".len()..])
            .expect("decodes");
        let last = envelope.len() - 1;
        envelope[last] ^= 0x01;
        let tampered = format!("local:v1:{}", BASE64.encode(envelope));
        assert!(matches!(
            open(&KEY, &tampered),
            Err(SecretError::Undecryptable)
        ));
    }

    #[test]
    fn a_plain_value_is_not_mistaken_for_an_envelope() {
        assert!(matches!(
            open(&KEY, "JBSWY3DPEHPK3PXP"),
            Err(SecretError::NotAnEnvelope)
        ));
    }

    #[test]
    fn a_truncated_envelope_is_refused_before_reaching_the_cipher() {
        let sealed = format!("local:v1:{}", BASE64.encode([0_u8; 8]));
        assert!(matches!(
            open(&KEY, &sealed),
            Err(SecretError::Undecryptable)
        ));
    }
}

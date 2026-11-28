//! Password hashing: Argon2id, with the parameters in one place.
//!
//! Argon2id rather than bcrypt or PBKDF2 because it is memory-hard as well as
//! CPU-hard, which is what makes a GPU-driven cracking rig expensive. The `argon2`
//! crate's default parameters are used deliberately rather than tuned here: they
//! are the OWASP recommendation (19 MiB, two passes, one lane), and a project that
//! invents its own either weakens them or makes every verification slower than it
//! has to be.
//!
//! Two properties matter beyond "hash the password":
//!
//!   * **The same password hashes differently every time** — the salt is random per
//!     hash, so two accounts with the same password cannot be spotted from the table.
//!   * **An unknown account costs the same as a known one** — [`verify_against_dummy`]
//!     exists so a login for an address that does not exist takes the same time as
//!     one for an address that does. Without it, response time is an account
//!     enumeration oracle.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::{Algorithm, Argon2, Params, Version};

/// Marker for a password that could not be hashed or a hash that cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    /// The hashing parameters produced a hash string that cannot be parsed back.
    /// Not reachable through the public API; a bug if it happens.
    #[error("password: the hash could not be produced")]
    Unusable,
}

/// The hasher. `Argon2::default()` is Argon2id v19 with the parameters above.
fn hasher() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::DEFAULT)
}

/// Hashes a password for storage.
///
/// The result is a PHC string, so the algorithm, version and parameters travel with
/// the hash. Raising the cost later verifies old passwords with the parameters they
/// were stored with, and rehashes on next sign-in, without a migration.
pub fn hash(password: &str) -> Result<String, PasswordError> {
    let salt = SaltString::generate(&mut OsRng);
    hasher()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordError::Unusable)
}

/// Returns whether `password` matches `encoded_hash`.
///
/// A malformed stored hash answers `false` rather than erroring: the caller's
/// decision is "may this sign in", and a broken hash means no. The distinction is
/// logged by the caller, which is where it can be acted on.
#[must_use]
pub fn verify(password: &str, encoded_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(encoded_hash) else {
        return false;
    };
    hasher()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Burns the same work as a real verification, for an account that does not exist.
///
/// The hash is generated once per process rather than per call, so the cost is one
/// verification and not one hash plus one verification. It exists at all because
/// the alternative — returning early when the address is unknown — is measurably
/// faster and turns the login endpoint into an account enumeration oracle.
#[must_use]
pub fn verify_against_dummy(password: &str) -> bool {
    use std::sync::OnceLock;
    static DUMMY: OnceLock<String> = OnceLock::new();
    let hash = DUMMY.get_or_init(|| {
        hash("a password that is never anyone's, only the shape of one")
            .unwrap_or_else(|_| String::from("$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$0000000000000000000000000000000000000000000"))
    });
    let _ = verify(password, hash);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_verifies_against_its_own_hash() {
        let encoded = hash("correct horse battery staple").expect("hashed");
        assert!(verify("correct horse battery staple", &encoded));
    }

    #[test]
    fn a_different_password_is_refused() {
        let encoded = hash("correct horse battery staple").expect("hashed");
        assert!(!verify("correct horse battery stapler", &encoded));
        assert!(!verify("", &encoded));
    }

    #[test]
    fn the_same_password_hashes_differently_every_time() {
        let first = hash("same password").expect("hashed");
        let second = hash("same password").expect("hashed");
        assert_ne!(first, second, "the salt must be random per hash");
        assert!(verify("same password", &first));
        assert!(verify("same password", &second));
    }

    #[test]
    fn the_hash_is_argon2id_at_the_documented_parameters() {
        let encoded = hash("x").expect("hashed");
        assert!(
            encoded.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{encoded}"
        );
    }

    #[test]
    fn a_malformed_hash_refuses_rather_than_panicking() {
        assert!(!verify("anything", "not-a-phc-string"));
        assert!(!verify("anything", ""));
    }

    #[test]
    fn the_dummy_verification_always_fails() {
        assert!(!verify_against_dummy("correct horse battery staple"));
        assert!(!verify_against_dummy(""));
    }
}

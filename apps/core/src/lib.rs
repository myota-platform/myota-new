//! The MyOTA identity and programme service.
//!
//! **This service owns the `myota_core` database.** No other service may connect to
//! it, and this service connects to no other service's database — see ADR-0001.
//!
//! What it does, in the order the modules are declared: reads its configuration
//! once and refuses to start on a bad value; hashes passwords with Argon2id; keeps
//! TOTP secrets encrypted at rest; issues and verifies Ed25519 access tokens and
//! publishes the key set the gateway reads; and owns the administrator sign-in
//! flow — password, then a mandatory second factor, then a session whose audience
//! the gateway enforces.
//!
//! The layering is deliberate: the primitives ([`password`], [`totp`],
//! [`secretbox`], [`token`]) know nothing about HTTP or SQL, so every property they
//! promise is testable without a database or a socket. [`auth`] is where those
//! primitives meet the tables, and [`http`] is the only layer that knows about
//! status codes.

pub mod audit;
pub mod auth;
pub mod bootstrap;
pub mod config;
pub mod db;
pub mod http;
pub mod password;
pub mod problem;
pub mod secretbox;
pub mod session;
pub mod token;
pub mod totp;

/// The service name, used in logs and in health responses.
///
/// It matches the image name and the Kubernetes workload prefix, so a log line
/// names the same thing that `kubectl` and the registry do.
pub const SERVICE_NAME: &str = "core";

/// The administrator session cookie.
///
/// `__Host-` forces `Secure`, forbids `Domain`, and pins the path to `/`. Two
/// cookies with different names cannot overwrite each other, so an operator can be
/// signed into both applications in one browser.
pub const ADMIN_SESSION_COOKIE: &str = "__Host-myota_admin_sid";

/// The participant session cookie. Named here because `core` must be able to tell
/// the two apart even while it only issues the administrator one.
pub const PARTICIPANT_SESSION_COOKIE: &str = "__Host-myota_sid";

/// Lowercase hexadecimal, the encoding every hash in this service is stored in.
///
/// One helper rather than a `format!` per call site: it keeps the encoding identical
/// across the refresh-token hash, the recovery-code hash and the rate-limit keys, and
/// it appends into one `String` instead of allocating a `String` per byte.
#[must_use]
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

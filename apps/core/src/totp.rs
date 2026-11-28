//! TOTP, per RFC 6238, with the base32 secret handling an authenticator app needs.
//!
//! The algorithm is six lines — HMAC-SHA1 over the time step, dynamic truncation,
//! modulo a million — and the rest of this module is the parts that are easy to get
//! subtly wrong:
//!
//!   * the **step** is derived from Unix time in whole seconds, so a code is valid
//!     for its 30-second window and no longer;
//!   * a **skew** of one step either side is accepted, because a phone's clock and
//!     the server's disagree by seconds and a strict window turns that into a
//!     rejected code that the user cannot debug;
//!   * the **matched step** is returned rather than a boolean, so the caller can
//!     refuse a code that was already used in the same window. A TOTP code is
//!     valid for its whole window, and accepting it twice is a replay.
//!
//! Verified against the RFC 4226 Appendix D vectors, which are the same computation
//! with the time step fixed — see the tests.

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// Digits in a generated code.
pub const DIGITS: u32 = 6;
/// Length of a time step, in seconds.
pub const STEP_SECONDS: i64 = 30;
/// Steps either side of now that are accepted. One step is thirty seconds, which
/// absorbs clock skew without widening the window to a minute and a half.
pub const DEFAULT_SKEW_STEPS: i64 = 1;
/// Bytes in a generated secret. 160 bits, the length RFC 4226 recommends.
pub const SECRET_BYTES: usize = 20;

type HmacSha1 = Hmac<Sha1>;

/// Generates a fresh secret.
#[must_use]
pub fn generate_secret() -> [u8; SECRET_BYTES] {
    use rand::RngCore;
    let mut secret = [0_u8; SECRET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    secret
}

/// Encodes a secret the way authenticator apps expect to receive it.
#[must_use]
pub fn encode_secret(secret: &[u8]) -> String {
    BASE32_NOPAD.encode(secret)
}

/// Decodes a base32 secret, ignoring case and the padding some apps add.
#[must_use]
pub fn decode_secret(encoded: &str) -> Option<Vec<u8>> {
    let normalised = encoded.trim().to_ascii_uppercase().replace('=', "");
    BASE32_NOPAD.decode(normalised.as_bytes()).ok()
}

/// The time step a Unix timestamp falls in.
#[must_use]
pub fn step_at(unix_seconds: i64) -> i64 {
    unix_seconds.div_euclid(STEP_SECONDS)
}

/// The code for a given step.
///
/// This is HOTP with the counter fixed to the step, which is exactly what TOTP is.
/// Keeping the primitive visible makes the RFC 4226 vectors usable as tests and
/// makes the window logic in [`verify`] obviously separate from the computation.
#[must_use]
pub fn code_for_step(secret: &[u8], step: i64) -> String {
    let mut mac = HmacSha1::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&(step as u64).to_be_bytes());
    let digest = mac.finalize().into_bytes();

    // Dynamic truncation, RFC 4226 §5.4: the low nibble of the last byte picks a
    // four-byte window, and the high bit of that window is masked off so the value
    // is positive on every platform.
    let offset = usize::from(digest[19] & 0x0f);
    let binary = ((u32::from(digest[offset]) & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);

    format!("{:06}", binary % 10_u32.pow(DIGITS))
}

/// The code valid right now.
#[must_use]
pub fn code_now(secret: &[u8], unix_seconds: i64) -> String {
    code_for_step(secret, step_at(unix_seconds))
}

/// Checks a submitted code and returns the step it matched.
///
/// The caller uses the step, not a boolean: a code replayed inside its own window
/// is the same string, and only the step tells the two apart.
#[must_use]
pub fn verify(secret: &[u8], code: &str, unix_seconds: i64, skew_steps: i64) -> Option<i64> {
    let submitted = code.trim();
    if submitted.len() != DIGITS as usize || !submitted.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let current = step_at(unix_seconds);
    // `-skew..=skew`, and deliberately not `return` on the first match: iterating
    // the whole range keeps the comparison count independent of which step matched.
    let mut matched = None;
    for step in (current - skew_steps)..=(current + skew_steps) {
        if code_for_step(secret, step) == submitted {
            matched = Some(step);
        }
    }
    matched
}

/// The `otpauth://` URI an authenticator app reads from a QR code.
///
/// The label is `Issuer:account` and the issuer is repeated as a query parameter
/// because Google Authenticator reads one and most others read the other.
#[must_use]
pub fn otpauth_uri(issuer: &str, account: &str, secret_base32: &str) -> String {
    let label = urlencode(&format!("{issuer}:{account}"));
    let issuer_parameter = urlencode(issuer);
    format!(
        "otpauth://totp/{label}?secret={secret_base32}&issuer={issuer_parameter}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}"
    )
}

/// Percent-encodes the characters that would break the URI. Deliberately small:
/// the inputs are an issuer chosen by the deployment and an email address, and a
/// full URI encoder would be a dependency for four characters.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4226 Appendix D: the secret is "12345678901234567890" and the expected
    /// values are the HOTP codes for counters 0..9. TOTP is the same computation
    /// with the counter derived from time, so these pin the primitive.
    const RFC4226_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn the_rfc4226_vectors_match() {
        let expected = [
            "755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583",
            "399871", "520489",
        ];
        for (counter, want) in expected.iter().enumerate() {
            let got = code_for_step(RFC4226_SECRET, counter as i64);
            assert_eq!(&got, want, "counter {counter}");
        }
    }

    #[test]
    fn a_code_is_valid_in_its_own_window() {
        // 2026-11-27T10:00:00Z is 1764237600; the step is 58807920.
        let now = 1_764_237_600;
        let code = code_now(RFC4226_SECRET, now);
        assert_eq!(
            verify(RFC4226_SECRET, &code, now, DEFAULT_SKEW_STEPS),
            Some(step_at(now))
        );
    }

    #[test]
    fn the_skew_accepts_the_step_before_and_after() {
        let now = 1_764_237_600;
        let previous = code_for_step(RFC4226_SECRET, step_at(now) - 1);
        let next = code_for_step(RFC4226_SECRET, step_at(now) + 1);
        assert_eq!(
            verify(RFC4226_SECRET, &previous, now, DEFAULT_SKEW_STEPS),
            Some(step_at(now) - 1)
        );
        assert_eq!(
            verify(RFC4226_SECRET, &next, now, DEFAULT_SKEW_STEPS),
            Some(step_at(now) + 1)
        );
    }

    #[test]
    fn two_steps_away_is_refused() {
        let now = 1_764_237_600;
        let stale = code_for_step(RFC4226_SECRET, step_at(now) - 2);
        assert_eq!(
            verify(RFC4226_SECRET, &stale, now, DEFAULT_SKEW_STEPS),
            None
        );
    }

    #[test]
    fn a_code_for_another_secret_is_refused() {
        let now = 1_764_237_600;
        let other = code_now(b"a different secret", now);
        assert_eq!(
            verify(RFC4226_SECRET, &other, now, DEFAULT_SKEW_STEPS),
            None
        );
    }

    #[test]
    fn malformed_codes_are_refused_without_comparing() {
        let now = 1_764_237_600;
        for code in ["", "12345", "1234567", "abcdef", "12 34 56", "１２３４５６"] {
            assert_eq!(
                verify(RFC4226_SECRET, code, now, DEFAULT_SKEW_STEPS),
                None,
                "{code:?}"
            );
        }
    }

    #[test]
    fn the_secret_round_trips_through_base32() {
        let secret = generate_secret();
        let encoded = encode_secret(&secret);
        assert_eq!(encoded.len(), 32, "20 bytes encode to 32 base32 characters");
        assert_eq!(decode_secret(&encoded).as_deref(), Some(&secret[..]));
    }

    #[test]
    fn decoding_tolerates_case_and_padding() {
        let secret = generate_secret();
        let encoded = encode_secret(&secret);
        let padded = format!("{}====", encoded.to_ascii_lowercase());
        assert_eq!(decode_secret(&padded).as_deref(), Some(&secret[..]));
    }

    #[test]
    fn decoding_rejects_nonsense() {
        assert!(decode_secret("not base32!").is_none());
        assert!(
            decode_secret("").is_some(),
            "an empty secret decodes to no bytes"
        );
    }

    #[test]
    fn the_otpauth_uri_carries_what_an_authenticator_app_needs() {
        let uri = otpauth_uri("Myota", "admin@example.org", "JBSWY3DPEHPK3PXP");
        assert!(
            uri.starts_with("otpauth://totp/Myota%3Aadmin%40example.org?"),
            "{uri}"
        );
        assert!(uri.contains("secret=JBSWY3DPEHPK3PXP"), "{uri}");
        assert!(uri.contains("issuer=Myota"), "{uri}");
        assert!(uri.contains("period=30"), "{uri}");
        assert!(uri.contains("digits=6"), "{uri}");
    }

    #[test]
    fn the_generated_codes_are_always_six_digits() {
        for step in 0..500 {
            let code = code_for_step(RFC4226_SECRET, step);
            assert_eq!(code.len(), 6, "{code}");
            assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code}");
        }
    }
}

//! Access tokens: Ed25519 over a compact JWS, and the JWKS the gateway reads.
//!
//! ## Why the JWS is written out here
//!
//! It is four steps — encode the header, encode the payload, sign
//! `header.payload`, concatenate — and the verify side is the same in reverse. The
//! alternative, `jsonwebtoken`, wants a PKCS#8 key, so the key would have to be
//! wrapped in DER before it could be used; that wrapping is the only thing between
//! a 32-byte seed and a signature and it is the part a library would not test for
//! us. `ed25519-dalek` does the cryptography and is audited; this module does the
//! encoding and is tested against tokens the gateway's verifier accepts.
//!
//! ## Claims
//!
//! Exactly the set in `docs/architecture/auth-model.md`:
//!
//! ```text
//! iss  myota-core
//! aud  admin (or participant)
//! sub  account uuid
//! sid  session uuid
//! jti  token uuid, recorded in auth_revocation on logout
//! scp  effective scopes at issue time
//! iat  issued at
//! exp  expiry
//! ```
//!
//! The gateway requires `iss`, exactly one `aud`, a `sub` and a `sid`; anything
//! missing is refused there, so it is refused here first.

use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::TOKEN_ISSUER;

/// Why a token was refused. The distinction matters for logging: an expired token
/// is normal, a bad signature is an incident.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    /// Not three base64url segments.
    #[error("token: not a compact JWS")]
    Malformed,
    /// The header names an algorithm this service does not use.
    #[error("token: unsupported algorithm")]
    WrongAlgorithm,
    /// The header names a key id this service does not hold.
    #[error("token: unknown key id")]
    UnknownKey,
    /// The signature does not verify.
    #[error("token: the signature does not verify")]
    BadSignature,
    /// The issuer is not this deployment.
    #[error("token: wrong issuer")]
    WrongIssuer,
    /// The audience is not the one this service issues.
    #[error("token: wrong audience")]
    WrongAudience,
    /// `exp` has passed.
    #[error("token: expired")]
    Expired,
    /// A service token claims a lifetime longer than sixty seconds.
    #[error("token: the lifetime exceeds the service token limit")]
    LifetimeTooLong,
    /// A required claim is missing or the wrong type.
    #[error("token: a required claim is missing")]
    MissingClaim,
}

/// The claims `core` issues and checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessClaims {
    /// Issuer, always [`TOKEN_ISSUER`].
    pub iss: String,
    /// Audience: `admin` or `participant`. One value, never both.
    pub aud: String,
    /// The account.
    pub sub: Uuid,
    /// The session the token belongs to.
    pub sid: Uuid,
    /// The token's own identity, for revocation.
    pub jti: Uuid,
    /// Effective scopes at issue time.
    pub scp: Vec<String>,
    /// Issued at, Unix seconds.
    pub iat: i64,
    /// Expiry, Unix seconds.
    pub exp: i64,
}

/// The two audiences.
pub const AUDIENCE_ADMIN: &str = "admin";
/// The participant audience, issued by P1/P2's endpoints and refused here.
pub const AUDIENCE_PARTICIPANT: &str = "participant";
/// The service audience: a call between services, never a person.
///
/// The gateway accepts exactly two audiences — `admin` and `participant` — so a
/// `service` token is refused there by construction. That is the second half of
/// "internal endpoints are internal": the route table does not publish them, and the
/// credential class would not pass the gateway if it did.
pub const AUDIENCE_SERVICE: &str = "service";

/// How long a service token lives.
///
/// Sixty seconds, per the architecture note: short enough that a leaked token is nearly
/// worthless, long enough for one call. Enforced on both ends — [`TokenService::issue_service`]
/// mints it, and [`TokenService::verify_service`] refuses anything that claims longer,
/// because a token with a longer TTL is either a bug or an attempt.
pub const SERVICE_TOKEN_TTL_SECONDS: i64 = 60;

/// The claims a service token carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceClaims {
    /// Issuer, always [`TOKEN_ISSUER`].
    pub iss: String,
    /// Audience, always [`AUDIENCE_SERVICE`].
    pub aud: String,
    /// The calling service, e.g. `geodata`.
    pub svc: String,
    /// Issued at, Unix seconds.
    pub iat: i64,
    /// Expiry, Unix seconds.
    pub exp: i64,
}

/// Signs and verifies access tokens.
pub struct TokenService {
    signing_key: SigningKey,
    key_id: String,
    access_ttl: Duration,
    now: fn() -> i64,
}

impl TokenService {
    /// Builds the service from a 32-byte Ed25519 seed.
    #[must_use]
    pub fn new(seed: &[u8; 32], key_id: impl Into<String>, access_ttl: Duration) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(seed),
            key_id: key_id.into(),
            access_ttl,
            now: current_unix_seconds,
        }
    }

    /// Replaces the clock. Tests only.
    #[cfg(test)]
    pub fn with_clock(mut self, now: fn() -> i64) -> Self {
        self.now = now;
        self
    }

    /// The public key, base64url — the `x` member of the JWKS entry.
    #[must_use]
    pub fn public_key_base64url(&self) -> String {
        B64.encode(self.signing_key.verifying_key().to_bytes())
    }

    /// The key id, echoed in the token header.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The JWKS document the gateway fetches from `/internal/auth/jwks`.
    ///
    /// One key today. The shape allows more, and the gateway refetches on an
    /// unknown `kid`, which is what makes the 180-day rollover a deploy rather than
    /// an outage.
    #[must_use]
    pub fn jwks(&self) -> serde_json::Value {
        serde_json::json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "kid": self.key_id,
                "use": "sig",
                "alg": "EdDSA",
                "x": self.public_key_base64url(),
            }]
        })
    }

    /// Issues an access token for one session.
    ///
    /// `jti` is passed in rather than generated here so the caller can record the
    /// same value in the session row, which is what makes logout able to revoke the
    /// token that was just issued.
    #[must_use]
    pub fn issue(
        &self,
        account_id: Uuid,
        session_id: Uuid,
        token_id: Uuid,
        audience: &str,
        scopes: &[String],
    ) -> (String, i64) {
        let issued_at = (self.now)();
        let expires_at = issued_at + self.access_ttl.as_secs() as i64;
        let claims = AccessClaims {
            iss: TOKEN_ISSUER.to_string(),
            aud: audience.to_string(),
            sub: account_id,
            sid: session_id,
            jti: token_id,
            scp: scopes.to_vec(),
            iat: issued_at,
            exp: expires_at,
        };

        (self.sign(&claims), expires_at)
    }

    /// Issues a service token for a calling service.
    ///
    /// `svc` is the caller's name, not a target: the token says *who is calling*, and
    /// every internal endpoint decides for itself whether that caller may. A
    /// per-target `aud` would need one audience value per service and would make a
    /// token minted for `core` useless to `geodata`, which is not a property worth the
    /// bookkeeping.
    #[must_use]
    pub fn issue_service(&self, service: &str) -> (String, i64) {
        let issued_at = (self.now)();
        let expires_at = issued_at + SERVICE_TOKEN_TTL_SECONDS;
        let claims = ServiceClaims {
            iss: TOKEN_ISSUER.to_string(),
            aud: AUDIENCE_SERVICE.to_string(),
            svc: service.to_string(),
            iat: issued_at,
            exp: expires_at,
        };
        (self.sign(&claims), expires_at)
    }

    /// Verifies a service token and returns the caller's name.
    ///
    /// Three checks beyond the signature, and the session path needs none of them:
    /// the audience must be `service` (so a stolen administrator token is not a service
    /// credential), the token must not have expired, and the **claimed lifetime** must
    /// fit in sixty seconds. The last one matters because `exp` is signed: a token
    /// minted with a week-long life would verify perfectly and defeat the point.
    pub fn verify_service(&self, token: &str) -> Result<String, TokenError> {
        let claims: ServiceClaims = self.verify_signed(token)?;

        if claims.iss != TOKEN_ISSUER {
            return Err(TokenError::WrongIssuer);
        }
        if claims.aud != AUDIENCE_SERVICE {
            return Err(TokenError::WrongAudience);
        }
        let lifetime = claims.exp - claims.iat;
        if lifetime <= 0 || lifetime > SERVICE_TOKEN_TTL_SECONDS {
            return Err(TokenError::LifetimeTooLong);
        }
        if claims.exp <= (self.now)() {
            return Err(TokenError::Expired);
        }
        if claims.svc.is_empty() {
            return Err(TokenError::MissingClaim);
        }
        Ok(claims.svc)
    }

    /// Signs one claims object as a compact JWS.
    ///
    /// A struct of scalars cannot fail to serialise, and a signing failure would mean
    /// the key is unusable — a process that cannot mint a token has nothing to serve.
    fn sign<T: Serialize>(&self, claims: &T) -> String {
        let header = serde_json::json!({
            "alg": "EdDSA",
            "typ": "JWT",
            "kid": self.key_id,
        });
        let header_segment = B64.encode(serde_json::to_vec(&header).expect("header serialises"));
        let payload_segment = B64.encode(serde_json::to_vec(claims).expect("claims serialise"));
        let signing_input = format!("{header_segment}.{payload_segment}");
        let signature = self.signing_key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", B64.encode(signature.to_bytes()))
    }

    /// Checks the header, the key id and the signature, and returns the typed claims.
    ///
    /// Split out so the two claim shapes share one verification and cannot drift: the
    /// algorithm pinning and the signature check are the security-relevant part, and
    /// they exist once.
    fn verify_signed<T: serde::de::DeserializeOwned>(&self, token: &str) -> Result<T, TokenError> {
        let mut segments = token.split('.');
        let (Some(header_segment), Some(payload_segment), Some(signature_segment), None) = (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ) else {
            return Err(TokenError::Malformed);
        };

        let header_bytes = B64
            .decode(header_segment)
            .map_err(|_| TokenError::Malformed)?;
        let header: serde_json::Value =
            serde_json::from_slice(&header_bytes).map_err(|_| TokenError::Malformed)?;
        match header.get("alg").and_then(|value| value.as_str()) {
            Some("EdDSA") => {}
            Some(_) => return Err(TokenError::WrongAlgorithm),
            None => return Err(TokenError::Malformed),
        }
        match header.get("kid").and_then(|value| value.as_str()) {
            Some(key_id) if key_id == self.key_id => {}
            _ => return Err(TokenError::UnknownKey),
        }

        let signature_bytes = B64
            .decode(signature_segment)
            .map_err(|_| TokenError::Malformed)?;
        let signature =
            Signature::from_slice(&signature_bytes).map_err(|_| TokenError::BadSignature)?;
        let signing_input = format!("{header_segment}.{payload_segment}");
        self.signing_key
            .verifying_key()
            .verify_strict(signing_input.as_bytes(), &signature)
            .map_err(|_| TokenError::BadSignature)?;

        let payload_bytes = B64
            .decode(payload_segment)
            .map_err(|_| TokenError::Malformed)?;
        serde_json::from_slice(&payload_bytes).map_err(|_| TokenError::MissingClaim)
    }

    /// Verifies a token and returns its claims.
    ///
    /// The checks are ordered by cost and by what a failure means: the header
    /// first (cheap, and it decides which key to use), then the signature (the only
    /// check that makes the payload trustworthy), then the claims.
    pub fn verify(&self, token: &str) -> Result<AccessClaims, TokenError> {
        let claims: AccessClaims = self.verify_signed(token)?;

        if claims.iss != TOKEN_ISSUER {
            return Err(TokenError::WrongIssuer);
        }
        if claims.aud != AUDIENCE_ADMIN && claims.aud != AUDIENCE_PARTICIPANT {
            return Err(TokenError::WrongAudience);
        }
        // No leeway in this direction: `core` is the issuer, so its own clock is the one
        // the claim was written with. The gateway allows ten seconds for a token arriving
        // from another pod; here there is nothing to allow for.
        if claims.exp <= (self.now)() {
            return Err(TokenError::Expired);
        }

        Ok(claims)
    }

    /// The verifying key, for tests and for a future rotation overlap.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }
}

/// Unix seconds. Split out so the clock can be replaced in a test.
fn current_unix_seconds() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u8; 32] = [11_u8; 32];

    fn service() -> TokenService {
        TokenService::new(&SEED, "core-test", Duration::from_secs(600))
    }

    #[test]
    fn an_issued_token_verifies_and_carries_its_claims() {
        let service = service();
        let account = Uuid::new_v4();
        let session = Uuid::new_v4();
        let token_id = Uuid::new_v4();
        let scopes = vec!["geodata.review".to_string()];

        let (token, expires_at) =
            service.issue(account, session, token_id, AUDIENCE_ADMIN, &scopes);
        let claims = service.verify(&token).expect("verifies");

        assert_eq!(claims.sub, account);
        assert_eq!(claims.sid, session);
        assert_eq!(claims.jti, token_id);
        assert_eq!(claims.aud, AUDIENCE_ADMIN);
        assert_eq!(claims.scp, scopes);
        assert_eq!(claims.iss, TOKEN_ISSUER);
        assert_eq!(claims.exp, expires_at);
        assert_eq!(claims.exp - claims.iat, 600);
    }

    #[test]
    fn the_header_carries_the_key_id_and_the_algorithm() {
        let service = service();
        let (token, _) = service.issue(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            AUDIENCE_ADMIN,
            &[],
        );
        let header_segment = token.split('.').next().expect("segments");
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(header_segment).expect("base64")).expect("json");
        assert_eq!(header["alg"], "EdDSA");
        assert_eq!(header["kid"], "core-test");
        assert_eq!(header["typ"], "JWT");
    }

    #[test]
    fn the_jwks_carries_the_public_key_the_gateway_needs() {
        let service = service();
        let jwks = service.jwks();
        let key = &jwks["keys"][0];
        assert_eq!(key["kty"], "OKP");
        assert_eq!(key["crv"], "Ed25519");
        assert_eq!(key["kid"], "core-test");

        // The published `x` must be the raw 32-byte public key, base64url.
        let published = B64.decode(key["x"].as_str().expect("x")).expect("base64");
        assert_eq!(published, service.verifying_key().to_bytes());
    }

    #[test]
    fn a_token_signed_with_another_key_is_refused() {
        let other = TokenService::new(&[12_u8; 32], "core-test", Duration::from_secs(600));
        let (token, _) = other.issue(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            AUDIENCE_ADMIN,
            &[],
        );
        assert_eq!(service().verify(&token), Err(TokenError::BadSignature));
    }

    #[test]
    fn a_token_with_another_key_id_is_refused() {
        let other = TokenService::new(&SEED, "core-2", Duration::from_secs(600));
        let (token, _) = other.issue(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            AUDIENCE_ADMIN,
            &[],
        );
        assert_eq!(service().verify(&token), Err(TokenError::UnknownKey));
    }

    #[test]
    fn a_tampered_payload_is_refused() {
        let service = service();
        let (token, _) = service.issue(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            AUDIENCE_ADMIN,
            &[],
        );
        let mut segments: Vec<String> = token.split('.').map(str::to_string).collect();

        let mut claims: AccessClaims =
            serde_json::from_slice(&B64.decode(&segments[1]).expect("base64")).expect("json");
        claims.aud = AUDIENCE_PARTICIPANT.to_string();
        segments[1] = B64.encode(serde_json::to_vec(&claims).expect("json"));

        assert_eq!(
            service.verify(&segments.join(".")),
            Err(TokenError::BadSignature)
        );
    }

    #[test]
    fn an_expired_token_is_refused() {
        static NOW: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1_000_000);
        fn clock() -> i64 {
            NOW.load(std::sync::atomic::Ordering::SeqCst)
        }

        let service = service().with_clock(clock);
        let (token, _) = service.issue(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            AUDIENCE_ADMIN,
            &[],
        );
        NOW.store(1_000_000 + 601, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(service.verify(&token), Err(TokenError::Expired));
    }

    #[test]
    fn the_none_algorithm_is_refused() {
        let service = service();
        let header = B64.encode(br#"{"alg":"none","typ":"JWT","kid":"core-test"}"#);
        let payload = B64.encode(br#"{"iss":"myota-core","aud":"admin"}"#);
        assert_eq!(
            service.verify(&format!("{header}.{payload}.")),
            Err(TokenError::WrongAlgorithm)
        );
    }

    #[test]
    fn an_hs256_token_reinterpreting_the_public_key_is_refused() {
        let service = service();
        let header = B64.encode(br#"{"alg":"HS256","typ":"JWT","kid":"core-test"}"#);
        let payload = B64.encode(br#"{}"#);
        assert_eq!(
            service.verify(&format!("{header}.{payload}.sig")),
            Err(TokenError::WrongAlgorithm)
        );
    }

    #[test]
    fn a_malformed_token_is_refused() {
        let service = service();
        for token in ["", "one", "one.two", "one.two.three.four", "!!.??.**"] {
            assert!(
                matches!(service.verify(token), Err(TokenError::Malformed)),
                "{token:?}"
            );
        }
    }

    #[test]
    fn a_wrong_issuer_is_refused() {
        // Signed with the right key, but by a deployment that is not this one.
        let service = TokenService::new(&SEED, "core-test", Duration::from_secs(600));
        let claims = AccessClaims {
            iss: "somewhere-else".into(),
            aud: AUDIENCE_ADMIN.into(),
            sub: Uuid::new_v4(),
            sid: Uuid::new_v4(),
            jti: Uuid::new_v4(),
            scp: vec![],
            iat: current_unix_seconds(),
            exp: current_unix_seconds() + 600,
        };
        let header_segment = B64
            .encode(serde_json::json!({"alg":"EdDSA","typ":"JWT","kid":"core-test"}).to_string());
        let payload_segment = B64.encode(serde_json::to_vec(&claims).expect("json"));
        let signing_input = format!("{header_segment}.{payload_segment}");
        let signature = service.signing_key.sign(signing_input.as_bytes());
        let token = format!("{signing_input}.{}", B64.encode(signature.to_bytes()));

        assert_eq!(service.verify(&token), Err(TokenError::WrongIssuer));
    }
}

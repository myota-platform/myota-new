//! Configuration for `core`, read once from the environment.
//!
//! Two rules, both learned from the gateway's configuration package:
//!
//!   * **Configuration, never a secret stored in the image.** The database URL and
//!     the two keys arrive in the environment from Kubernetes Secrets. Nothing here
//!     is committed.
//!   * **A bad value stops the process.** A service that starts with a 10-second
//!     access-token lifetime, or with a key of the wrong length, produces behaviour
//!     nobody can explain later. Refusing to start is the honest failure.

use std::time::Duration;

use base64::Engine as _;

/// Default listen address. Matches every other service and the Service object.
pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8080";

/// The lifetime of an administrator access token: ten minutes.
pub const DEFAULT_ACCESS_TTL: Duration = Duration::from_secs(600);
/// Idle lifetime of a refresh session: thirty minutes without use.
pub const DEFAULT_REFRESH_IDLE: Duration = Duration::from_secs(30 * 60);
/// Absolute lifetime of a refresh session: eight hours from sign-in, used or not.
pub const DEFAULT_REFRESH_ABSOLUTE: Duration = Duration::from_secs(8 * 60 * 60);
/// How long a second-factor challenge stays answerable.
pub const DEFAULT_CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
/// How long a pending MFA enrolment can be confirmed before it must be restarted.
pub const DEFAULT_ENROLMENT_TTL: Duration = Duration::from_secs(15 * 60);

/// Failed administrator logins allowed per key inside one window.
pub const DEFAULT_LOGIN_RATE_LIMIT: i64 = 5;
/// The window for [`DEFAULT_LOGIN_RATE_LIMIT`].
pub const DEFAULT_LOGIN_RATE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Wrong passwords that lock an account.
pub const DEFAULT_LOCKOUT_THRESHOLD: i32 = 5;
/// How long an account stays locked.
pub const DEFAULT_LOCKOUT_DURATION: Duration = Duration::from_secs(15 * 60);

/// Wrong second-factor codes allowed per challenge before it is burned.
pub const CHALLENGE_ATTEMPT_LIMIT: i32 = 5;

/// The issuer claim. The gateway refuses any other value, so it is a constant of
/// the deployment rather than a per-environment setting.
pub const TOKEN_ISSUER: &str = "myota-core";

/// Everything `core` needs to run.
#[derive(Clone)]
pub struct Config {
    /// Address to listen on.
    pub listen_addr: String,
    /// `myota_core` connection URL. Points at the pooler, never at PostgreSQL.
    pub database_url: String,
    /// Ed25519 seed used to sign access tokens. The gateway fetches the public half.
    pub token_seed: [u8; 32],
    /// Key id published with the signature, so the gateway can pick the right key
    /// during a rollover.
    pub token_key_id: String,
    /// AES-256-GCM key that encrypts TOTP seeds at rest.
    pub mfa_key: [u8; 32],
    /// The label a TOTP app shows: `otpauth://totp/Myota:admin@example.org`.
    pub otp_issuer: String,
    /// Access-token lifetime.
    pub access_ttl: Duration,
    /// Refresh idle lifetime — extended on every rotation.
    pub refresh_idle: Duration,
    /// Refresh absolute lifetime — never extended.
    pub refresh_absolute: Duration,
    /// Challenge lifetime.
    pub challenge_ttl: Duration,
    /// Pending-enrolment lifetime.
    pub enrolment_ttl: Duration,
    /// Failed logins allowed per key per window.
    pub login_rate_limit: i64,
    /// The rate-limit window.
    pub login_rate_window: Duration,
    /// Wrong passwords before an account locks.
    pub lockout_threshold: i32,
    /// How long a locked account stays locked.
    pub lockout_duration: Duration,
    /// Service credentials, by service name.
    ///
    /// A service that can present one of these gets a sixty-second token from
    /// `/internal/token`. Empty means no service may call an internal endpoint, which is
    /// the correct default: a caller that was never issued a credential cannot obtain
    /// authority by asking for it.
    ///
    /// Held as plaintext because they arrive in an environment variable from a Secret
    /// either way, and the comparison is constant-time. Hashing them here would protect
    /// nothing a `kubectl describe pod` does not already reveal to anyone who can read
    /// the pod.
    pub service_clients: std::collections::BTreeMap<String, String>,
    /// Whether to take the client address from `X-Forwarded-For`. True behind the
    /// gateway, false when the service is addressed directly — otherwise a caller
    /// could choose its own rate-limit bucket by inventing a header.
    pub trust_forwarded_for: bool,
}

impl std::fmt::Debug for Config {
    /// Deliberately omits the database password and both keys: a `Debug` that
    /// prints them is a `tracing::debug!` away from a credential in a log.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field("listen_addr", &self.listen_addr)
            .field("database_url", &"<redacted>")
            .field("token_key_id", &self.token_key_id)
            .field("otp_issuer", &self.otp_issuer)
            .field("access_ttl", &self.access_ttl)
            .field("refresh_idle", &self.refresh_idle)
            .field("refresh_absolute", &self.refresh_absolute)
            .field("trust_forwarded_for", &self.trust_forwarded_for)
            .field(
                "service_clients",
                &self.service_clients.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// A configuration value that cannot be used, named well enough to fix.
#[derive(Debug, thiserror::Error)]
#[error("config: {0}")]
pub struct ConfigError(pub String);

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the configuration from a lookup function, so tests need no environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let database_url = database_url(&lookup)?;

        Ok(Self {
            listen_addr: lookup("CORE_LISTEN_ADDR")
                .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_string()),
            database_url,
            token_seed: key(&lookup, "CORE_JWT_SEED_BASE64", 32)?,
            token_key_id: lookup("CORE_JWT_KEY_ID").unwrap_or_else(|| "core-1".to_string()),
            mfa_key: key(&lookup, "CORE_MFA_KEY_BASE64", 32)?,
            otp_issuer: lookup("CORE_OTP_ISSUER").unwrap_or_else(|| "Myota".to_string()),
            access_ttl: seconds(&lookup, "CORE_ACCESS_TTL_SECONDS", DEFAULT_ACCESS_TTL)?,
            refresh_idle: seconds(&lookup, "CORE_REFRESH_IDLE_SECONDS", DEFAULT_REFRESH_IDLE)?,
            refresh_absolute: seconds(
                &lookup,
                "CORE_REFRESH_ABSOLUTE_SECONDS",
                DEFAULT_REFRESH_ABSOLUTE,
            )?,
            challenge_ttl: seconds(&lookup, "CORE_CHALLENGE_TTL_SECONDS", DEFAULT_CHALLENGE_TTL)?,
            enrolment_ttl: seconds(&lookup, "CORE_ENROLMENT_TTL_SECONDS", DEFAULT_ENROLMENT_TTL)?,
            login_rate_limit: integer(&lookup, "CORE_LOGIN_RATE_LIMIT", DEFAULT_LOGIN_RATE_LIMIT)?,
            login_rate_window: seconds(
                &lookup,
                "CORE_LOGIN_RATE_WINDOW_SECONDS",
                DEFAULT_LOGIN_RATE_WINDOW,
            )?,
            lockout_threshold: integer_i32(
                &lookup,
                "CORE_LOCKOUT_THRESHOLD",
                DEFAULT_LOCKOUT_THRESHOLD,
            )?,
            lockout_duration: seconds(&lookup, "CORE_LOCKOUT_SECONDS", DEFAULT_LOCKOUT_DURATION)?,
            service_clients: service_clients(&lookup)?,
            trust_forwarded_for: boolean(&lookup, "CORE_TRUST_FORWARDED_FOR", true)?,
        })
    }
}

/// The connection URL, from `CORE_DATABASE_URL` or from its parts.
///
/// Two forms because they serve different readers. A URL is what an operator has in
/// a shell; discrete variables are what a Deployment can assemble from a Secret,
/// since a container environment cannot concatenate a password into a string.
/// AGENTS.md rule 8 prefers the discrete form for exactly this reason — the password
/// stays in the Secret, and the host, port and database name stay configuration.
fn database_url(lookup: &impl Fn(&str) -> Option<String>) -> Result<String, ConfigError> {
    if let Some(url) = lookup("CORE_DATABASE_URL").filter(|value| !value.trim().is_empty()) {
        if !url.starts_with("postgres://") && !url.starts_with("postgresql://") {
            return Err(ConfigError(
                "CORE_DATABASE_URL must be a postgres:// or postgresql:// URL".into(),
            ));
        }
        return Ok(url);
    }

    let host = required(lookup, "CORE_DB_HOST")?;
    let name = required(lookup, "CORE_DB_NAME")?;
    let user = required(lookup, "CORE_DB_USER")?;
    // The password is required here even though a trust-authenticated local server
    // does not use one: in the cluster it always exists, and a missing Secret key
    // should stop the pod rather than produce a URL that fails at the first query.
    let password = required(lookup, "CORE_DB_PASSWORD")?;
    let port = lookup("CORE_DB_PORT").unwrap_or_else(|| "5432".to_string());
    let sslmode = lookup("CORE_DB_SSLMODE").unwrap_or_else(|| "disable".to_string());

    Ok(format!(
        "postgres://{}:{}@{}:{}/{}?sslmode={}",
        urlencode(&user),
        urlencode(&password),
        host,
        port,
        name,
        sslmode
    ))
}

/// Percent-encodes the characters that would end the user-info section of a URL.
///
/// A password is arbitrary bytes-as-text and will eventually contain one of these;
/// a DSN that breaks on an `@` in a password is a deploy that fails for a reason
/// nobody can see in the values file.
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

fn required(lookup: &impl Fn(&str) -> Option<String>, name: &str) -> Result<String, ConfigError> {
    match lookup(name) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ConfigError(format!("{name} is not set"))),
    }
}

/// Reads `CORE_SERVICE_CLIENTS`: a JSON object of service name to secret.
///
/// A JSON object rather than one variable per service because the set is deployment
/// data, not a schema: adding a service should not need a code change, and a malformed
/// value should stop the process rather than silently authorising nobody.
fn service_clients(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<std::collections::BTreeMap<String, String>, ConfigError> {
    let Some(raw) = lookup("CORE_SERVICE_CLIENTS").filter(|value| !value.trim().is_empty()) else {
        return Ok(std::collections::BTreeMap::new());
    };

    let parsed: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&raw).map_err(|error| {
            ConfigError(format!(
                "CORE_SERVICE_CLIENTS is not a JSON object: {error}"
            ))
        })?;

    for (service, secret) in &parsed {
        if service.is_empty()
            || !service
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(ConfigError(format!(
                "CORE_SERVICE_CLIENTS names {service:?}, which is not a lowercase service name"
            )));
        }
        // 32 characters is the shortest a secret of this kind should be: it is compared
        // for equality, and a short one is guessable by a caller who can reach the
        // endpoint at all.
        if secret.len() < 32 {
            return Err(ConfigError(format!(
                "CORE_SERVICE_CLIENTS gives {service} a secret shorter than 32 characters"
            )));
        }
    }

    Ok(parsed)
}

fn boolean(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: bool,
) -> Result<bool, ConfigError> {
    match lookup(name) {
        None => Ok(default),
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(ConfigError(format!("{name} = {other:?} is not a boolean"))),
        },
    }
}

fn integer(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: i64,
) -> Result<i64, ConfigError> {
    match lookup(name) {
        None => Ok(default),
        Some(value) => {
            let parsed: i64 = value
                .trim()
                .parse()
                .map_err(|_| ConfigError(format!("{name} = {value:?} is not an integer")))?;
            if parsed <= 0 {
                return Err(ConfigError(format!(
                    "{name} must be positive, got {parsed}"
                )));
            }
            Ok(parsed)
        }
    }
}

fn integer_i32(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: i32,
) -> Result<i32, ConfigError> {
    let value = integer(lookup, name, i64::from(default))?;
    i32::try_from(value).map_err(|_| ConfigError(format!("{name} = {value} is out of range")))
}

fn seconds(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: Duration,
) -> Result<Duration, ConfigError> {
    let value = integer(lookup, name, default.as_secs() as i64)?;
    Ok(Duration::from_secs(value as u64))
}

/// Reads a base64 key of an exact length.
///
/// Both key encodings are accepted — standard and URL-safe, with or without
/// padding — because an operator generating one with `head -c 32 | base64` and an
/// operator generating one with `openssl rand -base64 32 | tr '+/' '-_'` should not
/// get different answers. The length check is not a formality: a 16-byte AES key
/// silently changes the cipher to AES-128 in some APIs, and this one insists.
fn key(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    length: usize,
) -> Result<[u8; 32], ConfigError> {
    let encoded = required(lookup, name)?;
    let trimmed = encoded.trim();

    let decoded = [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ]
    .iter()
    .find_map(|engine| engine.decode(trimmed).ok())
    .ok_or_else(|| ConfigError(format!("{name} is not valid base64")))?;

    if decoded.len() != length {
        return Err(ConfigError(format!(
            "{name} must decode to {length} bytes, got {}",
            decoded.len()
        )));
    }
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&decoded);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn base() -> HashMap<String, String> {
        HashMap::from([
            (
                "CORE_DATABASE_URL".to_string(),
                "postgres://u@h/db".to_string(),
            ),
            ("CORE_JWT_SEED_BASE64".to_string(), base64_key(&[7_u8; 32])),
            ("CORE_MFA_KEY_BASE64".to_string(), base64_key(&[9_u8; 32])),
        ])
    }

    fn base64_key(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn lookup(map: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn defaults_match_the_documented_admin_lifetimes() {
        let config = Config::from_lookup(lookup(base())).expect("valid");
        assert_eq!(config.access_ttl, Duration::from_secs(600));
        assert_eq!(config.refresh_idle, Duration::from_secs(1800));
        assert_eq!(config.refresh_absolute, Duration::from_secs(8 * 60 * 60));
        assert_eq!(config.lockout_threshold, 5);
        assert_eq!(config.lockout_duration, Duration::from_secs(900));
        assert_eq!(config.login_rate_limit, 5);
    }

    #[test]
    fn a_missing_database_configuration_is_refused() {
        let mut map = base();
        map.remove("CORE_DATABASE_URL");
        let error = Config::from_lookup(lookup(map)).expect_err("refused");
        assert!(error.to_string().contains("CORE_DB_HOST"), "{error}");
    }

    #[test]
    fn discrete_database_variables_build_the_url_with_the_password_encoded() {
        let mut map = base();
        map.remove("CORE_DATABASE_URL");
        map.insert("CORE_DB_HOST".into(), "myota-core-pgbouncer".into());
        map.insert("CORE_DB_PORT".into(), "6432".into());
        map.insert("CORE_DB_NAME".into(), "myota_core".into());
        map.insert("CORE_DB_USER".into(), "myota".into());
        map.insert("CORE_DB_PASSWORD".into(), "p@ss:w/rd".into());

        let config = Config::from_lookup(lookup(map)).expect("valid");
        assert_eq!(
            config.database_url,
            "postgres://myota:p%40ss%3Aw%2Frd@myota-core-pgbouncer:6432/myota_core?sslmode=disable"
        );
    }

    #[test]
    fn a_password_is_required_beside_the_other_parts() {
        let mut map = base();
        map.remove("CORE_DATABASE_URL");
        map.insert("CORE_DB_HOST".into(), "h".into());
        map.insert("CORE_DB_NAME".into(), "n".into());
        map.insert("CORE_DB_USER".into(), "u".into());
        let error = Config::from_lookup(lookup(map)).expect_err("refused");
        assert!(error.to_string().contains("CORE_DB_PASSWORD"), "{error}");
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused_rather_than_truncated() {
        let mut map = base();
        map.insert("CORE_MFA_KEY_BASE64".into(), base64_key(&[1_u8; 16]));
        let error = Config::from_lookup(lookup(map)).expect_err("refused");
        assert!(error.to_string().contains("32 bytes"), "{error}");
    }

    #[test]
    fn a_non_postgres_url_is_refused() {
        let mut map = base();
        map.insert("CORE_DATABASE_URL".into(), "mysql://u@h/db".into());
        let error = Config::from_lookup(lookup(map)).expect_err("refused");
        assert!(error.to_string().contains("postgres://"), "{error}");
    }

    #[test]
    fn overrides_are_read() {
        let mut map = base();
        map.insert("CORE_ACCESS_TTL_SECONDS".into(), "120".into());
        map.insert("CORE_TRUST_FORWARDED_FOR".into(), "false".into());
        let config = Config::from_lookup(lookup(map)).expect("valid");
        assert_eq!(config.access_ttl, Duration::from_secs(120));
        assert!(!config.trust_forwarded_for);
    }

    #[test]
    fn a_zero_lifetime_is_refused() {
        let mut map = base();
        map.insert("CORE_ACCESS_TTL_SECONDS".into(), "0".into());
        let error = Config::from_lookup(lookup(map)).expect_err("refused");
        assert!(error.to_string().contains("positive"), "{error}");
    }

    #[test]
    fn service_clients_parse_and_are_redacted_in_debug() {
        let mut map = base();
        map.insert(
            "CORE_SERVICE_CLIENTS".into(),
            r#"{"geodata":"a-secret-at-least-thirty-two-chars-long"}"#.into(),
        );
        let config = Config::from_lookup(lookup(map)).expect("valid");
        assert_eq!(
            config.service_clients.get("geodata").map(String::as_str),
            Some("a-secret-at-least-thirty-two-chars-long")
        );
        let rendered = format!("{config:?}");
        assert!(rendered.contains("geodata"), "{rendered}");
        assert!(!rendered.contains("a-secret-at-least"), "{rendered}");
    }

    #[test]
    fn a_short_service_secret_is_refused() {
        let mut map = base();
        map.insert(
            "CORE_SERVICE_CLIENTS".into(),
            r#"{"geodata":"short"}"#.into(),
        );
        let error = Config::from_lookup(lookup(map)).expect_err("refused");
        assert!(error.to_string().contains("32 characters"), "{error}");
    }

    #[test]
    fn malformed_service_clients_are_refused_rather_than_ignored() {
        let mut map = base();
        map.insert("CORE_SERVICE_CLIENTS".into(), "geodata=secret".into());
        let error = Config::from_lookup(lookup(map)).expect_err("refused");
        assert!(error.to_string().contains("JSON object"), "{error}");
    }

    #[test]
    fn no_service_clients_configured_is_a_valid_empty_set() {
        let config = Config::from_lookup(lookup(base())).expect("valid");
        assert!(config.service_clients.is_empty());
    }

    #[test]
    fn the_debug_output_never_contains_the_database_url_or_keys() {
        let config = Config::from_lookup(lookup(base())).expect("valid");
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("postgres://"), "{rendered}");
        assert!(!rendered.contains(&base64_key(&[9_u8; 32])), "{rendered}");
    }
}

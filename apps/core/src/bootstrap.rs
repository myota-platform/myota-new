//! Provisioning the first administrator.
//!
//! There is no self-service admin registration, and there cannot be: the sign-in flow
//! refuses to issue a session without a confirmed second factor, and enrolment
//! requires an existing session. The first administrator is therefore provisioned
//! out of band, which is what "a brand-new administrator is provisioned by another
//! administrator" means when there is no other administrator yet.
//!
//! This is the same act a later console will perform — create the account, set the
//! credential, grant a role, enrol a factor — with the factor supplied here because
//! the operator running it is standing in for the second administrator. The tool
//! prints the `otpauth://` URI and the recovery codes once; they are not recoverable
//! from the database afterwards, which is the point of storing the seed encrypted and
//! the codes hashed.
//!
//! ## What A2 replaces
//!
//! `role_definition` is written here with a bootstrap role because the catalogue does
//! not exist yet. A2 owns it: the role codes, the scope vocabulary and the rule that
//! a role is granted by someone who holds it. This tool creates the minimum an
//! administrator needs to sign in and be recognised as one.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::audit::{self, event};
use crate::config::Config;
use crate::{password, secretbox, totp};

/// The role a provisioned administrator gets until A2 defines the catalogue.
pub const BOOTSTRAP_ROLE: &str = "platform_admin";

/// What a successful provisioning produced.
pub struct Provisioned {
    /// The account.
    pub account_id: Uuid,
    /// The `otpauth://` URI, for the QR code.
    pub otpauth_uri: String,
    /// The base32 secret, for a manual entry.
    pub secret: String,
    /// Ten single-use codes.
    pub recovery_codes: Vec<String>,
    /// False when the account already had a confirmed factor and none was replaced.
    pub enrolled_now: bool,
}

/// Creates or updates an administrator account and enrols a TOTP factor.
///
/// Idempotent in the sense that matters for a provisioning tool: running it twice
/// does not create two accounts and does not leave two factors. It does replace the
/// password, because an operator running it is usually resetting one.
pub async fn provision_admin(
    pool: &PgPool,
    config: &Config,
    email: &str,
    display_name: &str,
    plain_password: &str,
) -> Result<Provisioned, Box<dyn std::error::Error>> {
    let email = email.trim().to_ascii_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err("an email address is required".into());
    }
    if plain_password.chars().count() < 12 {
        return Err("the password must be at least 12 characters".into());
    }

    // Hashing is CPU-bound; the provisioning tool is single-use, so the cost is paid
    // once and does not need a blocking pool here.
    let password_hash = password::hash(plain_password)?;

    let mut transaction = pool.begin().await?;

    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM account WHERE email = $1::citext")
            .bind(&email)
            .fetch_optional(&mut *transaction)
            .await?;

    let account_id = match existing {
        Some(account_id) => {
            sqlx::query(
                "UPDATE account
                    SET status = 'active', email_verified = true, closed_at = NULL, updated_at = now()
                  WHERE id = $1",
            )
            .bind(account_id)
            .execute(&mut *transaction)
            .await?;
            account_id
        }
        None => {
            let account_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO account
                     (id, display_name, email, participation_type, status, email_verified)
                 VALUES ($1, $2, $3, 'OPERATOR', 'active', true)",
            )
            .bind(account_id)
            .bind(display_name)
            .bind(&email)
            .execute(&mut *transaction)
            .await?;
            account_id
        }
    };

    sqlx::query(
        "INSERT INTO account_credential
             (account_id, password_hash, password_changed_at, failed_attempts, locked_until)
         VALUES ($1, $2, now(), 0, NULL)
         ON CONFLICT (account_id) DO UPDATE
             SET password_hash = EXCLUDED.password_hash,
                 password_changed_at = now(),
                 failed_attempts = 0,
                 locked_until = NULL,
                 updated_at = now()",
    )
    .bind(account_id)
    .bind(&password_hash)
    .execute(&mut *transaction)
    .await?;

    // The bootstrap role. `is_system` marks it as one the console must not delete.
    sqlx::query(
        "INSERT INTO role_definition (role_code, display_name, scopes, is_system)
         VALUES ($1, 'Platform administrator', $2, true)
         ON CONFLICT (role_code) DO UPDATE
             SET scopes = EXCLUDED.scopes, updated_at = now()",
    )
    .bind(BOOTSTRAP_ROLE)
    .bind(vec![
        "identity.admins.manage".to_string(),
        "identity.participants.manage".to_string(),
        "geodata.review".to_string(),
        "geodata.import".to_string(),
        "programmes.manage".to_string(),
        "awards.manage".to_string(),
    ])
    .execute(&mut *transaction)
    .await?;

    let already_granted: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM account_role
              WHERE account_id = $1 AND role_code = $2 AND valid_to IS NULL)",
    )
    .bind(account_id)
    .bind(BOOTSTRAP_ROLE)
    .fetch_one(&mut *transaction)
    .await?;

    if !already_granted {
        sqlx::query(
            "INSERT INTO account_role (id, account_id, role_code, scopes)
             VALUES ($1, $2, $3, '{}'::jsonb)",
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(BOOTSTRAP_ROLE)
        .execute(&mut *transaction)
        .await?;
    }

    let confirmed: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT confirmed_at FROM account_mfa WHERE account_id = $1 AND method = 'totp'",
    )
    .bind(account_id)
    .fetch_optional(&mut *transaction)
    .await?
    .flatten();

    let mut provisioned = Provisioned {
        account_id,
        otpauth_uri: String::new(),
        secret: String::new(),
        recovery_codes: Vec::new(),
        enrolled_now: false,
    };

    if confirmed.is_none() {
        let secret = totp::generate_secret();
        let secret_base32 = totp::encode_secret(&secret);
        let sealed = secretbox::seal(&config.mfa_key, secret_base32.as_bytes());

        sqlx::query(
            "INSERT INTO account_mfa (account_id, method, secret_ref, confirmed_at)
             VALUES ($1, 'totp', $2, now())
             ON CONFLICT (account_id, method) DO UPDATE
                 SET secret_ref = EXCLUDED.secret_ref,
                     confirmed_at = now(),
                     last_used_at = NULL,
                     updated_at = now()",
        )
        .bind(account_id)
        .bind(&sealed)
        .execute(&mut *transaction)
        .await?;

        sqlx::query("DELETE FROM account_recovery_code WHERE account_id = $1")
            .bind(account_id)
            .execute(&mut *transaction)
            .await?;

        let mut recovery_codes = Vec::with_capacity(10);
        for _ in 0..10 {
            let code = recovery_code();
            sqlx::query(
                "INSERT INTO account_recovery_code (id, account_id, code_hash) VALUES ($1, $2, $3)",
            )
            .bind(Uuid::new_v4())
            .bind(account_id)
            .bind(hash_recovery_code(&code))
            .execute(&mut *transaction)
            .await?;
            recovery_codes.push(code);
        }

        provisioned.otpauth_uri = totp::otpauth_uri(&config.otp_issuer, &email, &secret_base32);
        provisioned.secret = secret_base32;
        provisioned.recovery_codes = recovery_codes;
        provisioned.enrolled_now = true;
    }

    transaction.commit().await?;

    audit::record(
        pool,
        Some(account_id),
        event::MFA_ENROLLED,
        serde_json::json!({
            "method": "totp",
            "by": "provision-admin",
            "enrolledNow": provisioned.enrolled_now,
        }),
    )
    .await;

    Ok(provisioned)
}

/// A recovery code in the same shape the API issues.
fn recovery_code() -> String {
    use rand::RngCore;
    let mut bytes = [0_u8; 6];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex = crate::hex_lower(&bytes);
    format!("{}-{}-{}", &hex[0..4], &hex[4..8], &hex[8..12])
}

/// The stored form of a recovery code: lowercase hex of its SHA-256.
fn hash_recovery_code(code: &str) -> String {
    use sha2::{Digest, Sha256};
    let normalised = code.trim().to_ascii_lowercase();
    crate::hex_lower(&Sha256::digest(normalised.as_bytes()))
}

/// Formats the provisioning output for an operator.
///
/// Printed to stdout and nowhere else: this is the only time the secret and the
/// codes exist in the clear, and a log aggregator that captures them defeats the
/// point of encrypting the seed.
#[must_use]
pub fn render(provisioned: &Provisioned, email: &str) -> String {
    if !provisioned.enrolled_now {
        return format!(
            "account {} already has a confirmed TOTP factor; nothing was enrolled.\n\
             The password was set. Existing recovery codes are unchanged.\n",
            provisioned.account_id
        );
    }

    let mut output = String::new();
    output.push_str(&format!("account:   {}\n", provisioned.account_id));
    output.push_str(&format!("email:     {email}\n"));
    output.push_str(&format!("secret:    {}\n", provisioned.secret));
    output.push_str(&format!("otpauth:   {}\n", provisioned.otpauth_uri));
    output.push_str("\nrecovery codes (shown once):\n");
    for code in &provisioned.recovery_codes {
        output.push_str(&format!("  {code}\n"));
    }
    output.push_str(
        "\nAdd the secret to an authenticator app now. It cannot be recovered from the\n\
         database: the seed is stored encrypted and the codes are stored hashed.\n",
    );
    output
}

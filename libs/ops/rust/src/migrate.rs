//! The migration runner: advisory-locked, idempotent, and refusing to run against a
//! schema it does not understand.

use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, PgPool};

use crate::{OpsError, Result};

/// The `ops` DDL. A copy of `libs/ops/sql/`, made by `tools/gen_ops_sql.py`, because
/// `include_str!` cannot reach outside the module. `--check` fails when a copy is stale,
/// so the two languages cannot apply different schemas.
const MIGRATIONS: &[(i32, &str, &str)] =
    &[(1, "ops_schema", include_str!("../sql/001_ops_schema.sql"))];

/// Arbitrary but permanent: two binaries that disagree hold different locks and migrate
/// concurrently, which is the failure the lock exists to prevent. The Go implementation
/// uses the same value.
const ADVISORY_LOCK_KEY: i64 = 0x6f70_735f_6d69_6772; // "ops_migr"

/// One numbered DDL step.
#[derive(Debug, Clone)]
pub struct Migration {
    pub version: i32,
    pub name: String,
    pub checksum: String,
}

fn checksum(sql: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sql.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The versions and names this binary carries, for a startup log line and for tests.
#[must_use]
pub fn migrations() -> Vec<Migration> {
    MIGRATIONS
        .iter()
        .map(|(version, name, sql)| Migration {
            version: *version,
            name: (*name).to_string(),
            checksum: checksum(sql),
        })
        .collect()
}

/// Creates the ledger. Done by the runner rather than by a migration, because the runner
/// needs it before it can record anything.
const LEDGER_DDL: &str = "CREATE SCHEMA IF NOT EXISTS ops;
CREATE TABLE IF NOT EXISTS ops.schema_migration (
    version    integer     PRIMARY KEY,
    name       text        NOT NULL,
    checksum   text        NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now()
);";

/// Applies every pending migration and returns how many ran.
///
/// Three properties, each a way this goes wrong in production:
///
/// * **One migrator at a time.** A session advisory lock is taken before anything is
///   read, so N replicas starting together apply the DDL once.
/// * **Ahead is an error.** A database with a migration this binary does not know means
///   an older binary has been pointed at a newer database.
/// * **An edited migration is an error.** A version applied with one checksum and
///   presented with another means the DDL changed after it ran.
///
/// Re-running when everything is applied applies nothing and returns 0.
pub async fn migrate(pool: &PgPool) -> Result<usize> {
    let known = migrations();
    let highest_known = known.last().map(|m| m.version).unwrap_or(0);

    let mut conn = pool.acquire().await?;
    let conn: &mut PgConnection = &mut conn;

    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut *conn)
        .await?;

    let result = migrate_locked(conn, &known, highest_known).await;

    // Released explicitly so the pooled connection goes back clean. Ignored because the
    // connection close would drop it anyway.
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut *conn)
        .await;

    result
}

async fn migrate_locked(
    conn: &mut PgConnection,
    known: &[Migration],
    highest_known: i32,
) -> Result<usize> {
    // `raw_sql` sends the script through the simple query protocol, which accepts several
    // statements. Splitting on ';' by hand is what you reach for first and it is wrong:
    // this schema's own header comment contains a semicolon, and a naive split cut the
    // file mid-sentence into a chunk starting with the word "SQL".
    sqlx::raw_sql(LEDGER_DDL).execute(&mut *conn).await?;

    let applied: Vec<(i32, String)> =
        sqlx::query_as("SELECT version, checksum FROM ops.schema_migration")
            .fetch_all(&mut *conn)
            .await?;

    if let Some((highest_applied, _)) = applied.iter().max_by_key(|(version, _)| *version) {
        if *highest_applied > highest_known {
            return Err(OpsError::SchemaAhead(format!(
                "database has migration {highest_applied}, this binary knows up to {highest_known}"
            )));
        }
    }

    let mut count = 0;
    for (index, migration) in known.iter().enumerate() {
        if let Some((_, recorded)) = applied
            .iter()
            .find(|(version, _)| *version == migration.version)
        {
            if recorded != &migration.checksum {
                return Err(OpsError::SchemaAhead(format!(
                    "migration {} ({}) was applied with checksum {} and now has {}; \
                     an applied migration must never be edited — add a new one",
                    migration.version, migration.name, recorded, migration.checksum
                )));
            }
            continue;
        }

        let (_, _, sql) = MIGRATIONS[index];
        let mut transaction = conn.begin().await?;
        // Each migration in its own transaction, so a failure leaves the ledger agreeing
        // with the schema rather than half-applied. `raw_sql` rather than a hand-rolled
        // split — see the note above LEDGER_DDL's call.
        sqlx::raw_sql(sql).execute(&mut *transaction).await?;
        sqlx::query(
            "INSERT INTO ops.schema_migration (version, name, checksum) VALUES ($1, $2, $3)",
        )
        .bind(migration.version)
        .bind(&migration.name)
        .bind(&migration.checksum)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        count += 1;
    }
    Ok(count)
}

/// Checks that the database has at least the migrations this binary knows about, without
/// applying anything.
///
/// This is what a replica calls at startup instead of [`migrate`]: a replica never
/// migrates, it verifies and refuses to start if behind (AGENTS.md rule 12).
pub async fn verify_schema(pool: &PgPool) -> Result<()> {
    let required = migrations().last().map(|m| m.version).unwrap_or(0);

    let (present,): (bool,) =
        sqlx::query_as("SELECT to_regclass('ops.schema_migration') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if !present {
        return Err(OpsError::SchemaBehind(
            "no migration ledger; the migrator has not run".into(),
        ));
    }

    let (highest,): (Option<i32>,) =
        sqlx::query_as("SELECT max(version) FROM ops.schema_migration")
            .fetch_one(pool)
            .await?;
    let highest = highest.unwrap_or(0);
    if highest < required {
        return Err(OpsError::SchemaBehind(format!(
            "database is at {highest}, this binary requires {required}"
        )));
    }
    Ok(())
}

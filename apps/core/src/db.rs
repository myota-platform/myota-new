//! The database pool.
//!
//! One pool, built once, pointed at the **pooler** rather than at PostgreSQL. The
//! pooler is the only path to the database (AGENTS.md rule 17 and the chart's
//! NetworkPolicy), and it runs pgBouncer in transaction mode.
//!
//! ## Prepared statements, and the mistake that cost a deploy
//!
//! In transaction mode a server connection is handed to a different client between
//! transactions. sqlx prepares every statement under a name (`sqlx_s_1`, `sqlx_s_2`, …),
//! so two replicas meeting on one server connection collide:
//!
//! ```text
//! error returned from database: prepared statement "sqlx_s_1" already exists
//! ```
//!
//! The first version of this file set `statement_cache_capacity(0)` and a comment
//! claimed that made sqlx use unnamed statements. It does not: it only stops the
//! client *caching*, and the client still sends a named `Parse` on every query. The
//! pool went from "stale statement" to "duplicate statement" and one of the two core
//! replicas never became ready.
//!
//! The fix is on the pooler, where the state actually lives: pgBouncer 1.21+ tracks
//! prepared statements per client connection (`MAX_PREPARED_STATEMENTS`, set in the
//! chart). `tools/check_deploy.py` fails the build if that value is missing, so the
//! chart cannot regress into the collision. With the pooler tracking them, the client
//! cache is safe and worth keeping — the default hundred distinct statements.

use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

/// How long a replica waits for a connection before reporting unhealthy.
///
/// Short, because the pooler either has capacity or does not: a request waiting
/// thirty seconds for a connection has already failed the caller's patience, and a
/// fast failure lets the replica shed load instead of queueing it.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Connections held per replica.
///
/// The connection budget in AGENTS.md rule 17 is `POOL_CLIENT_CONN=500` across all
/// replicas and poolers; four replicas at ten connections is a fraction of it, and
/// the pooler is what makes the number safe to be generous with.
const MAX_CONNECTIONS: u32 = 10;

/// Builds the pool.
///
/// # Errors
///
/// Returns the connection error when the database cannot be reached at startup. A
/// replica that cannot reach its database must not start: it would pass its
/// liveness probe and fail every request.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let options: PgConnectOptions = database_url
        .parse::<PgConnectOptions>()?
        // The application name shows up in `pg_stat_activity`, which is how an
        // operator tells a pooler session from a migration's.
        .application_name("myota-core");

    PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .connect_with(options)
        .await
}

/// Checks that the database answers, for `/readyz`.
///
/// A single `SELECT 1` rather than a version check: the migration Job owns the
/// schema, and a replica that refuses to serve because the schema moved under it
/// is the behaviour `ops::migrate::verify_schema` provides where it belongs.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
        .map(|_| ())
}

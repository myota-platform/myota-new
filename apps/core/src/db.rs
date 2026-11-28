//! The database pool.
//!
//! One pool, built once, pointed at the **pooler** rather than at PostgreSQL. The
//! pooler is the only path to the database (AGENTS.md rule 17 and the chart's
//! NetworkPolicy), and it runs pgBouncer in transaction mode, which has one
//! consequence that has to be handled here rather than remembered by every query:
//!
//! > In transaction mode a server connection is returned to the pool between
//! > statements, so a prepared statement created by one transaction is not
//! > guaranteed to exist in the next. sqlx prepares by default and caches the
//! > prepared statements per connection.
//!
//! Setting the statement cache to zero makes sqlx use unnamed statements, which is
//! the documented way to talk to pgBouncer in this mode. Without it the first query
//! after a connection is reused fails with
//! `prepared statement "s1" does not exist` — an error that looks like a bug in the
//! query and is not one.

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
        // See the module comment. Zero cached statements is the pgBouncer
        // transaction-mode contract.
        .statement_cache_capacity(0)
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

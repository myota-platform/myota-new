//! The handle a service holds: a pool, a lease owner, and a lease duration.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Options for [`Store`]. The defaults are what a service wants; the setters exist for
/// tests and for naming the replica in a log line.
#[derive(Debug, Clone)]
pub struct StoreOptions {
    /// Identifies this replica in leases.
    pub owner: String,
    /// How long a job claim or partition lease is valid.
    pub lease_duration: Duration,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            // A random owner per process: two replicas must never share one, or one
            // could extend the other's leases.
            owner: format!("ops-{}", &Uuid::new_v4().to_string()[..8]),
            lease_duration: Duration::from_secs(300),
        }
    }
}

impl StoreOptions {
    /// Names the replica. Use the pod name, so a stuck lease names who holds it.
    #[must_use]
    pub fn owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = owner.into();
        self
    }

    /// Sets the lease duration.
    #[must_use]
    pub fn lease_duration(mut self, duration: Duration) -> Self {
        self.lease_duration = duration;
        self
    }
}

/// The handle a service holds. Cheap to clone — a pool plus two values — and safe to
/// share across tasks, which is what lets a service run as N replicas (ADR-0005).
#[derive(Debug, Clone)]
pub struct Store {
    pub(crate) pool: PgPool,
    pub(crate) owner: String,
    pub(crate) lease_duration: Duration,
}

impl Store {
    /// Wraps a pool. The pool is supplied by the service, not opened here: connection
    /// lifecycle, PgBouncer settings and pool size are deployment decisions.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self::with_options(pool, StoreOptions::default())
    }

    /// Wraps a pool with explicit options.
    #[must_use]
    pub fn with_options(pool: PgPool, options: StoreOptions) -> Self {
        Self {
            pool,
            owner: options.owner,
            lease_duration: options.lease_duration,
        }
    }

    /// The lease owner this store identifies itself by.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The underlying pool, for the rare caller that needs its own transaction.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The instant a lease taken now would expire.
    pub(crate) fn lease_deadline(&self) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::from_std(self.lease_duration).unwrap_or_default()
    }
}

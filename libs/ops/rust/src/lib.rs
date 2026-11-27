//! The operational pattern shared by every service: transactional outbox,
//! idempotency records, job claiming and dead letters.
//!
//! There is no message broker (ADR-0004). Each service writes events to its own
//! `ops.outbox_event` inside the domain transaction, and peers pull them. Ordering
//! is per subject, which is what partition assignment is for.
//!
//! This crate exists twice — `libs/ops/go` and `libs/ops/rust` — because a Rust
//! service cannot link a Go module (ADR-0002). The two must agree on behaviour,
//! and `libs/ops/conformance/` is the suite that holds them to it. The DDL is
//! **not** duplicated: `libs/ops/sql/` is written once and copied into both
//! modules, so the two cannot be applying different schemas.
//!
//! The API mirrors the Go implementation method for method, so a reader moving
//! between services finds the same names and the same semantics.

use std::num::NonZeroU16;

pub mod feed;
pub mod idempotency;
pub mod job;
pub mod migrate;
pub mod outbox;
pub mod relay;
pub mod store;

pub use feed::Feed;
pub use idempotency::{hash_request, HashRequest, IdempotencyOutcome, StoredResponse};
pub use job::{Backoff, Job};
pub use migrate::{migrate, verify_schema, Migration};
pub use outbox::{enqueue, Enqueued, Event};
pub use store::{Store, StoreOptions};

const FNV_OFFSET_BASIS_64: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME_64: u64 = 0x0000_0100_0000_01b3;

/// How many partitions the event log is divided into.
///
/// A constant rather than a setting: changing it rewrites the assignment of every
/// existing subject, and a consumer mid-stream would see each aggregate jump
/// partitions. Raise it in a migration, with the resync that implies.
pub const PARTITIONS: u16 = 32;

/// Errors a caller has to distinguish from a generic failure.
#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    /// A row is not in a state that permits the operation — completing something
    /// dead, for instance. A caller bug, not a retryable failure.
    #[error("ops: the row is not in a state that permits this operation: {0}")]
    NoRun(String),

    /// The database has not had the migrations this binary needs. A replica must
    /// refuse to start rather than serve queries against a schema it does not
    /// understand.
    #[error("ops: the database schema is behind this binary: {0}")]
    SchemaBehind(String),

    /// The database has migrations this binary does not know about — an older
    /// binary pointed at a newer database, which during a rollback is exactly the
    /// situation that corrupts data if it proceeds.
    #[error("ops: the database schema is ahead of this binary: {0}")]
    SchemaAhead(String),

    /// A key was reused with a different request body.
    #[error("ops: idempotency key reused with a different body: {0}")]
    IdempotencyConflict(String),

    /// The same key is being processed right now by another request.
    #[error("ops: a request with this idempotency key is in flight: {0}")]
    IdempotencyInFlight(String),

    /// An event or job failed validation before reaching the database.
    #[error("ops: invalid input: {0}")]
    Invalid(String),

    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// The result type this crate returns.
pub type Result<T> = std::result::Result<T, OpsError>;

/// Returns the partition that a subject belongs to.
///
/// The same subject always yields the same partition, on every replica and in
/// every language, which is what makes per-aggregate ordering hold without
/// coordination. See `docs/architecture/messaging.md`, "Subjects and
/// partitions".
///
/// `subject` is normally `"<aggregate>:<id>"`, for example `"entity:8f3c"`.
/// Partitioning on the aggregate id rather than the event id is deliberate: two
/// events about the same entity must be ordered relative to each other.
///
/// The partition count is a [`NonZeroU16`] rather than a plain integer so that
/// "zero partitions" is not representable — the invariant lives in the type
/// rather than in a runtime check and a comment asking callers to be careful.
#[must_use]
pub fn partition(subject: &str, partition_count: NonZeroU16) -> u16 {
    (fnv1a64(subject) % u64::from(partition_count.get())) as u16
}

/// Returns the partition for a subject against [`PARTITIONS`].
#[must_use]
pub fn partition_default(subject: &str) -> u16 {
    let count = NonZeroU16::new(PARTITIONS).expect("PARTITIONS is non-zero");
    partition(subject, count)
}

/// FNV-1a, 64-bit, over the UTF-8 bytes of `subject`.
///
/// Written out rather than pulled from a crate so the exact byte sequence is part
/// of this crate's contract: the Go implementation must produce the same numbers,
/// and a shared abstraction would hide that requirement rather than state it.
fn fnv1a64(subject: &str) -> u64 {
    let mut hash = FNV_OFFSET_BASIS_64;
    for byte in subject.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME_64);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(n: u16) -> NonZeroU16 {
        NonZeroU16::new(n).expect("test partition count must be non-zero")
    }

    /// The expected partitions are not derived from the implementation: they were
    /// computed independently from the FNV-1a 64 specification. `libs/ops/go`
    /// asserts the same table. If either language drifts, one of these fails —
    /// which is the only thing keeping two implementations honest without shared
    /// code.
    #[test]
    fn partition_matches_specification() {
        let subjects = [
            "entity:8f3c",
            "account:44a1",
            "activation:1b2d",
            "programme:pota",
            "",
        ];

        let want_32 = [8_u16, 8, 20, 15, 5];
        for (index, subject) in subjects.iter().enumerate() {
            assert_eq!(
                partition(subject, count(32)),
                want_32[index],
                "partition({subject:?}, 32)"
            );
        }

        let want_7 = [0_u16, 2, 4, 2, 2];
        for (index, subject) in subjects.iter().enumerate() {
            assert_eq!(
                partition(subject, count(7)),
                want_7[index],
                "partition({subject:?}, 7)"
            );
        }
    }

    #[test]
    fn partition_is_stable() {
        let first = partition("entity:8f3c", count(32));
        for _ in 0..100 {
            assert_eq!(partition("entity:8f3c", count(32)), first);
        }
    }

    #[test]
    fn partition_stays_in_range() {
        for partition_count in 1..=64_u16 {
            for subject in ["entity:8f3c", "entity:0000", "account:44a1", "x"] {
                let assigned = partition(subject, count(partition_count));
                assert!(
                    assigned < partition_count,
                    "partition({subject:?}, {partition_count}) = {assigned}, outside the range"
                );
            }
        }
    }

    #[test]
    fn partition_default_matches_the_constant() {
        assert_eq!(PARTITIONS, 32);
        assert_eq!(partition_default("entity:8f3c"), 8);
        assert_eq!(partition_default(""), 5);
    }
}

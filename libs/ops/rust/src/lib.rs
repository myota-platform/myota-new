//! The operational pattern shared by every service: transactional outbox,
//! idempotency records, job claiming and dead letters.
//!
//! There is no message broker (ADR-0004). Each service writes events to its own
//! `ops.outbox_event` inside the domain transaction, and peers pull them. Ordering
//! is per subject, which is what partition assignment is for.
//!
//! This crate exists twice — `libs/ops/go` and `libs/ops/rust` — because a Rust
//! service cannot link a Go module (ADR-0002). The two must agree on behaviour,
//! and `libs/ops/conformance/` is the suite that holds them to it.
//!
//! Chunk F1 provides only [`partition`]. The rest arrives with F4.

use std::num::NonZeroU16;

const FNV_OFFSET_BASIS_64: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME_64: u64 = 0x0000_0100_0000_01b3;

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
}

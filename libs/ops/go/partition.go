// Package ops holds the operational pattern shared by every service: the
// transactional outbox, idempotency records, job claiming, and dead letters.
//
// There is no message broker (ADR-0004). Each service writes events to its own
// ops.outbox_event inside the domain transaction, and peers pull them. Ordering
// is per subject, which is what partition assignment here is for: every event for
// one aggregate must land on one partition so a consumer applies them in order.
//
// This package exists twice — libs/ops/go and libs/ops/rust — because a Go
// service cannot link a Rust crate (ADR-0002). The two must agree on behaviour,
// and libs/ops/conformance/ is the suite that holds them to it.
//
// Chunk F1 provides only Partition. The rest arrives with F4.
package ops

import "fmt"

const (
	// fnvOffsetBasis64 and fnvPrime64 are the FNV-1a 64-bit parameters. FNV is
	// used rather than a cryptographic hash because partition assignment must be
	// stable across replicas and languages, not unpredictable.
	fnvOffsetBasis64 uint64 = 0xcbf29ce484222325
	fnvPrime64       uint64 = 0x100000001b3
)

// Partition returns the partition that a subject belongs to.
//
// The same subject always yields the same partition, on every replica and in
// every language, which is what makes per-aggregate ordering hold without
// coordination. See docs/architecture/messaging.md, "Subjects and partitions".
//
// subject is normally "<aggregate>:<id>", for example "entity:8f3c". Partitioning
// on the aggregate id rather than on the event id is deliberate: two events about
// the same entity must be ordered relative to each other.
//
// An error is returned for a non-positive partitionCount. That is a
// configuration mistake rather than input, but it is still returned as an error
// rather than panicking, so a misconfigured service fails its readiness check
// instead of crashing a replica.
func Partition(subject string, partitionCount int) (int, error) {
	if partitionCount <= 0 {
		return 0, fmt.Errorf("ops: partition count must be positive, got %d", partitionCount)
	}
	return int(fnv1a64(subject) % uint64(partitionCount)), nil
}

// fnv1a64 is FNV-1a, 64-bit, over the UTF-8 bytes of s.
//
// Written out rather than taken from hash/fnv so the exact byte sequence is part
// of this package's contract: the Rust implementation must produce the same
// numbers, and a shared abstraction would hide that requirement rather than state
// it.
func fnv1a64(s string) uint64 {
	hash := fnvOffsetBasis64
	for i := 0; i < len(s); i++ {
		hash ^= uint64(s[i])
		hash *= fnvPrime64
	}
	return hash
}

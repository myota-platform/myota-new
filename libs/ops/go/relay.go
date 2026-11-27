package ops

import (
	"context"
	"fmt"
	"log/slog"
	"time"
)

// Relay publishes outbox events, one partition at a time, under a lease.
//
// ## Why a relay at all, when there is no broker
//
// ADR-0004 removed the broker: the outbox row *is* the log, and consumers pull it over
// HTTP. So "publishing" is not a network operation — it is setting `published_at`,
// which is what makes an event visible on the feed.
//
// That sounds like a column, but it is what fixes a real ordering problem. A
// `bigserial` is allocated at INSERT time, not at COMMIT time, so two concurrent
// transactions can commit out of sequence order: transaction A takes seq 1, transaction
// B takes seq 2, and B commits first. A consumer reading the feed at that moment would
// see seq 2 and none of seq 1. If it advanced its cursor to 2, it would never see seq 1
// — a lost event, silently.
//
// The relay closes that window by publishing in ascending seq order under a lease, and
// by never moving a partition's published high-water mark past a seq it has not
// published. A feed reader that stops at the first gap is then correct: a gap below the
// high-water mark is a trimmed row (already acked), and a gap above it is simply not
// published yet.
//
// ## What is deliberately not solved here
//
// If transaction A holds seq 1 uncommitted for a long time while B commits seq 2, the
// relay sees only seq 2 and publishes it. A can then commit seq 1 afterwards. The
// high-water mark works at partition granularity, not per row, so this narrow window
// remains. It is recorded as a finding rather than papered over: closing it needs
// either transaction-id bookkeeping or a settle delay, and both cost more than the
// problem currently does.
//
// ## Concurrency
//
// A partition lease guarantees one relay per partition, so ordering holds when several
// replicas run. The `published_at IS NULL` predicate in the UPDATE is a second,
// independent guard: even if two relays raced on one partition, a row is published
// once.

// PartitionLease is a claimed partition and how long the claim is good for.
type PartitionLease struct {
	Partition int
	Owner     string
	ExpiresAt time.Time
	// LastSeq is the highest seq already published for this partition.
	LastSeq int64
}

// RelayBatch is the result of one publish pass.
type RelayBatch struct {
	// Published is the events made visible by this pass, in seq order.
	Published []Enqueued
	// Partition is the partition that was worked.
	Partition int
	// More reports whether the partition had more unpublished rows than the batch
	// allowed, so a caller can loop without sleeping.
	More bool
}

// ClaimPartition takes the lease on one free partition.
//
// Returns (nil, nil) when every partition is leased to someone else — that is the
// normal steady state with PARTITIONS replicas running, not an error. A caller loops
// and retries rather than treating it as a failure.
//
// `FOR UPDATE SKIP LOCKED` is what makes this safe: two replicas asking at the same
// moment take different rows, and neither blocks on the other. A lease that has
// expired is reclaimable, which is how a killed replica's partitions come back without
// a rebalancing protocol.
func (s *Store) ClaimPartition(ctx context.Context) (*PartitionLease, error) {
	// The partition rows are created on demand. Inserting them here rather than in the
	// DDL keeps Partitions a constant in code with the schema unchanged if it is ever
	// raised.
	if err := s.ensurePartitionRows(ctx); err != nil {
		return nil, err
	}

	var lease PartitionLease
	err := s.pool.QueryRow(ctx, `
		UPDATE ops.outbox_partition
		SET lease_owner = $1, lease_until = $2, updated_at = now()
		WHERE partition = (
			SELECT partition FROM ops.outbox_partition
			WHERE lease_until IS NULL OR lease_until < now()
			-- Least recently worked, not lowest number. Ordering by partition id
			-- would make every relay claim partition 0 first, release it, and claim
			-- it again — so with a handful of relays only the lowest few partitions
			-- are ever published and the rest starve. Release stamps updated_at, so a
			-- just-finished partition goes to the back of the queue.
			ORDER BY updated_at, partition
			FOR UPDATE SKIP LOCKED
			LIMIT 1)
		RETURNING partition, lease_owner, lease_until, last_seq`,
		s.owner, s.now().UTC().Add(s.leaseDuration),
	).Scan(&lease.Partition, &lease.Owner, &lease.ExpiresAt, &lease.LastSeq)
	if err != nil {
		if isNoRows(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("ops: claiming a partition: %w", err)
	}
	return &lease, nil
}

// ensurePartitionRows creates a row per partition if they are not there yet.
func (s *Store) ensurePartitionRows(ctx context.Context) error {
	_, err := s.pool.Exec(ctx, `
		INSERT INTO ops.outbox_partition (partition)
		SELECT generate_series(0, $1 - 1)
		ON CONFLICT (partition) DO NOTHING`, Partitions)
	if err != nil {
		return fmt.Errorf("ops: preparing partition rows: %w", err)
	}
	return nil
}

// PublishPartition publishes up to limit unpublished events for one partition, in seq
// order, and advances the partition's high-water mark.
//
// The caller is expected to hold the lease. It is not required for correctness — the
// `published_at IS NULL` predicate makes a double publish impossible — but it is what
// preserves ordering, and ordering is the only thing the lease is for.
func (s *Store) PublishPartition(ctx context.Context, partition int, limit int) (RelayBatch, error) {
	if limit <= 0 {
		limit = 100
	}

	// A CTE rather than a bare UPDATE, for two reasons: `UPDATE … RETURNING` has no
	// ORDER BY, and the picking step needs `FOR UPDATE SKIP LOCKED` so two relays that
	// somehow hold the same partition still take disjoint rows instead of queueing
	// behind each other. The `published_at IS NULL` predicate is repeated in the UPDATE
	// so a row another relay published between the pick and the write is skipped rather
	// than published twice.
	rows, err := s.pool.Query(ctx, `
		WITH picked AS (
			SELECT seq FROM ops.outbox_event
			WHERE partition = $1 AND published_at IS NULL
			ORDER BY seq
			LIMIT $2
			FOR UPDATE SKIP LOCKED
		), updated AS (
			UPDATE ops.outbox_event AS e
			SET published_at = now()
			FROM picked
			WHERE e.seq = picked.seq AND e.published_at IS NULL
			RETURNING e.seq, e.id, e.type, e.subject, e.partition, e.payload, e.created_at
		)
		SELECT seq, id, type, subject, partition, payload, created_at
		FROM updated
		ORDER BY seq`, partition, limit)
	if err != nil {
		return RelayBatch{}, fmt.Errorf("ops: publishing partition %d: %w", partition, err)
	}
	defer rows.Close()

	batch := RelayBatch{Partition: partition, Published: make([]Enqueued, 0, limit)}
	for rows.Next() {
		var stored Enqueued
		if err := rows.Scan(&stored.Seq, &stored.ID, &stored.Type, &stored.Subject,
			&stored.Partition, &stored.Payload, &stored.Time); err != nil {
			return RelayBatch{}, fmt.Errorf("ops: scanning a published event: %w", err)
		}
		batch.Published = append(batch.Published, stored)
	}
	if err := rows.Err(); err != nil {
		return RelayBatch{}, fmt.Errorf("ops: publishing partition %d: %w", partition, err)
	}

	if len(batch.Published) == 0 {
		return batch, nil
	}

	// Advance the mark, never backwards. GREATEST rather than assignment so a relay
	// that publishes an older row after a newer one — possible if a batch is retried —
	// cannot move the mark back and make a consumer re-read.
	lastSeq := batch.Published[len(batch.Published)-1].Seq
	if _, err := s.pool.Exec(ctx, `
		UPDATE ops.outbox_partition
		SET last_seq = GREATEST(last_seq, $2), updated_at = now()
		WHERE partition = $1`, partition, lastSeq); err != nil {
		return RelayBatch{}, fmt.Errorf("ops: advancing the high-water mark for partition %d: %w", partition, err)
	}

	// Was there more than this batch? Asking costs one index probe and saves the
	// caller a sleep to discover it.
	var more bool
	if err := s.pool.QueryRow(ctx, `
		SELECT EXISTS (
			SELECT 1 FROM ops.outbox_event
			WHERE partition = $1 AND published_at IS NULL AND seq > $2)`,
		partition, lastSeq).Scan(&more); err != nil {
		return RelayBatch{}, fmt.Errorf("ops: checking for more work on partition %d: %w", partition, err)
	}
	batch.More = more
	return batch, nil
}

// ReleasePartition gives up a lease early. Not required — leases expire — but it makes
// a partition available to another replica immediately instead of after the lease
// duration.
func (s *Store) ReleasePartition(ctx context.Context, partition int) error {
	_, err := s.pool.Exec(ctx, `
		UPDATE ops.outbox_partition
		SET lease_owner = NULL, lease_until = NULL, updated_at = now()
		WHERE partition = $1 AND lease_owner = $2`, partition, s.owner)
	if err != nil {
		return fmt.Errorf("ops: releasing partition %d: %w", partition, err)
	}
	return nil
}

// RelayOnce claims one partition and publishes one batch. It returns how many events
// were published, and 0 when no partition was free.
//
// Separated from Relay so a test can drive exactly one pass and assert on it, instead
// of racing a background loop.
func (s *Store) RelayOnce(ctx context.Context, limit int) (int, error) {
	lease, err := s.ClaimPartition(ctx)
	if err != nil {
		return 0, err
	}
	if lease == nil {
		return 0, nil
	}
	defer func() {
		if err := s.ReleasePartition(context.WithoutCancel(ctx), lease.Partition); err != nil {
			slog.ErrorContext(ctx, "releasing a partition lease", "partition", lease.Partition, "error", err)
		}
	}()

	batch, err := s.PublishPartition(ctx, lease.Partition, limit)
	if err != nil {
		return 0, err
	}
	return len(batch.Published), nil
}

// RelayDrain publishes everything currently unpublished, then returns.
//
// Used by tests and by a shutdown drain; a running service calls RelayOnce on a ticker
// instead. It walks every partition rather than using ClaimPartition, because a
// single claim can return a partition with no work while another partition still has
// rows — and stopping there would make the drain silently incomplete.
func (s *Store) RelayDrain(ctx context.Context, limit int) (int, error) {
	total := 0
	for {
		progressed := false
		for partition := 0; partition < Partitions; partition++ {
			published, err := s.drainPartition(ctx, partition, limit)
			if err != nil {
				return total, err
			}
			if published > 0 {
				progressed = true
				total += published
			}
		}
		if !progressed {
			return total, nil
		}
	}
}

// drainPartition publishes one batch from a specific partition, if this replica can
// lease it. Returns 0 when the partition is held elsewhere, which is why the caller
// keeps walking the rest.
func (s *Store) drainPartition(ctx context.Context, partition int, limit int) (int, error) {
	if err := s.ensurePartitionRows(ctx); err != nil {
		return 0, err
	}

	var leased bool
	err := s.pool.QueryRow(ctx, `
		UPDATE ops.outbox_partition
		SET lease_owner = $1, lease_until = $2, updated_at = now()
		WHERE partition = $3 AND (lease_until IS NULL OR lease_until < now())
		RETURNING true`,
		s.owner, s.now().UTC().Add(s.leaseDuration), partition).Scan(&leased)
	if err != nil {
		if isNoRows(err) {
			return 0, nil
		}
		return 0, fmt.Errorf("ops: claiming partition %d: %w", partition, err)
	}

	defer func() {
		if err := s.ReleasePartition(context.WithoutCancel(ctx), partition); err != nil {
			slog.ErrorContext(ctx, "releasing a partition lease", "partition", partition, "error", err)
		}
	}()

	batch, err := s.PublishPartition(ctx, partition, limit)
	if err != nil {
		return 0, err
	}
	return len(batch.Published), nil
}

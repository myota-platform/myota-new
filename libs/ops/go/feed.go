package ops

import (
	"context"
	"fmt"
	"time"
)

// Feed is one batch of the event log, as `GET /internal/events` returns it.
type Feed struct {
	Events []Enqueued `json:"events"`
	// NextSeq is the cursor to pass as `after`. It advances even when Events is empty,
	// so a consumer filtering by type does not re-read the same window forever.
	NextSeq int64 `json:"nextSeq"`
	// More reports whether rows exist past NextSeq.
	More bool `json:"more"`
}

// ReadEvents returns published events with `seq > after`, in seq order.
//
// Only published rows. `published_at IS NOT NULL` is the relay's high-water mark in
// practice: a row is visible once the relay has seen it in order, which is what keeps
// a consumer from reading past an event that committed late.
//
// `types` filters by event type server-side. An unsubscribed type is never turned into
// a job, so filtering here saves transfer rather than correctness.
func (s *Store) ReadEvents(ctx context.Context, after int64, limit int, types []string) (Feed, error) {
	if limit <= 0 || limit > 500 {
		limit = 100
	}
	if after < 0 {
		after = 0
	}

	// One row more than asked for, so `more` is known without a second query.
	rows, err := s.pool.Query(ctx, `
		SELECT seq, id, type, subject, partition, payload, created_at
		FROM ops.outbox_event
		WHERE seq > $1
		  AND published_at IS NOT NULL
		  -- coalesce, because a nil slice arrives as SQL NULL and cardinality(NULL)
		  -- is NULL: the whole OR would be NULL and every row would be filtered out.
		  -- That reads as "the feed is empty" rather than as a bug, which is how it
		  -- survived the first run of these tests.
		  AND (coalesce(cardinality($3::text[]), 0) = 0 OR type = ANY($3))
		ORDER BY seq
		LIMIT $2 + 1`, after, limit, types)
	if err != nil {
		return Feed{}, fmt.Errorf("ops: reading events: %w", err)
	}
	defer rows.Close()

	feed := Feed{Events: make([]Enqueued, 0, limit), NextSeq: after}
	for rows.Next() {
		var event Enqueued
		if err := rows.Scan(&event.Seq, &event.ID, &event.Type, &event.Subject,
			&event.Partition, &event.Payload, &event.Time); err != nil {
			return Feed{}, fmt.Errorf("ops: scanning an event: %w", err)
		}
		if len(feed.Events) == limit {
			feed.More = true
			break
		}
		feed.Events = append(feed.Events, event)
		feed.NextSeq = event.Seq
	}
	if err := rows.Err(); err != nil {
		return Feed{}, fmt.Errorf("ops: reading events: %w", err)
	}
	return feed, nil
}

// AckEvents records how far a consumer has durably applied, so the producer can trim
// without trimming past a live consumer.
//
// Monotonic: a lower seq is ignored rather than applied, because a replica that has
// fallen behind must not be able to move the floor backwards and un-protect events
// another consumer still needs.
func (s *Store) AckEvents(ctx context.Context, consumer, source string, seq int64) error {
	_, err := s.pool.Exec(ctx, `
		INSERT INTO ops.event_ack (consumer, source, last_acked_seq, reported_at)
		VALUES ($1, $2, $3, now())
		ON CONFLICT (consumer, source) DO UPDATE
		SET last_acked_seq = GREATEST(ops.event_ack.last_acked_seq, EXCLUDED.last_acked_seq),
		    reported_at = now()`,
		consumer, source, seq)
	if err != nil {
		return fmt.Errorf("ops: acknowledging events for %s/%s: %w", consumer, source, err)
	}
	return nil
}

// Trim deletes published events the system no longer needs, with two independent
// bounds. Both are safety rails and either alone would be wrong:
//
//   - **The age ceiling** bounds storage even if a consumer is stopped or gone.
//   - **The ack floor** never trims past what a live consumer has confirmed, so a slow
//     consumer does not silently lose events it has not applied.
//
// A consumer that falls behind the age ceiling has lost events. That is a detected
// condition rather than a silent one: the lag metric in `docs/architecture/messaging.md`
// is what makes it visible, and a full resync is the recovery.
func (s *Store) Trim(ctx context.Context, retention time.Duration, keepRecent int64) (int64, error) {
	if retention <= 0 {
		retention = 14 * 24 * time.Hour
	}
	if keepRecent < 0 {
		keepRecent = 0
	}

	tag, err := s.pool.Exec(ctx, `
		DELETE FROM ops.outbox_event
		WHERE published_at IS NOT NULL
		  AND created_at < now() - $1::interval
		  AND seq < COALESCE((SELECT min(last_acked_seq) FROM ops.event_ack), 0)
		  AND seq < (SELECT max(seq) FROM ops.outbox_event) - $2`,
		retention, keepRecent)
	if err != nil {
		return 0, fmt.Errorf("ops: trimming the outbox: %w", err)
	}
	return tag.RowsAffected(), nil
}

// EventLag reports, per consumer and source, how far behind the newest event the
// consumer's ack is. This is the metric that turns "a consumer is stuck" into an
// alert instead of a discovery.
func (s *Store) EventLag(ctx context.Context) (map[string]time.Duration, error) {
	rows, err := s.pool.Query(ctx, `
		SELECT a.consumer, a.source,
		       now() - (SELECT created_at FROM ops.outbox_event
		                 WHERE seq > a.last_acked_seq
		                 ORDER BY seq LIMIT 1) AS lag
		FROM ops.event_ack a`)
	if err != nil {
		return nil, fmt.Errorf("ops: computing event lag: %w", err)
	}
	defer rows.Close()

	lags := make(map[string]time.Duration, 4)
	for rows.Next() {
		var consumer, source string
		var lag *time.Duration
		if err := rows.Scan(&consumer, &source, &lag); err != nil {
			return nil, fmt.Errorf("ops: computing event lag: %w", err)
		}
		if lag != nil {
			lags[consumer+"/"+source] = *lag
		}
	}
	return lags, rows.Err()
}

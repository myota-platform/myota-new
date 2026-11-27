package ops

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5"
)

// Event is one fact, ready to be written to the outbox.
//
// The same value is the row, the feed item and the conformance fixture's "given",
// so there is one description of an event rather than three that drift.
type Event struct {
	// ID is the event's identity, and on the consuming side it becomes the job id.
	// That equality is what makes a duplicate delivery a no-op rather than a double
	// count: the consumer inserts its job with ON CONFLICT (id) DO NOTHING.
	ID uuid.UUID
	// Type is `<service>.<aggregate>.<past-tense fact>.v<major>`, e.g.
	// `geodata.entity.published.v1`.
	Type string
	// Subject is `<aggregate>:<id>`, e.g. `entity:8f3c…`. The partition is derived
	// from this, never from time or a counter, so every event about one aggregate is
	// ordered relative to the others.
	Subject string
	// Payload is the event body. It is stored as jsonb, so a payload that is not
	// valid JSON fails at write time rather than at read time in a consumer.
	Payload json.RawMessage
	// Time is when the fact became true. Defaults to now when zero.
	Time time.Time
}

// Enqueued is an event as stored: the caller's Event plus the sequence number the
// database assigned.
type Enqueued struct {
	Event
	// Seq is monotonic per database and not contiguous. Callers use it to assert
	// ordering; production code treats it as a cursor.
	Seq int64
	// Partition is the partition the event landed on.
	Partition int
}

// NewEvent builds an event with a fresh id and the current time.
func NewEvent(eventType, subject string, payload any) (Event, error) {
	body, err := json.Marshal(payload)
	if err != nil {
		return Event{}, fmt.Errorf("ops: encoding the event payload: %w", err)
	}
	return Event{
		ID:      uuid.New(),
		Type:    eventType,
		Subject: subject,
		Payload: body,
		Time:    time.Now().UTC(),
	}, nil
}

// Validate rejects an event that cannot be stored meaningfully.
//
// Checked here rather than by a database constraint because the message can then name
// the field. A payload that is not valid JSON would otherwise be a 500 from a driver
// error.
func (e Event) Validate() error {
	if e.ID == uuid.Nil {
		return fmt.Errorf("ops: an event needs an id")
	}
	if e.Type == "" {
		return fmt.Errorf("ops: an event needs a type")
	}
	if e.Subject == "" {
		return fmt.Errorf("ops: an event needs a subject")
	}
	if len(e.Payload) == 0 {
		return fmt.Errorf("ops: an event needs a payload")
	}
	if !json.Valid(e.Payload) {
		return fmt.Errorf("ops: the payload of %s is not valid JSON", e.Type)
	}
	return nil
}

// Enqueue writes an event inside the caller's transaction.
//
// It takes a `pgx.Tx` rather than using the pool on purpose: the whole promise of the
// transactional outbox is that the event and the domain change commit together. An
// Enqueue that could open its own transaction would make the wrong thing easy, so the
// signature makes it impossible.
//
// The partition is computed here from the subject, so no caller can choose one and
// silently break ordering for its aggregate.
func (s *Store) Enqueue(ctx context.Context, tx pgx.Tx, event Event) (Enqueued, error) {
	if err := event.Validate(); err != nil {
		return Enqueued{}, err
	}
	partition, err := Partition(event.Subject, Partitions)
	if err != nil {
		return Enqueued{}, err
	}
	if event.Time.IsZero() {
		event.Time = s.now().UTC()
	}

	var seq int64
	err = tx.QueryRow(ctx, `
		INSERT INTO ops.outbox_event (id, type, subject, partition, payload, created_at)
		VALUES ($1, $2, $3, $4, $5, $6)
		RETURNING seq`,
		event.ID, event.Type, event.Subject, partition, event.Payload, event.Time,
	).Scan(&seq)
	if err != nil {
		return Enqueued{}, fmt.Errorf("ops: enqueueing %s for %s: %w", event.Type, event.Subject, err)
	}

	return Enqueued{Event: event, Seq: seq, Partition: partition}, nil
}

// EnqueueInTx runs fn in a transaction and enqueues nothing itself — a convenience for
// the common shape where a domain write and one or more events share a commit.
func (s *Store) EnqueueInTx(ctx context.Context, events []Event, fn func(pgx.Tx) error) ([]Enqueued, error) {
	enqueued := make([]Enqueued, 0, len(events))
	err := s.InTx(ctx, func(tx pgx.Tx) error {
		for _, event := range events {
			stored, err := s.Enqueue(ctx, tx, event)
			if err != nil {
				return err
			}
			enqueued = append(enqueued, stored)
		}
		if fn != nil {
			return fn(tx)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return enqueued, nil
}

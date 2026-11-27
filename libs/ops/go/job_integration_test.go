//go:build integration

package ops_test

import (
	"context"
	"encoding/json"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"

	"github.com/myota-platform/myota-new/libs/ops"
)

// TestConcurrentClaimsAreDisjoint is acceptance criterion 2: four claimers, disjoint
// rows, no duplicates and nothing lost.
func TestConcurrentClaimsAreDisjoint(t *testing.T) {
	pool, store := freshDatabase(t)
	ctx := context.Background()

	const jobs = 200
	enqueued := make(map[uuid.UUID]struct{}, jobs)
	for i := range jobs {
		id, inserted, err := store.EnqueueJob(ctx, ops.Job{
			Kind:    "import.preprocess",
			Payload: json.RawMessage(fmt.Sprintf(`{"n":%d}`, i)),
		})
		if err != nil {
			t.Fatalf("enqueueing job %d: %v", i, err)
		}
		if !inserted {
			t.Fatalf("job %d was not inserted", i)
		}
		enqueued[id] = struct{}{}
	}

	const claimers = 4
	var (
		mutex    sync.Mutex
		claimed  = make(map[uuid.UUID]int, jobs)
		waiting  sync.WaitGroup
		failures = make([]error, claimers)
	)
	start := make(chan struct{})

	for slot := range claimers {
		waiting.Add(1)
		go func(slot int) {
			defer waiting.Done()
			// A distinct owner per claimer: two sharing an owner could extend each
			// other's leases, which is exactly what the owner field exists to prevent.
			claimer := ops.New(pool, ops.WithOwner(fmt.Sprintf("claimer-%d", slot)))
			<-start
			for {
				jobs, err := claimer.Claim(ctx, 7)
				if err != nil {
					failures[slot] = err
					return
				}
				if len(jobs) == 0 {
					return // the queue is drained
				}
				mutex.Lock()
				for _, job := range jobs {
					claimed[job.ID]++
				}
				mutex.Unlock()
			}
		}(slot)
	}
	close(start)
	waiting.Wait()

	for slot, err := range failures {
		if err != nil {
			t.Fatalf("claimer %d failed: %v", slot, err)
		}
	}

	if len(claimed) != jobs {
		t.Errorf("claimed %d distinct jobs, want %d", len(claimed), jobs)
	}
	for id, count := range claimed {
		if count != 1 {
			t.Errorf("job %s was claimed %d times, want 1", id, count)
		}
	}
	for id := range enqueued {
		if claimed[id] == 0 {
			t.Errorf("job %s was never claimed", id)
		}
	}

	// Every claimed job is `leased` exactly once in the table, not `queued`.
	var leaked int
	if err := pool.QueryRow(ctx, `SELECT count(*) FROM ops.job WHERE state = 'queued'`).Scan(&leaked); err != nil {
		t.Fatalf("counting queued jobs: %v", err)
	}
	if leaked != 0 {
		t.Errorf("%d job(s) are still queued after every claimer finished", leaked)
	}
}

// TestExpiredLeaseIsReclaimable: a killed replica's work comes back without a recovery
// sweep, because the lease lapses rather than being released.
func TestExpiredLeaseIsReclaimable(t *testing.T) {
	pool, _ := freshDatabase(t)
	ctx := context.Background()

	store := ops.New(pool)
	if _, _, err := store.EnqueueJob(ctx, ops.Job{Kind: "entity.enrich", Payload: json.RawMessage(`{}`)}); err != nil {
		t.Fatalf("enqueueing: %v", err)
	}

	// A very short lease, so the test does not sleep for minutes.
	dying := ops.New(pool, ops.WithOwner("doomed"), ops.WithLeaseDuration(time.Nanosecond))
	claimed, err := dying.Claim(ctx, 1)
	if err != nil || len(claimed) != 1 {
		t.Fatalf("first claim: %d jobs, err %v", len(claimed), err)
	}

	// A different replica, with a normal lease. It must see the job once the first
	// lease has lapsed — which it already has.
	time.Sleep(5 * time.Millisecond)
	successor := ops.New(pool, ops.WithOwner("successor"))
	reclaimed, err := successor.Claim(ctx, 1)
	if err != nil {
		t.Fatalf("second claim: %v", err)
	}
	if len(reclaimed) != 1 {
		t.Fatalf("the expired lease was not reclaimable: got %d jobs", len(reclaimed))
	}
	if reclaimed[0].ID != claimed[0].ID {
		t.Errorf("reclaimed job %s, want %s", reclaimed[0].ID, claimed[0].ID)
	}
	if reclaimed[0].Attempt != 2 {
		t.Errorf("attempt = %d after a reclaim, want 2", reclaimed[0].Attempt)
	}
}

// TestFailRetriesThenDies: eight failures reach `dead`, and a dead job has a dead
// letter. That transition is what stops a poison message spinning forever.
func TestFailRetriesThenDies(t *testing.T) {
	pool, store := freshDatabase(t)
	ctx := context.Background()

	id, _, err := store.EnqueueJob(ctx, ops.Job{
		Kind:        "import.fetch",
		Payload:     json.RawMessage(`{"url":"https://example.org/x.geojson"}`),
		MaxAttempts: 3,
	})
	if err != nil {
		t.Fatalf("enqueueing: %v", err)
	}

	for attempt := 1; attempt <= 3; attempt++ {
		claimed, err := store.Claim(ctx, 1)
		if err != nil {
			t.Fatalf("claim %d: %v", attempt, err)
		}
		if len(claimed) != 1 {
			t.Fatalf("claim %d returned %d jobs, want 1", attempt, len(claimed))
		}
		dead, err := store.Fail(ctx, claimed[0].ID, fmt.Errorf("upstream said no"))
		if err != nil {
			t.Fatalf("failing attempt %d: %v", attempt, err)
		}
		wantDead := attempt == 3
		if dead != wantDead {
			t.Fatalf("attempt %d: dead = %v, want %v", attempt, dead, wantDead)
		}
		// Make the retry immediately available rather than waiting for the backoff.
		if !dead {
			if _, err := pool.Exec(ctx, `UPDATE ops.job SET available_at = now() WHERE id = $1`, id); err != nil {
				t.Fatalf("clearing the backoff: %v", err)
			}
		}
	}

	var state string
	if err := pool.QueryRow(ctx, `SELECT state FROM ops.job WHERE id = $1`, id).Scan(&state); err != nil {
		t.Fatalf("reading the job: %v", err)
	}
	if state != "dead" {
		t.Errorf("state = %q, want dead", state)
	}

	var deadLetters int
	if err := pool.QueryRow(ctx,
		`SELECT count(*) FROM ops.dead_letter WHERE ref_id = $1 AND source_table = 'ops.job'`, id,
	).Scan(&deadLetters); err != nil {
		t.Fatalf("counting dead letters: %v", err)
	}
	if deadLetters != 1 {
		t.Errorf("%d dead letter(s) for the dead job, want 1", deadLetters)
	}

	// A dead job is not claimable again.
	if claimed, err := store.Claim(ctx, 1); err != nil || len(claimed) != 0 {
		t.Errorf("a dead job was claimed again: %d jobs, err %v", len(claimed), err)
	}
}

// TestCompleteIsIdempotent: a worker that finishes and then crashes before recording it
// will be retried, and the second completion must not be an error.
func TestCompleteIsIdempotent(t *testing.T) {
	_, store := freshDatabase(t)
	ctx := context.Background()

	id, _, err := store.EnqueueJob(ctx, ops.Job{Kind: "award.evaluate", Payload: json.RawMessage(`{}`)})
	if err != nil {
		t.Fatalf("enqueueing: %v", err)
	}
	if _, err := store.Claim(ctx, 1); err != nil {
		t.Fatalf("claiming: %v", err)
	}

	if err := store.Complete(ctx, id); err != nil {
		t.Fatalf("first completion: %v", err)
	}
	if err := store.Complete(ctx, id); err != nil {
		t.Errorf("second completion: %v, want nil", err)
	}

	// Completing a job that never existed is a caller bug and must say so.
	if err := store.Complete(ctx, uuid.New()); err == nil {
		t.Error("completing an unknown job returned nil")
	}
}

// TestScheduledJobFiresOncePerSlot is what makes a cron loop a table: the partial unique
// index rejects a second insert for the same (kind, slot), so ten replicas insert one
// row.
func TestScheduledJobFiresOncePerSlot(t *testing.T) {
	pool, store := freshDatabase(t)
	ctx := context.Background()

	slot := time.Now().UTC().Truncate(time.Hour)
	first := ops.Job{Kind: "account.retention", Payload: json.RawMessage(`{}`), ScheduledFor: &slot}

	if _, inserted, err := store.EnqueueJob(ctx, first); err != nil || !inserted {
		t.Fatalf("first insert: inserted=%v err=%v", inserted, err)
	}
	// The same slot, a different id — as another replica would produce.
	if _, inserted, err := store.EnqueueJob(ctx, ops.Job{
		Kind: "account.retention", Payload: json.RawMessage(`{}`), ScheduledFor: &slot,
	}); err != nil {
		t.Fatalf("second insert: %v", err)
	} else if inserted {
		t.Error("a second job was inserted for the same (kind, slot)")
	}

	var count int
	if err := pool.QueryRow(ctx,
		`SELECT count(*) FROM ops.job WHERE kind = 'account.retention'`).Scan(&count); err != nil {
		t.Fatalf("counting: %v", err)
	}
	if count != 1 {
		t.Errorf("%d retention jobs for one slot, want 1", count)
	}
}

// TestEnqueueJobIdIsTheEventId pins the property that replaces broker de-duplication:
// a consumer inserts its job with the event's id, so pulling the same event twice
// inserts once.
func TestEnqueueJobIdIsTheEventId(t *testing.T) {
	_, store := freshDatabase(t)
	ctx := context.Background()

	eventID := uuid.New()
	if _, inserted, err := store.EnqueueJob(ctx, ops.Job{
		ID: eventID, Kind: "notification.send", Payload: json.RawMessage(`{}`),
	}); err != nil || !inserted {
		t.Fatalf("first insert: inserted=%v err=%v", inserted, err)
	}
	if _, inserted, err := store.EnqueueJob(ctx, ops.Job{
		ID: eventID, Kind: "notification.send", Payload: json.RawMessage(`{}`),
	}); err != nil {
		t.Fatalf("second insert: %v", err)
	} else if inserted {
		t.Error("the same event id produced a second job")
	}
}

// TestBackoffIsBounded: an unbounded exponential would make the eighth attempt of a
// failing import wait 21 hours, which is indistinguishable from never.
func TestBackoffIsBounded(t *testing.T) {
	cases := []struct {
		attempt int
		want    time.Duration
	}{
		{0, 0},
		{1, 2 * time.Second},
		{2, 4 * time.Second},
		{3, 8 * time.Second},
		{6, 60 * time.Second},
		{8, 60 * time.Second},
		{20, 60 * time.Second},
	}
	for _, c := range cases {
		if got := ops.Backoff(c.attempt); got != c.want {
			t.Errorf("Backoff(%d) = %s, want %s", c.attempt, got, c.want)
		}
	}
}

// TestIdempotencyReplayAndConflict covers the two outcomes a caller has to tell apart.
func TestIdempotencyReplayAndConflict(t *testing.T) {
	_, store := freshDatabase(t)
	ctx := context.Background()

	const scope, key = "geodata.imports", "4f2a1c9e-0000-4000-8000-000000000001"
	body := []byte(`{"sourceId":"1c5e"}`)
	hash := ops.HashRequest(body)

	outcome, stored, err := store.ClaimIdempotency(ctx, scope, key, hash, 0)
	if err != nil {
		t.Fatalf("claiming: %v", err)
	}
	if outcome != ops.IdempotencyProceed {
		t.Fatalf("outcome = %v, want proceed", outcome)
	}
	if stored != nil {
		t.Fatal("a first claim returned a stored response")
	}

	if err := store.SaveIdempotentResponse(ctx, scope, key, 201, []byte(`{"id":"abc"}`)); err != nil {
		t.Fatalf("saving the response: %v", err)
	}

	// Same key, same body: replay.
	outcome, stored, err = store.ClaimIdempotency(ctx, scope, key, hash, 0)
	if err != nil {
		t.Fatalf("replaying: %v", err)
	}
	if outcome != ops.IdempotencyReplay || stored == nil {
		t.Fatalf("outcome = %v, stored = %v; want a replay", outcome, stored)
	}
	if stored.Status != 201 {
		t.Errorf("replayed status %d, want 201", stored.Status)
	}

	// Same key, different body: conflict.
	otherHash := ops.HashRequest([]byte(`{"sourceId":"ffff"}`))
	if _, _, err := store.ClaimIdempotency(ctx, scope, key, otherHash, 0); err == nil {
		t.Fatal("a key reused with a different body was accepted")
	} else if !errorIs(err, ops.ErrIdempotencyConflict) {
		t.Errorf("err = %v, want ErrIdempotencyConflict", err)
	}
}

// TestIdempotencyInFlight: a second request with the same key while the first is still
// running must not execute alongside it.
func TestIdempotencyInFlight(t *testing.T) {
	_, store := freshDatabase(t)
	ctx := context.Background()

	const scope, key = "core.programmes", "abc"
	hash := ops.HashRequest([]byte(`{}`))

	if outcome, _, err := store.ClaimIdempotency(ctx, scope, key, hash, 0); err != nil || outcome != ops.IdempotencyProceed {
		t.Fatalf("first claim: outcome=%v err=%v", outcome, err)
	}

	_, _, err := store.ClaimIdempotency(ctx, scope, key, hash, 0)
	if err == nil {
		t.Fatal("a concurrent claim succeeded")
	}
	if !errorIs(err, ops.ErrIdempotencyInFlight) {
		t.Errorf("err = %v, want ErrIdempotencyInFlight", err)
	}

	// Releasing the key lets a retry proceed, which is what happens after a 500.
	if err := store.ReleaseIdempotency(ctx, scope, key); err != nil {
		t.Fatalf("releasing: %v", err)
	}
	if outcome, _, err := store.ClaimIdempotency(ctx, scope, key, hash, 0); err != nil || outcome != ops.IdempotencyProceed {
		t.Errorf("after release: outcome=%v err=%v", outcome, err)
	}
}

// TestTrimRespectsTheAckFloor: the producer must never trim past what a live consumer
// has confirmed, because that would silently lose events for the one consumer that
// needed them.
func TestTrimRespectsTheAckFloor(t *testing.T) {
	pool, store := freshDatabase(t)
	ctx := context.Background()

	for i := range 10 {
		enqueue(t, store, fmt.Sprintf("entity:%d", i), "geodata.entity.published.v1")
	}
	if _, err := store.RelayDrain(ctx, 100); err != nil {
		t.Fatalf("draining: %v", err)
	}

	// The consumer has applied up to seq 3. "Applied up to 3" means 1 and 2 are done
	// and 3 is not yet safe to remove, so exactly two rows may go.
	if err := store.AckEvents(ctx, "activity", "geodata", 3); err != nil {
		t.Fatalf("acking: %v", err)
	}

	// Age the rows so the ceiling does not protect them.
	if _, err := pool.Exec(ctx, `UPDATE ops.outbox_event SET created_at = now() - interval '30 days'`); err != nil {
		t.Fatalf("ageing rows: %v", err)
	}

	if _, err := store.Trim(ctx, 14*24*time.Hour, 0); err != nil {
		t.Fatalf("trimming: %v", err)
	}

	var remaining int
	if err := pool.QueryRow(ctx, `SELECT count(*) FROM ops.outbox_event`).Scan(&remaining); err != nil {
		t.Fatalf("counting: %v", err)
	}
	if remaining != 8 {
		t.Errorf("%d rows remain after a trim bounded by an ack floor of 3, want 8 "+
			"(seq 1 and 2 are below the floor and may go; 3 upward must stay)", remaining)
	}

	// With the consumer caught up, the trim is allowed to proceed.
	if err := store.AckEvents(ctx, "activity", "geodata", 10); err != nil {
		t.Fatalf("acking: %v", err)
	}
	trimmed, err := store.Trim(ctx, 14*24*time.Hour, 0)
	if err != nil {
		t.Fatalf("trimming: %v", err)
	}
	// A floor of 10 releases seq 1..9; 1 and 2 are already gone, so 7 remain to trim.
	if trimmed != 7 {
		t.Errorf("trimmed %d rows once the consumer caught up, want 7", trimmed)
	}
}

// TestAckDoesNotRewind: a replica that falls behind must not be able to move the ack
// floor backwards and un-protect events another consumer still needs.
func TestAckDoesNotRewind(t *testing.T) {
	pool, store := freshDatabase(t)
	ctx := context.Background()

	if err := store.AckEvents(ctx, "activity", "core", 100); err != nil {
		t.Fatalf("acking 100: %v", err)
	}
	if err := store.AckEvents(ctx, "activity", "core", 5); err != nil {
		t.Fatalf("acking 5: %v", err)
	}

	var floor int64
	if err := pool.QueryRow(ctx,
		`SELECT last_acked_seq FROM ops.event_ack WHERE consumer = 'activity' AND source = 'core'`).Scan(&floor); err != nil {
		t.Fatalf("reading the floor: %v", err)
	}
	if floor != 100 {
		t.Errorf("ack floor = %d after a lower report, want 100", floor)
	}
}

// errorIs is errors.Is without importing errors in every test file.
func errorIs(err, target error) bool {
	for err != nil {
		if err == target {
			return true
		}
		unwrapped, ok := err.(interface{ Unwrap() error })
		if !ok {
			return false
		}
		err = unwrapped.Unwrap()
	}
	return false
}

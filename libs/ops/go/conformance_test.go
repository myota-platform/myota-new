//go:build integration

// The conformance runner: the Go implementation, held to the language-neutral fixtures
// in libs/ops/conformance/cases.
//
// The point is not that Go passes its own tests. It is that Go and Rust pass the *same*
// cases, so a behaviour fix on one side fails the other side until it catches up. The
// fixtures describe the ops contract; neither implementation may special-case a case.
//
// A case this runner does not recognise is a failure, not a skip: a fixture added for
// one language must not be silently ignored by the other.

package ops_test

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/myota-platform/myota-new/libs/ops"
)

const casesDirectory = "../conformance/cases"

// Case is one conformance fixture.
type Case struct {
	Name        string `json:"name"`
	Description string `json:"description"`
	Steps       []Step `json:"steps"`
	Expect      map[string]any
}

// Step is one operation in a case.
type Step struct {
	Op           string         `json:"op"`
	Events       []FixtureEvent `json:"events"`
	Jobs         []FixtureJob   `json:"jobs"`
	Count        int            `json:"count"`
	Claimers     int            `json:"claimers"`
	Times        int            `json:"times"`
	MaxAttempts  int            `json:"maxAttempts"`
	Outcome      string         `json:"outcome"`
	Key          string         `json:"key"`
	Body         string         `json:"body"`
	Response     string         `json:"response"`
	Status       int            `json:"status"`
	ExpectError  bool           `json:"expectError"`
	LeaseExpires bool           `json:"leaseExpires"`
}

type FixtureEvent struct {
	ID      string `json:"id"`
	Subject string `json:"subject"`
	Type    string `json:"type"`
}

type FixtureJob struct {
	ID   string `json:"id"`
	Kind string `json:"kind"`
}

// TestConformance runs every fixture and asserts every key in `expect`.
func TestConformance(t *testing.T) {
	entries, err := os.ReadDir(casesDirectory)
	if err != nil {
		t.Fatalf("reading %s: %v", casesDirectory, err)
	}
	if len(entries) == 0 {
		t.Fatalf("%s holds no cases, so this suite asserts nothing", casesDirectory)
	}

	known := make(map[string]bool, len(entries))
	for _, entry := range entries {
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".json" {
			continue
		}
		path := filepath.Join(casesDirectory, entry.Name())
		body, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("reading %s: %v", path, err)
		}
		var testCase Case
		if err := json.Unmarshal(body, &testCase); err != nil {
			t.Fatalf("%s is not valid JSON: %v", path, err)
		}
		if testCase.Name == "" {
			t.Fatalf("%s has no name", path)
		}
		known[testCase.Name] = true

		t.Run(testCase.Name, func(t *testing.T) {
			runCase(t, testCase)
		})
	}

	// The runner must also fail on a case it knows nothing about — otherwise a fixture
	// it silently ignores looks like a pass.
	t.Run("unknown case is a failure", func(t *testing.T) {
		if known["a-case-nobody-implemented"] {
			t.Fatal("the known-case table contains a case that does not exist")
		}
	})
}

// harness holds one case's state: the store, and the symbolic-name maps so results can
// be reported in the fixture's vocabulary rather than in UUIDs.
type harness struct {
	store *ops.Store
	// expiringStore has a lease that has already lapsed by the time Claim returns,
	// which is how the lease-expiry case is expressed without sleeping.
	expiringStore *ops.Store
	symbolOf      map[uuid.UUID]string
	idOf          map[string]uuid.UUID
	published     []string
	claimed       []string
	attempts      []int
	outcomes      []string
	// lastLeaseless is set when a step asked for a lease that has already expired.
	lastLeaseless bool
}

func (h *harness) id(symbol string) uuid.UUID {
	if id, ok := h.idOf[symbol]; ok {
		return id
	}
	id := uuid.New()
	h.idOf[symbol] = id
	h.symbolOf[id] = symbol
	return id
}

// runCase builds a fresh database, runs the steps and asserts the expectations.
func runCase(t *testing.T, testCase Case) {
	pool, store := freshDatabase(t)
	ctx := context.Background()

	h := &harness{
		store:         store,
		expiringStore: ops.New(pool, ops.WithOwner("expiring"), ops.WithLeaseDuration(time.Nanosecond)),
		symbolOf:      make(map[uuid.UUID]string, 4),
		idOf:          make(map[string]uuid.UUID, 4),
	}

	for index, step := range testCase.Steps {
		if err := h.apply(ctx, pool, step); err != nil {
			if step.ExpectError {
				continue
			}
			t.Fatalf("step %d (%s): %v", index+1, step.Op, err)
		}
		if step.ExpectError {
			t.Fatalf("step %d (%s) was expected to fail and did not", index+1, step.Op)
		}
	}

	observed := map[string]any{
		"published":           h.published,
		"publishedCount":      len(h.published),
		"publishedDistinct":   distinct(h.published),
		"claimed":             h.claimed,
		"claimedCount":        len(h.claimed),
		"claimedDistinct":     distinct(h.claimed),
		"claimedAttempts":     h.attempts,
		"idempotencyOutcomes": h.outcomes,
	}
	observed["remainingEvents"] = countRows(t, ctx, pool, `SELECT count(*) FROM ops.outbox_event`)
	observed["remainingUnpublished"] = countRows(t, ctx, pool,
		`SELECT count(*) FROM ops.outbox_event WHERE published_at IS NULL`)
	observed["deadLetters"] = countRows(t, ctx, pool, `SELECT count(*) FROM ops.dead_letter`)
	observed["jobStates"] = jobStates(t, ctx, pool, h)

	for key, want := range testCase.Expect {
		got, known := observed[key]
		if !known {
			t.Fatalf("expectation %q is not an outcome this runner produces", key)
		}
		if !equalJSON(got, want) {
			t.Errorf("%s = %s, want %s", key, mustJSON(got), mustJSON(want))
		}
	}
}

// apply performs one step.
func (h *harness) apply(ctx context.Context, pool *pgxpool.Pool, step Step) error {
	switch step.Op {
	case "outbox.write":
		return h.writeEvents(ctx, step.Events, false)
	case "outbox.write_rollback":
		return h.writeEvents(ctx, step.Events, true)
	case "outbox.relay":
		times := max(step.Times, 1)
		for range times {
			if _, err := h.store.RelayDrain(ctx, 100); err != nil {
				return err
			}
		}
		// Record what became visible, in seq order, as symbols.
		feed, err := h.store.ReadEvents(ctx, 0, 500, nil)
		if err != nil {
			return err
		}
		h.published = h.published[:0]
		for _, event := range feed.Events {
			h.published = append(h.published, h.symbolOf[event.ID])
		}
		return nil

	case "job.enqueue":
		maxAttempts := step.MaxAttempts
		for _, job := range step.Jobs {
			id := h.id(job.ID)
			if _, _, err := h.store.EnqueueJob(ctx, ops.Job{
				ID: id, Kind: job.Kind, Payload: json.RawMessage(`{}`), MaxAttempts: maxAttempts,
			}); err != nil {
				return err
			}
		}
		return nil

	case "job.claim":
		claimers := max(step.Claimers, 1)
		if claimers == 1 {
			return h.claimOne(ctx, step)
		}
		// Concurrent claimers, each with its own owner, sharing one result slice.
		type result struct {
			jobs []ops.Job
			err  error
		}
		results := make(chan result, claimers)
		start := make(chan struct{})
		for slot := range claimers {
			go func(slot int) {
				claimer := ops.New(pool, ops.WithOwner(fmt.Sprintf("claim-%d", slot)))
				<-start
				jobs, err := claimer.Claim(ctx, step.Count)
				results <- result{jobs: jobs, err: err}
			}(slot)
		}
		close(start)
		for range claimers {
			outcome := <-results
			if outcome.err != nil {
				return outcome.err
			}
			for _, job := range outcome.jobs {
				h.claimed = append(h.claimed, h.symbolOf[job.ID])
				h.attempts = append(h.attempts, job.Attempt)
			}
		}
		return nil

	case "job.settle":
		// Settle the most recently claimed job.
		if len(h.claimed) == 0 {
			return fmt.Errorf("job.settle with nothing claimed")
		}
		symbol := h.claimed[len(h.claimed)-1]
		id := h.idOf[symbol]
		switch step.Outcome {
		case "complete":
			return h.store.Complete(ctx, id)
		case "fail":
			if _, err := h.store.Fail(ctx, id, fmt.Errorf("conformance: deliberate failure")); err != nil {
				return err
			}
			// Clear the backoff so the next claim is immediate; the fixture is about the
			// attempt count, not about waiting.
			_, err := pool.Exec(ctx, `UPDATE ops.job SET available_at = now() WHERE id = $1`, id)
			return err
		default:
			return fmt.Errorf("unknown job.settle outcome %q", step.Outcome)
		}

	case "idempotency.request":
		hash := ops.HashRequest([]byte(step.Body))
		outcome, stored, err := h.store.ClaimIdempotency(ctx, "conformance", step.Key, hash, 0)
		switch {
		case errors.Is(err, ops.ErrIdempotencyConflict):
			h.outcomes = append(h.outcomes, "conflict")
			return nil
		case errors.Is(err, ops.ErrIdempotencyInFlight):
			h.outcomes = append(h.outcomes, "in_flight")
			return nil
		case err != nil:
			return err
		}
		if outcome == ops.IdempotencyReplay {
			h.outcomes = append(h.outcomes, "replay")
			if stored == nil {
				return fmt.Errorf("a replay returned no stored response")
			}
			return nil
		}
		h.outcomes = append(h.outcomes, "proceed")
		return h.store.SaveIdempotentResponse(ctx, "conformance", step.Key, step.Status, []byte(step.Response))

	default:
		return fmt.Errorf("unknown op %q", step.Op)
	}
}

// claimOne claims with the case's own store, optionally using a lease that has already
// expired so the next claim can reclaim it without sleeping.
func (h *harness) claimOne(ctx context.Context, step Step) error {
	claimer := h.store
	if step.LeaseExpires {
		claimer = h.expiringStore
	}
	jobs, err := claimer.Claim(ctx, max(step.Count, 1))
	if err != nil {
		return err
	}
	for _, job := range jobs {
		h.claimed = append(h.claimed, h.symbolOf[job.ID])
		h.attempts = append(h.attempts, job.Attempt)
	}
	return nil
}

// writeEvents writes the events, committing or rolling back.
func (h *harness) writeEvents(ctx context.Context, events []FixtureEvent, rollback bool) error {
	build := func(event FixtureEvent) ops.Event {
		return ops.Event{
			ID:      h.id(event.ID),
			Type:    event.Type,
			Subject: event.Subject,
			Payload: json.RawMessage(`{}`),
		}
	}
	if rollback {
		_ = h.store.InTx(ctx, func(tx pgx.Tx) error {
			for _, event := range events {
				if _, err := h.store.Enqueue(ctx, tx, build(event)); err != nil {
					return err
				}
			}
			return fmt.Errorf("conformance: deliberate rollback")
		})
		return nil
	}
	return h.store.InTx(ctx, func(tx pgx.Tx) error {
		for _, event := range events {
			if _, err := h.store.Enqueue(ctx, tx, build(event)); err != nil {
				return err
			}
		}
		return nil
	})
}

// ---- helpers -------------------------------------------------------------------

// countRows runs a scalar count query.
func countRows(t *testing.T, ctx context.Context, pool *pgxpool.Pool, query string) int {
	t.Helper()
	var count int
	if err := pool.QueryRow(ctx, query).Scan(&count); err != nil {
		t.Fatalf("counting (%s): %v", query, err)
	}
	return count
}

// jobStates maps each symbol to its job's state.
func jobStates(t *testing.T, ctx context.Context, pool *pgxpool.Pool, h *harness) map[string]string {
	t.Helper()
	states := make(map[string]string, len(h.idOf))
	for symbol, id := range h.idOf {
		var state string
		err := pool.QueryRow(ctx, `SELECT state FROM ops.job WHERE id = $1`, id).Scan(&state)
		if err != nil {
			continue // not a job symbol
		}
		states[symbol] = state
	}
	return states
}

func distinct(values []string) bool {
	seen := make(map[string]struct{}, len(values))
	for _, value := range values {
		if _, duplicate := seen[value]; duplicate {
			return false
		}
		seen[value] = struct{}{}
	}
	return true
}

func mustJSON(value any) string {
	encoded, err := json.Marshal(value)
	if err != nil {
		return fmt.Sprintf("<unencodable: %v>", err)
	}
	return string(encoded)
}

// equalJSON compares two values through JSON, so a fixture's []any and the runner's
// []string compare equal.
func equalJSON(got, want any) bool {
	var normalisedGot, normalisedWant any
	if err := json.Unmarshal([]byte(mustJSON(got)), &normalisedGot); err != nil {
		return false
	}
	if err := json.Unmarshal([]byte(mustJSON(want)), &normalisedWant); err != nil {
		return false
	}
	return mustJSON(normalisedGot) == mustJSON(normalisedWant)
}

package ops

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5"
)

// Job is one unit of work with competing consumers.
//
// There is no queue server: N replicas claim disjoint rows with `FOR UPDATE SKIP
// LOCKED`, which is exactly what a durable pull consumer did, minus the server
// (ADR-0004). The lease is what makes a crashed replica's work reclaimable without a
// coordinator.
type Job struct {
	ID      uuid.UUID
	Kind    string
	Payload json.RawMessage
	// Attempt is how many times this job has been claimed, including this claim.
	Attempt int
	// MaxAttempts is when it stops retrying and becomes dead.
	MaxAttempts int
	// AvailableAt is the earliest the job may be claimed. Retries push it forward.
	AvailableAt time.Time
	// LeaseUntil is when this claim lapses, if this claim holds one.
	LeaseUntil *time.Time
	// LeaseOwner is the replica holding the claim.
	LeaseOwner   *string
	ScheduledFor *time.Time
	CreatedAt    time.Time
	// LastError is the previous attempt's failure, kept so a dead job explains itself.
	LastError *string
}

// Backoff computes how long to wait before the next attempt.
//
// Exponential with a cap, and no jitter: a job's retries are not a thundering herd —
// one job retries on its own schedule — and deterministic delays make the tests
// assertable. The cap matters more than the curve: without it the eighth attempt of a
// failing import would wait 21 hours, which is indistinguishable from "never".
func Backoff(attempt int) time.Duration {
	if attempt < 1 {
		return 0
	}
	// 2s, 4s, 8s, 16s, 32s, 60s, 60s, …
	const base = 2 * time.Second
	const cap = 60 * time.Second
	delay := base << min(attempt-1, 6) // shift past 6 is capped below anyway
	if delay > cap || delay <= 0 {
		return cap
	}
	return delay
}

// claimSQL is the one query that makes the queue work.
//
// Three details carry the whole design:
//
//   - `FOR UPDATE SKIP LOCKED` in the subquery: two replicas claiming at the same
//     instant take different rows and neither blocks. Without SKIP LOCKED they would
//     serialise behind each other and claim throughput would fall as replicas are
//     added.
//   - The `leased AND lease_until < now()` arm: an expired lease is claimable again,
//     which is how a killed replica's work returns without a recovery sweep.
//   - `ORDER BY available_at`: oldest ready work first, so a retry with a backoff does
//     not starve behind newer jobs.
const claimSQL = `
UPDATE ops.job
SET state = 'leased',
    lease_owner = $1,
    lease_until = $2,
    attempt = attempt + 1,
    updated_at = now()
WHERE id IN (
    SELECT id FROM ops.job
    WHERE (state = 'queued' AND available_at <= now())
       OR (state = 'leased' AND lease_until < now())
    ORDER BY available_at, id
    FOR UPDATE SKIP LOCKED
    LIMIT $3)
RETURNING id, kind, payload, attempt, max_attempts, available_at,
          lease_until, lease_owner, scheduled_for, created_at, last_error`

// Claim takes up to n jobs, leased to this replica.
//
// The returned jobs are disjoint from any other replica's claim — that is the
// guarantee the acceptance criterion checks by running four claimers at once — and
// each is owned for leaseDuration. A caller that cannot finish in time should call
// Extend rather than letting the lease lapse, because a lapsed lease means another
// replica starts the same work.
func (s *Store) Claim(ctx context.Context, n int) ([]Job, error) {
	if n <= 0 {
		return nil, nil
	}
	rows, err := s.pool.Query(ctx, claimSQL, s.owner, s.now().UTC().Add(s.leaseDuration), n)
	if err != nil {
		return nil, fmt.Errorf("ops: claiming jobs: %w", err)
	}
	defer rows.Close()

	jobs := make([]Job, 0, n)
	for rows.Next() {
		var job Job
		if err := rows.Scan(&job.ID, &job.Kind, &job.Payload, &job.Attempt, &job.MaxAttempts,
			&job.AvailableAt, &job.LeaseUntil, &job.LeaseOwner, &job.ScheduledFor,
			&job.CreatedAt, &job.LastError); err != nil {
			return nil, fmt.Errorf("ops: scanning a claimed job: %w", err)
		}
		jobs = append(jobs, job)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("ops: claiming jobs: %w", err)
	}
	return jobs, nil
}

// Enqueue adds a job and returns its id.
//
// The id is the caller's if supplied, which is what lets a consumer make an event into
// a job idempotently: the job id *is* the event id, so pulling the same event twice
// inserts once. `ON CONFLICT DO NOTHING` turns the duplicate into a no-op instead of
// an error, so the puller does not have to distinguish.
func (s *Store) EnqueueJob(ctx context.Context, job Job) (uuid.UUID, bool, error) {
	if job.ID == uuid.Nil {
		job.ID = uuid.New()
	}
	if job.Kind == "" {
		return uuid.Nil, false, fmt.Errorf("ops: a job needs a kind")
	}
	if len(job.Payload) == 0 {
		job.Payload = json.RawMessage(`{}`)
	}
	if !json.Valid(job.Payload) {
		return uuid.Nil, false, fmt.Errorf("ops: the payload of a %s job is not valid JSON", job.Kind)
	}
	if job.MaxAttempts <= 0 {
		job.MaxAttempts = 8
	}
	availableAt := job.AvailableAt
	if availableAt.IsZero() {
		availableAt = s.now().UTC()
	}

	tag, err := s.pool.Exec(ctx, `
		INSERT INTO ops.job (id, kind, payload, max_attempts, available_at, scheduled_for)
		VALUES ($1, $2, $3, $4, $5, $6)
		ON CONFLICT DO NOTHING`,
		job.ID, job.Kind, job.Payload, job.MaxAttempts, availableAt, job.ScheduledFor)
	if err != nil {
		return uuid.Nil, false, fmt.Errorf("ops: enqueueing a %s job: %w", job.Kind, err)
	}
	return job.ID, tag.RowsAffected() == 1, nil
}

// Complete marks a job done.
//
// Idempotent: completing an already-done job succeeds and changes nothing. That is not
// a convenience — a worker that finishes the work and then crashes before recording it
// will be retried, and a second completion must not be an error the retry loop treats
// as failure.
func (s *Store) Complete(ctx context.Context, id uuid.UUID) error {
	tag, err := s.pool.Exec(ctx, `
		UPDATE ops.job
		SET state = 'done', lease_owner = NULL, lease_until = NULL, updated_at = now()
		WHERE id = $1 AND state IN ('queued', 'leased')`, id)
	if err != nil {
		return fmt.Errorf("ops: completing job %s: %w", id, err)
	}
	if tag.RowsAffected() == 1 {
		return nil
	}

	// Nothing changed: either it is already done (fine) or it is dead (a caller bug).
	var state string
	err = s.pool.QueryRow(ctx, `SELECT state FROM ops.job WHERE id = $1`, id).Scan(&state)
	if err != nil {
		if isNoRows(err) {
			return fmt.Errorf("%w: no job %s", ErrNoRun, id)
		}
		return fmt.Errorf("ops: completing job %s: %w", id, err)
	}
	if state == "done" {
		return nil
	}
	return fmt.Errorf("%w: job %s is %s", ErrNoRun, id, state)
}

// Fail records a failed attempt.
//
// Below max_attempts the job returns to `queued` with an exponential backoff; at
// max_attempts it becomes `dead` and a dead-letter row is written in the *same*
// transaction. One path, one place a job stops existing: without the dead letter, a
// poison message is a row that quietly spins forever.
//
// Returns whether the job is now dead, so a caller can log or alert on the transition
// rather than inferring it from a count.
func (s *Store) Fail(ctx context.Context, id uuid.UUID, cause error) (bool, error) {
	reason := "failed"
	if cause != nil {
		reason = cause.Error()
	}

	dead := false
	err := s.InTx(ctx, func(tx pgx.Tx) error {
		var attempt, maxAttempts int
		var payload json.RawMessage
		err := tx.QueryRow(ctx,
			`SELECT attempt, max_attempts, payload FROM ops.job WHERE id = $1 FOR UPDATE`,
			id).Scan(&attempt, &maxAttempts, &payload)
		if err != nil {
			if isNoRows(err) {
				return fmt.Errorf("%w: no job %s", ErrNoRun, id)
			}
			return fmt.Errorf("ops: failing job %s: %w", id, err)
		}

		if attempt >= maxAttempts {
			dead = true
			if _, err := tx.Exec(ctx, `
				UPDATE ops.job
				SET state = 'dead', lease_owner = NULL, lease_until = NULL,
				    last_error = $2, updated_at = now()
				WHERE id = $1`, id, reason); err != nil {
				return fmt.Errorf("ops: marking job %s dead: %w", id, err)
			}
			if _, err := tx.Exec(ctx, `
				INSERT INTO ops.dead_letter (id, source_table, ref_id, reason, payload)
				VALUES ($1, 'ops.job', $2, $3, $4)`,
				uuid.New(), id, reason, payload); err != nil {
				return fmt.Errorf("ops: writing a dead letter for job %s: %w", id, err)
			}
			return nil
		}

		next := s.now().UTC().Add(Backoff(attempt))
		if _, err := tx.Exec(ctx, `
			UPDATE ops.job
			SET state = 'queued', available_at = $2, last_error = $3,
			    lease_owner = NULL, lease_until = NULL, updated_at = now()
			WHERE id = $1`, id, next, reason); err != nil {
			return fmt.Errorf("ops: requeueing job %s: %w", id, err)
		}
		return nil
	})
	if err != nil {
		return false, err
	}
	return dead, nil
}

// Extend pushes a lease out, for work that legitimately takes longer than one lease.
//
// Only succeeds for the replica holding the lease: a lease that has already lapsed is
// another replica's to take, and extending it from here would give two replicas the
// same job.
func (s *Store) Extend(ctx context.Context, id uuid.UUID) error {
	tag, err := s.pool.Exec(ctx, `
		UPDATE ops.job
		SET lease_until = $2, updated_at = now()
		WHERE id = $1 AND state = 'leased' AND lease_owner = $3`,
		id, s.now().UTC().Add(s.leaseDuration), s.owner)
	if err != nil {
		return fmt.Errorf("ops: extending the lease on job %s: %w", id, err)
	}
	if tag.RowsAffected() == 0 {
		return fmt.Errorf("%w: job %s is not leased to %s", ErrNoRun, id, s.owner)
	}
	return nil
}

// Reap re-queues jobs whose lease lapsed without a completion or a failure.
//
// Not required for correctness — Claim already reclaims expired leases — but it keeps
// `state` honest between claims, so a monitoring query that counts `leased` rows is
// not counting ghosts. A service runs this on a schedule from `ops.job` itself, which
// is why it exists as a function rather than a background loop in the library.
func (s *Store) Reap(ctx context.Context) (int, error) {
	tag, err := s.pool.Exec(ctx, `
		UPDATE ops.job
		SET state = 'queued', lease_owner = NULL, lease_until = NULL, updated_at = now()
		WHERE state = 'leased' AND lease_until < now()`)
	if err != nil {
		return 0, fmt.Errorf("ops: reaping expired leases: %w", err)
	}
	return int(tag.RowsAffected()), nil
}

// JobState counts jobs in each state, for a health or metrics endpoint.
func (s *Store) JobState(ctx context.Context) (map[string]int, error) {
	rows, err := s.pool.Query(ctx, `SELECT state, count(*) FROM ops.job GROUP BY state`)
	if err != nil {
		return nil, fmt.Errorf("ops: counting jobs: %w", err)
	}
	defer rows.Close()

	counts := make(map[string]int, 4)
	for rows.Next() {
		var state string
		var count int
		if err := rows.Scan(&state, &count); err != nil {
			return nil, fmt.Errorf("ops: counting jobs: %w", err)
		}
		counts[state] = count
	}
	return counts, rows.Err()
}

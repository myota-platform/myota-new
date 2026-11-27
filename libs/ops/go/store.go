// Package ops implements the operational pattern every service shares: a
// transactional outbox, a partition-leased relay, a job queue with competing
// consumers, request-level idempotency, and dead letters.
//
// There is no message broker (ADR-0004). Each service writes its events to its own
// `ops.outbox_event` inside the domain transaction, and peers pull them through
// `GET /internal/events`. Ordering is per aggregate, which is what partition
// assignment is for.
//
// This package exists twice — libs/ops/go and libs/ops/rust — because a Go service
// cannot link a Rust crate (ADR-0002). The two must agree on behaviour, and
// libs/ops/conformance/ is the suite that holds them to it. The DDL is *not*
// duplicated: libs/ops/sql/ is written once and copied into both modules, so the two
// implementations cannot be applying different schemas.
package ops

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// Partitions is how many partitions the event log is divided into. It is a constant
// rather than a setting: changing it rewrites the assignment of every existing
// subject, and a consumer mid-stream would see each aggregate jump partitions. Raise
// it in a migration, with the resync that implies — never by configuration.
const Partitions = 32

// Errors a caller has to distinguish from a generic failure.
var (
	// ErrNoRun means a job is not in a state that permits the operation — completing
	// something already dead, for instance. It is a caller bug, not a retryable one.
	ErrNoRun = errors.New("ops: the row is not in a state that permits this operation")
	// ErrSchemaBehind means the database has not had the migrations this binary needs.
	// A replica must refuse to start rather than serve queries against a schema it
	// does not understand.
	ErrSchemaBehind = errors.New("ops: the database schema is behind this binary")
	// ErrSchemaAhead means the database has migrations this binary does not know
	// about — an older binary pointed at a newer database, which during a rollback is
	// exactly the situation that corrupts data if it proceeds.
	ErrSchemaAhead = errors.New("ops: the database schema is ahead of this binary")
	// ErrIdempotencyConflict means a key was reused with a different request body.
	ErrIdempotencyConflict = errors.New("ops: idempotency key reused with a different body")
	// ErrIdempotencyInFlight means the same key is being processed right now by
	// another request. The caller waits or returns a retryable failure.
	ErrIdempotencyInFlight = errors.New("ops: a request with this idempotency key is in flight")
)

// Store is the handle a service holds. It is safe for concurrent use: it wraps a
// pool and keeps no per-request state, which is what lets a service run as N
// replicas (ADR-0005).
type Store struct {
	pool *pgxpool.Pool
	// owner identifies this replica in leases. Two pods must never share one, so it
	// defaults to a fresh value per process.
	owner string
	// leaseDuration is how long a claim or partition lease is valid. A crashed
	// replica's work becomes reclaimable when it expires.
	leaseDuration time.Duration
	// now is injectable so lease expiry can be tested without sleeping.
	now func() time.Time
}

// Option configures a Store.
type Option func(*Store)

// WithOwner sets the lease owner name. Defaults to a random per-process value; set
// it to the pod name so a stuck lease names the replica holding it.
func WithOwner(owner string) Option {
	return func(s *Store) { s.owner = owner }
}

// WithLeaseDuration sets how long a job claim or partition lease is valid.
func WithLeaseDuration(d time.Duration) Option {
	return func(s *Store) { s.leaseDuration = d }
}

// WithClock replaces the time source, for tests.
func WithClock(now func() time.Time) Option {
	return func(s *Store) { s.now = now }
}

// New wraps a pool. The pool is supplied by the service, not opened here: connection
// lifecycle, PgBouncer settings and pool size are deployment decisions.
func New(pool *pgxpool.Pool, options ...Option) *Store {
	store := &Store{
		pool: pool,
		// A random owner per process. Two replicas must never share an owner, or one
		// could extend the other's leases.
		owner:         "ops-" + uuid.NewString()[:8],
		leaseDuration: 5 * time.Minute,
		now:           time.Now,
	}
	for _, option := range options {
		option(store)
	}
	return store
}

// isNoRows reports whether an error is "no rows", which several callers treat as an
// expected outcome rather than a failure.
func isNoRows(err error) bool {
	return errors.Is(err, pgx.ErrNoRows)
}

// Owner returns the lease owner this store identifies itself by.
func (s *Store) Owner() string { return s.owner }

// Pool exposes the underlying pool for the rare caller that needs a transaction of
// its own — a domain write that also enqueues an event, which is the normal case.
func (s *Store) Pool() *pgxpool.Pool { return s.pool }

// InTx runs fn inside a transaction, rolling back on error.
//
// Provided because the outbox's central promise is that the event and the domain
// change commit together, and the only way to make that hard to get wrong is to give
// callers one obvious way to write both.
func (s *Store) InTx(ctx context.Context, fn func(pgx.Tx) error) error {
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return fmt.Errorf("ops: beginning a transaction: %w", err)
	}
	// Rollback after a successful commit is a no-op, so this is safe on every path.
	defer func() { _ = tx.Rollback(ctx) }()

	if err := fn(tx); err != nil {
		return err
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("ops: committing: %w", err)
	}
	return nil
}

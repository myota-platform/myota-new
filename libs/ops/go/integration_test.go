//go:build integration

// Integration tests for libs/ops. They need a real PostgreSQL and are behind the
// `integration` build tag, so `go test ./...` on a machine without a database still
// passes instead of silently skipping inside a green run.
//
// The database comes from OPS_TEST_DATABASE_URL. `make -C libs/ops/go test` starts one
// with Docker when the variable is unset; CI does the same. See the README.
//
//	OPS_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres \
//	  go test -tags integration ./...

package ops_test

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/myota-platform/myota-new/libs/ops"
)

// testDatabaseURL returns the DSN or skips the test.
func testDatabaseURL(t *testing.T) string {
	t.Helper()
	url := os.Getenv("OPS_TEST_DATABASE_URL")
	if url == "" {
		t.Skip("OPS_TEST_DATABASE_URL is not set; integration tests need a PostgreSQL")
	}
	return url
}

// configFor returns a pool config pointing at the named database.
//
// It returns the config rather than a DSN on purpose: `pgxpool.Config.ConnString()` is
// the *original* string and does not reflect a change to `ConnConfig.Database`, so
// round-tripping through it silently connects to the admin database instead. That is a
// mistake worth not having a helper for.
func configFor(t *testing.T, name string) *pgxpool.Config {
	t.Helper()
	config, err := pgxpool.ParseConfig(testDatabaseURL(t))
	if err != nil {
		t.Fatalf("parsing the test DSN: %v", err)
	}
	config.ConnConfig.Database = name
	return config
}

// createDatabase makes a fresh database and drops it when the test ends.
//
// A database per test is the cheapest isolation PostgreSQL offers and it makes every
// assertion absolute rather than relative to whatever ran before.
func createDatabase(t *testing.T) string {
	t.Helper()
	adminURL := testDatabaseURL(t)
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	admin, err := pgxpool.New(ctx, adminURL)
	if err != nil {
		t.Fatalf("connecting to the test server: %v", err)
	}
	defer admin.Close()

	name := "ops_test_" + uuid.NewString()[:8]
	if _, err := admin.Exec(ctx, "CREATE DATABASE "+name); err != nil {
		t.Fatalf("creating a test database: %v", err)
	}
	t.Cleanup(func() {
		cleanup, err := pgxpool.New(context.Background(), adminURL)
		if err != nil {
			return
		}
		defer cleanup.Close()
		_, _ = cleanup.Exec(context.Background(), "DROP DATABASE IF EXISTS "+name+" WITH (FORCE)")
	})
	return name
}

// freshDatabase returns a pool and a store against a migrated, empty database.
func freshDatabase(t *testing.T) (*pgxpool.Pool, *ops.Store) {
	t.Helper()
	config := configFor(t, createDatabase(t))
	ctx := context.Background()

	pool, err := pgxpool.NewWithConfig(ctx, config)
	if err != nil {
		t.Fatalf("connecting: %v", err)
	}
	t.Cleanup(pool.Close)

	if _, err := ops.Migrate(ctx, pool); err != nil {
		t.Fatalf("migrating: %v", err)
	}
	return pool, ops.New(pool)
}

// enqueue writes one event in its own transaction.
func enqueue(t *testing.T, store *ops.Store, subject, eventType string) ops.Enqueued {
	t.Helper()
	ctx := context.Background()
	var stored ops.Enqueued
	if err := store.InTx(ctx, func(tx pgx.Tx) error {
		var err error
		stored, err = store.Enqueue(ctx, tx, ops.Event{
			ID:      uuid.New(),
			Type:    eventType,
			Subject: subject,
			Payload: json.RawMessage(`{"n":1}`),
		})
		return err
	}); err != nil {
		t.Fatalf("enqueueing an event: %v", err)
	}
	return stored
}

// TestMigrationsAreIdempotent is acceptance criterion 3.
func TestMigrationsAreIdempotent(t *testing.T) {
	pool, _ := freshDatabase(t) // migrates once
	ctx := context.Background()

	applied, err := ops.Migrate(ctx, pool)
	if err != nil {
		t.Fatalf("re-running migrations: %v", err)
	}
	if applied != 0 {
		t.Errorf("re-running applied %d migration(s), want 0", applied)
	}

	// A third time: "idempotent" that works twice is not a property.
	if applied, err = ops.Migrate(ctx, pool); err != nil || applied != 0 {
		t.Errorf("third run: applied %d, err %v; want 0, nil", applied, err)
	}

	var count int
	if err := pool.QueryRow(ctx, `SELECT count(*) FROM ops.schema_migration`).Scan(&count); err != nil {
		t.Fatalf("counting the ledger: %v", err)
	}
	migrations, err := ops.Migrations()
	if err != nil {
		t.Fatalf("reading the embedded migrations: %v", err)
	}
	if count != len(migrations) {
		t.Errorf("ledger has %d rows for %d migration(s)", count, len(migrations))
	}

	if err := ops.VerifySchema(ctx, pool); err != nil {
		t.Errorf("VerifySchema after migrating: %v", err)
	}
}

// TestVerifySchemaRefusesAnUnmigratedDatabase pins the other half of the migration
// contract: a replica must refuse to start against a database nobody migrated.
func TestVerifySchemaRefusesAnUnmigratedDatabase(t *testing.T) {
	config := configFor(t, createDatabase(t))
	ctx := context.Background()

	pool, err := pgxpool.NewWithConfig(ctx, config)
	if err != nil {
		t.Fatalf("connecting: %v", err)
	}
	defer pool.Close()

	if err := ops.VerifySchema(ctx, pool); err == nil {
		t.Fatal("VerifySchema accepted a database with no migration ledger")
	}
}

// TestMigrateRefusesADatabaseFromTheFuture covers the rollback hazard: an older binary
// pointed at a newer schema must refuse rather than write rows the schema no longer
// expects.
func TestMigrateRefusesADatabaseFromTheFuture(t *testing.T) {
	pool, _ := freshDatabase(t)
	ctx := context.Background()

	if _, err := pool.Exec(ctx,
		`INSERT INTO ops.schema_migration (version, name, checksum) VALUES (9999, 'from_the_future', 'x')`,
	); err != nil {
		t.Fatalf("inserting a future migration: %v", err)
	}

	if _, err := ops.Migrate(ctx, pool); err == nil {
		t.Fatal("Migrate accepted a database ahead of the binary")
	}
}

// TestMigrateRefusesAnEditedMigration covers the other half: DDL that changed after it
// was applied. The database and the file disagree, and a startup cannot decide which is
// right.
func TestMigrateRefusesAnEditedMigration(t *testing.T) {
	pool, _ := freshDatabase(t)
	ctx := context.Background()

	if _, err := pool.Exec(ctx,
		`UPDATE ops.schema_migration SET checksum = 'edited' WHERE version = (SELECT min(version) FROM ops.schema_migration)`,
	); err != nil {
		t.Fatalf("editing the ledger: %v", err)
	}

	if _, err := ops.Migrate(ctx, pool); err == nil {
		t.Fatal("Migrate accepted an edited migration")
	}
}

// TestConcurrentRelaysPublishEachEventOnce is acceptance criterion 1.
//
// Four relays run at once over the same 32 partitions. Every event must be published
// exactly once.
func TestConcurrentRelaysPublishEachEventOnce(t *testing.T) {
	pool, store := freshDatabase(t)
	ctx := context.Background()

	const events = 400
	ids := make(map[uuid.UUID]struct{}, events)
	for i := range events {
		ids[enqueue(t, store, fmt.Sprintf("entity:%d", i), "geodata.entity.published.v1").ID] = struct{}{}
	}

	const relayers = 4
	var (
		mutex     sync.Mutex
		batches   = make([][]ops.Enqueued, relayers)
		published atomic.Int64
		waiting   sync.WaitGroup
		failures  = make([]error, relayers)
	)

	start := make(chan struct{})
	for slot := range relayers {
		waiting.Add(1)
		go func(slot int) {
			defer waiting.Done()
			// A distinct owner per relay, so no two can share a lease by identity.
			relay := ops.New(pool, ops.WithOwner(fmt.Sprintf("relay-%d", slot)))
			<-start

			idle := 0
			for published.Load() < events && idle < 500 {
				lease, err := relay.ClaimPartition(ctx)
				if err != nil {
					failures[slot] = err
					return
				}
				if lease == nil {
					// Every partition is leased. Normal contention: back off and
					// re-check the shared count.
					idle++
					time.Sleep(time.Millisecond)
					continue
				}

				batch, err := relay.PublishPartition(ctx, lease.Partition, 7)
				if err != nil {
					failures[slot] = err
					_ = relay.ReleasePartition(ctx, lease.Partition)
					return
				}
				if len(batch.Published) > 0 {
					idle = 0
					published.Add(int64(len(batch.Published)))
					mutex.Lock()
					batches[slot] = append(batches[slot], batch.Published...)
					mutex.Unlock()
				} else {
					idle++
				}
				_ = relay.ReleasePartition(ctx, lease.Partition)
			}
		}(slot)
	}
	close(start)

	finished := make(chan struct{})
	go func() { waiting.Wait(); close(finished) }()
	select {
	case <-finished:
	case <-time.After(60 * time.Second):
		t.Fatal("relays did not finish within 60s")
	}

	for slot, err := range failures {
		if err != nil {
			t.Fatalf("relay %d failed: %v", slot, err)
		}
	}

	seen := make(map[uuid.UUID]int, events)
	total := 0
	for _, batch := range batches {
		for _, event := range batch {
			seen[event.ID]++
			total++
		}
	}

	if total != events {
		t.Errorf("published %d events in total, want %d", total, events)
	}
	for id := range ids {
		switch seen[id] {
		case 0:
			t.Errorf("event %s was never published", id)
		case 1:
		default:
			t.Errorf("event %s was published %d times, want 1", id, seen[id])
		}
	}
	for id := range seen {
		if _, expected := ids[id]; !expected {
			t.Errorf("published an event that was never enqueued: %s", id)
		}
	}

	var unpublished int
	if err := pool.QueryRow(ctx,
		`SELECT count(*) FROM ops.outbox_event WHERE published_at IS NULL`).Scan(&unpublished); err != nil {
		t.Fatalf("counting unpublished rows: %v", err)
	}
	if unpublished != 0 {
		t.Errorf("%d rows are still unpublished", unpublished)
	}
}

// TestRelayPreservesOrderPerPartition checks the property the lease exists for.
func TestRelayPreservesOrderPerPartition(t *testing.T) {
	_, store := freshDatabase(t)
	ctx := context.Background()

	const events = 60
	subjects := []string{"entity:aaa", "entity:bbb", "entity:ccc"}
	for i := range events {
		enqueue(t, store, subjects[i%len(subjects)], "geodata.entity.updated.v1")
	}

	if _, err := store.RelayDrain(ctx, 5); err != nil {
		t.Fatalf("draining: %v", err)
	}

	feed, err := store.ReadEvents(ctx, 0, 500, nil)
	if err != nil {
		t.Fatalf("reading the feed: %v", err)
	}
	if len(feed.Events) != events {
		t.Fatalf("feed has %d events, want %d", len(feed.Events), events)
	}

	// The feed is globally ordered by seq, so an ascending seq within each subject is
	// the observable form of "the relay published in order".
	last := make(map[string]int64, len(subjects))
	for _, event := range feed.Events {
		if previous, seen := last[event.Subject]; seen && event.Seq <= previous {
			t.Errorf("subject %s: seq %d came after %d", event.Subject, event.Seq, previous)
		}
		last[event.Subject] = event.Seq
	}
}

// TestEnqueueRollsBackWithTheTransaction pins the central promise: an event for a
// rolled-back transaction must never exist.
func TestEnqueueRollsBackWithTheTransaction(t *testing.T) {
	pool, store := freshDatabase(t)
	ctx := context.Background()

	err := store.InTx(ctx, func(tx pgx.Tx) error {
		if _, err := store.Enqueue(ctx, tx, ops.Event{
			ID:      uuid.New(),
			Type:    "core.account.created.v1",
			Subject: "account:rollback",
			Payload: json.RawMessage(`{}`),
		}); err != nil {
			return err
		}
		return fmt.Errorf("domain write failed")
	})
	if err == nil {
		t.Fatal("the transaction reported success")
	}

	var count int
	if err := pool.QueryRow(ctx, `SELECT count(*) FROM ops.outbox_event`).Scan(&count); err != nil {
		t.Fatalf("counting rows: %v", err)
	}
	if count != 0 {
		t.Errorf("%d event(s) survived a rolled-back transaction", count)
	}
}

// TestDuplicateEventIdWritesOneRow: the event id is a natural key, so a producer retry
// cannot create two events that a consumer would turn into two jobs.
func TestDuplicateEventIdWritesOneRow(t *testing.T) {
	pool, store := freshDatabase(t)
	ctx := context.Background()
	event := ops.Event{
		ID:      uuid.New(),
		Type:    "core.account.created.v1",
		Subject: "account:dupe",
		Payload: json.RawMessage(`{}`),
	}

	if err := store.InTx(ctx, func(tx pgx.Tx) error {
		_, err := store.Enqueue(ctx, tx, event)
		return err
	}); err != nil {
		t.Fatalf("first enqueue: %v", err)
	}

	if err := store.InTx(ctx, func(tx pgx.Tx) error {
		_, err := store.Enqueue(ctx, tx, event)
		return err
	}); err == nil {
		t.Fatal("a duplicate event id was accepted")
	}

	var count int
	if err := pool.QueryRow(ctx, `SELECT count(*) FROM ops.outbox_event`).Scan(&count); err != nil {
		t.Fatalf("counting rows: %v", err)
	}
	if count != 1 {
		t.Errorf("%d rows for one event id, want 1", count)
	}
}

// TestEventRejectsBadInput keeps validation at the boundary, where the message can name
// the field instead of surfacing as a driver error.
func TestEventRejectsBadInput(t *testing.T) {
	_, store := freshDatabase(t)
	ctx := context.Background()

	cases := map[string]ops.Event{
		"no id":      {Type: "a.b.v1", Subject: "account:x", Payload: json.RawMessage(`{}`)},
		"no type":    {ID: uuid.New(), Subject: "account:x", Payload: json.RawMessage(`{}`)},
		"no subject": {ID: uuid.New(), Type: "a.b.v1", Payload: json.RawMessage(`{}`)},
		"bad json":   {ID: uuid.New(), Type: "a.b.v1", Subject: "account:x", Payload: json.RawMessage(`{`)},
	}
	for name, event := range cases {
		t.Run(name, func(t *testing.T) {
			err := store.InTx(ctx, func(tx pgx.Tx) error {
				_, err := store.Enqueue(ctx, tx, event)
				return err
			})
			if err == nil {
				t.Error("an invalid event was accepted")
			}
		})
	}
}

// TestPartitionIsStableAndThirtyTwo pins the two facts ordering depends on.
func TestPartitionIsStableAndThirtyTwo(t *testing.T) {
	_, store := freshDatabase(t)
	stored := enqueue(t, store, "entity:8f3c1e00-0000-4000-8000-000000000001", "geodata.entity.published.v1")

	expected, err := ops.Partition(stored.Subject, ops.Partitions)
	if err != nil {
		t.Fatalf("Partition: %v", err)
	}
	if stored.Partition != expected {
		t.Errorf("stored partition %d, want %d", stored.Partition, expected)
	}
	if ops.Partitions != 32 {
		t.Errorf("Partitions = %d, want 32 per the architecture", ops.Partitions)
	}
}

// TestFeedServesOnlyPublishedEvents is why the relay exists: a consumer must not see an
// event before the relay has made it visible.
func TestFeedServesOnlyPublishedEvents(t *testing.T) {
	_, store := freshDatabase(t)
	ctx := context.Background()

	enqueue(t, store, "entity:one", "geodata.entity.published.v1")

	feed, err := store.ReadEvents(ctx, 0, 100, nil)
	if err != nil {
		t.Fatalf("reading the feed: %v", err)
	}
	if len(feed.Events) != 0 {
		t.Fatalf("the feed served %d unpublished event(s)", len(feed.Events))
	}

	if _, err := store.RelayDrain(ctx, 100); err != nil {
		t.Fatalf("draining: %v", err)
	}

	feed, err = store.ReadEvents(ctx, 0, 100, nil)
	if err != nil {
		t.Fatalf("reading the feed: %v", err)
	}
	if len(feed.Events) != 1 {
		t.Errorf("the feed served %d events after a drain, want 1", len(feed.Events))
	}
}

// TestFeedFilterAndCursor covers the two things a consumer relies on: a type filter and
// a cursor that does not rewind.
func TestFeedFilterAndCursor(t *testing.T) {
	_, store := freshDatabase(t)
	ctx := context.Background()

	enqueue(t, store, "entity:a", "geodata.entity.published.v1")
	enqueue(t, store, "account:b", "core.account.created.v1")
	enqueue(t, store, "entity:c", "geodata.entity.published.v1")

	if _, err := store.RelayDrain(ctx, 100); err != nil {
		t.Fatalf("draining: %v", err)
	}

	filtered, err := store.ReadEvents(ctx, 0, 100, []string{"geodata.entity.published.v1"})
	if err != nil {
		t.Fatalf("reading the feed: %v", err)
	}
	if len(filtered.Events) != 2 {
		t.Fatalf("filtered feed has %d events, want 2", len(filtered.Events))
	}

	// Cursor: after the first event, only the second remains.
	after, err := store.ReadEvents(ctx, filtered.Events[0].Seq, 100, nil)
	if err != nil {
		t.Fatalf("reading past a cursor: %v", err)
	}
	if len(after.Events) != 2 {
		t.Errorf("cursor at seq %d returned %d events, want 2", filtered.Events[0].Seq, len(after.Events))
	}
	if after.Events[0].Seq <= filtered.Events[0].Seq {
		t.Error("the cursor returned an event at or before the cursor")
	}
}

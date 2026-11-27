package ops

import (
	"context"
	"crypto/sha256"
	"embed"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"sort"
	"strconv"
	"strings"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// migrationFiles holds the `ops` DDL. The files are copies of libs/ops/sql/, made by
// tools/gen_ops_sql.py, because go:embed cannot reach outside this module. `--check`
// fails when a copy is stale, so the DDL cannot differ between the two languages.
//
//go:embed sql/*.sql
var migrationFiles embed.FS

// advisoryLockKey serialises migrations across replicas.
//
// The value is arbitrary but must never change: two binaries that disagree hold
// different locks and migrate concurrently, which is the failure the lock exists to
// prevent. It is spelled out rather than computed from a string so a refactor cannot
// silently change it.
const advisoryLockKey int64 = 0x6f70735f6d696772 // "ops_migr"

// Migration is one numbered DDL step.
type Migration struct {
	// Version is the numeric prefix of the filename: 001, 002, …
	Version int
	// Name is the rest of the filename without the extension.
	Name string
	// SQL is the exact text that was applied.
	SQL string
	// Checksum is the SHA-256 of SQL, recorded so an edited migration is detected.
	Checksum string
}

// ledgerDDL creates the table that records what has been applied. It is created by
// the runner rather than by a migration, because the runner needs it before it can
// record anything.
const ledgerDDL = `
CREATE SCHEMA IF NOT EXISTS ops;
CREATE TABLE IF NOT EXISTS ops.schema_migration (
    version    integer     PRIMARY KEY,
    name       text        NOT NULL,
    checksum   text        NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now()
);`

// loadMigrations reads the embedded DDL, sorted by version.
//
// Sorted rather than trusted to filesystem order: `embed.FS` yields lexical order,
// which is correct for zero-padded names but would silently misorder `10_` after
// `1_` if the padding convention were ever dropped. Sorting on the parsed number
// makes the padding cosmetic.
func loadMigrations() ([]Migration, error) {
	entries, err := fs.ReadDir(migrationFiles, "sql")
	if err != nil {
		return nil, fmt.Errorf("ops: reading embedded migrations: %w", err)
	}

	migrations := make([]Migration, 0, len(entries))
	seen := make(map[int]string, len(entries))
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		versionText, name, found := strings.Cut(strings.TrimSuffix(entry.Name(), ".sql"), "_")
		if !found {
			return nil, fmt.Errorf("ops: migration %q is not named NNN_description.sql", entry.Name())
		}
		version, err := strconv.Atoi(versionText)
		if err != nil {
			return nil, fmt.Errorf("ops: migration %q has a non-numeric version: %w", entry.Name(), err)
		}
		if other, duplicate := seen[version]; duplicate {
			return nil, fmt.Errorf("ops: migrations %q and %q share version %d", other, entry.Name(), version)
		}
		seen[version] = entry.Name()

		body, err := migrationFiles.ReadFile("sql/" + entry.Name())
		if err != nil {
			return nil, fmt.Errorf("ops: reading %q: %w", entry.Name(), err)
		}
		sum := sha256.Sum256(body)
		migrations = append(migrations, Migration{
			Version:  version,
			Name:     name,
			SQL:      string(body),
			Checksum: hex.EncodeToString(sum[:]),
		})
	}

	if len(migrations) == 0 {
		return nil, errors.New("ops: the embedded migrations are empty")
	}
	sort.Slice(migrations, func(i, j int) bool { return migrations[i].Version < migrations[j].Version })
	return migrations, nil
}

// Migrate applies every pending migration and returns how many ran.
//
// Three properties, each of which is a way this goes wrong in production:
//
//   - **One migrator at a time.** A session-level advisory lock is taken before
//     anything is read, so N replicas starting together apply the DDL once.
//   - **Ahead is an error.** If the database already has a migration this binary does
//     not know about, an older binary has been pointed at a newer database — during a
//     rollback, typically. Proceeding would write rows the new schema no longer
//     expects, so it refuses.
//   - **An edited migration is an error.** A version applied with one checksum and
//     presented with another means the DDL changed after it ran. The database and the
//     file disagree, and which one is right is not something a startup can decide.
//
// Re-running when everything is applied applies nothing and returns 0. That is the
// acceptance criterion, and it holds for two reasons: the ledger is checked first,
// and every statement is written with `IF NOT EXISTS` so even a ledger that was lost
// would not fail.
func Migrate(ctx context.Context, pool *pgxpool.Pool) (int, error) {
	migrations, err := loadMigrations()
	if err != nil {
		return 0, err
	}

	// The lock is on a dedicated connection and released explicitly, because the
	// advisory lock is session-scoped: taking it on a pooled connection and returning
	// that connection would leave the lock held by whoever gets it next.
	conn, err := pool.Acquire(ctx)
	if err != nil {
		return 0, fmt.Errorf("ops: acquiring a connection to migrate: %w", err)
	}
	defer conn.Release()

	if _, err := conn.Exec(ctx, "SELECT pg_advisory_lock($1)", advisoryLockKey); err != nil {
		return 0, fmt.Errorf("ops: taking the migration lock: %w", err)
	}
	defer func() {
		// Best effort: the connection is closed when released, which also drops the
		// lock. The explicit unlock keeps the pool's connection clean for reuse.
		_, _ = conn.Exec(context.WithoutCancel(ctx), "SELECT pg_advisory_unlock($1)", advisoryLockKey)
	}()

	if _, err := conn.Exec(ctx, ledgerDDL); err != nil {
		return 0, fmt.Errorf("ops: creating the migration ledger: %w", err)
	}

	applied, err := appliedVersions(ctx, conn)
	if err != nil {
		return 0, err
	}

	// Before applying anything: is the database already ahead of us?
	highestKnown := migrations[len(migrations)-1].Version
	if len(applied) > 0 {
		highestApplied := 0
		for version := range applied {
			if version > highestApplied {
				highestApplied = version
			}
		}
		if highestApplied > highestKnown {
			return 0, fmt.Errorf("%w: database has migration %d, this binary knows up to %d",
				ErrSchemaAhead, highestApplied, highestKnown)
		}
	}

	count := 0
	for _, migration := range migrations {
		if record, done := applied[migration.Version]; done {
			if record != migration.Checksum {
				return count, fmt.Errorf(
					"ops: migration %d (%s) was applied with checksum %s and now has %s; "+
						"an applied migration must never be edited — add a new one",
					migration.Version, migration.Name, record, migration.Checksum)
			}
			continue
		}

		// Each migration in its own transaction, so a failure leaves the ledger
		// agreeing with the schema rather than half-applied.
		if err := applyMigration(ctx, conn, migration); err != nil {
			return count, err
		}
		count++
	}
	return count, nil
}

// appliedVersions reads the ledger as version → checksum.
func appliedVersions(ctx context.Context, conn *pgxpool.Conn) (map[int]string, error) {
	rows, err := conn.Query(ctx, `SELECT version, checksum FROM ops.schema_migration`)
	if err != nil {
		return nil, fmt.Errorf("ops: reading the migration ledger: %w", err)
	}
	defer rows.Close()

	applied := make(map[int]string, 8)
	for rows.Next() {
		var version int
		var checksum string
		if err := rows.Scan(&version, &checksum); err != nil {
			return nil, fmt.Errorf("ops: scanning the migration ledger: %w", err)
		}
		applied[version] = checksum
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("ops: reading the migration ledger: %w", err)
	}
	return applied, nil
}

// applyMigration runs one migration and records it, atomically.
func applyMigration(ctx context.Context, conn *pgxpool.Conn, migration Migration) error {
	tx, err := conn.Begin(ctx)
	if err != nil {
		return fmt.Errorf("ops: beginning migration %d: %w", migration.Version, err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if _, err := tx.Exec(ctx, migration.SQL); err != nil {
		return fmt.Errorf("ops: applying migration %d (%s): %w", migration.Version, migration.Name, err)
	}
	if _, err := tx.Exec(ctx,
		`INSERT INTO ops.schema_migration (version, name, checksum) VALUES ($1, $2, $3)`,
		migration.Version, migration.Name, migration.Checksum); err != nil {
		return fmt.Errorf("ops: recording migration %d: %w", migration.Version, err)
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("ops: committing migration %d: %w", migration.Version, err)
	}
	return nil
}

// VerifySchema checks that the database has at least the migrations this binary
// knows about, without applying anything.
//
// This is what a replica calls at startup instead of Migrate: AGENTS.md rule 12 says
// a replica never migrates, it verifies and refuses to start if behind. Migrating
// from a replica start-up is how N replicas each try to apply the same DDL.
func VerifySchema(ctx context.Context, pool *pgxpool.Pool) error {
	migrations, err := loadMigrations()
	if err != nil {
		return err
	}
	required := migrations[len(migrations)-1].Version

	var present bool
	if err := pool.QueryRow(ctx, `SELECT to_regclass('ops.schema_migration') IS NOT NULL`).Scan(&present); err != nil {
		return fmt.Errorf("ops: checking the migration ledger: %w", err)
	}
	if !present {
		return fmt.Errorf("%w: no migration ledger; the migrator has not run", ErrSchemaBehind)
	}

	var highest int
	if err := pool.QueryRow(ctx, `SELECT coalesce(max(version), 0) FROM ops.schema_migration`).Scan(&highest); err != nil {
		return fmt.Errorf("ops: reading the schema version: %w", err)
	}
	if highest < required {
		return fmt.Errorf("%w: database is at %d, this binary requires %d", ErrSchemaBehind, highest, required)
	}
	return nil
}

// Migrations returns the versions and names this binary carries, for a startup log
// line and for tests.
func Migrations() ([]Migration, error) {
	return loadMigrations()
}

// ensure the pgx import is used even if a future edit removes the only direct use.
var _ = pgx.ErrNoRows

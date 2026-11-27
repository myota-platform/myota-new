# libs/ops

The operational pattern every service shares: transactional outbox, idempotency
records, job claiming, dead letters, and the partition assignment that keeps
per-aggregate ordering intact.

## It exists twice, and that is deliberate

```
libs/ops/go/      used by gateway, activity
libs/ops/rust/    used by core, geodata
libs/ops/conformance/   the fixtures both must pass
```

A Go service cannot link a Rust crate, so polyglot (ADR-0002) means this cannot be
one library. Twelve copies became two, not one — a duplication the consolidation
did not remove, recorded as a cost in ADR-0002 rather than glossed over.

**The code is duplicated; the contract is not.** `conformance/` holds
language-neutral fixtures describing what the two implementations must agree on,
and each language's test suite runs them. A behaviour fix on one side fails the
other side's tests until it catches up.

What that does not solve: a fix in one implementation is still not a fix in the
other. The suite catches divergence, not duplication.

## What is here

| Concern | Go | Rust |
| --- | --- | --- |
| Partition assignment | `Partition(subject, count)` | `partition(subject, count)` |
| Transactional outbox | `Store.Enqueue(ctx, tx, event)` | `outbox::enqueue(conn, event)` |
| Partition-leased relay | `ClaimPartition` / `PublishPartition` / `RelayOnce` | same names, `snake_case` |
| Job queue | `Claim` / `Complete` / `Fail` / `Extend` / `Reap` | same |
| Idempotency | `ClaimIdempotency` / `SaveIdempotentResponse` / `Middleware` | claim / save / release |
| Event feed | `ReadEvents` / `AckEvents` / `Trim` / `EventLag` | same |
| Migrations | `Migrate` / `VerifySchema` | `migrate` / `verify_schema` |

The two APIs mirror each other method for method, so a reader moving between services
finds the same names and the same semantics.

## The DDL is not duplicated

`libs/ops/sql/` is written once. `tools/gen_ops_sql.py` copies it into each module —
`go:embed` and `include_str!` are both confined to their own module directory — and
`--check` fails when a copy is stale. That is the one duplication in this library that
would be silent: a column added on one side would make the other side's queries fail at
run time, in production, on one service only.

## Tests

```sh
make -C libs/ops test          # both languages; needs a PostgreSQL or Docker
OPS_TEST_DATABASE_URL=... make -C libs/ops test
```

The suite is split deliberately:

- **unit tests** (`go test ./...`, `cargo test --lib`) run anywhere;
- **integration tests** (`-tags integration`) need a real PostgreSQL and cover the
  guarantees that only exist in the database: `FOR UPDATE SKIP LOCKED`, the advisory
  lock, the partial unique index, and exactly-once publication under four concurrent
  relays;
- **conformance** runs the same fixtures in both languages.

A database per test, created and dropped, so every assertion is absolute rather than
relative to whatever ran before.

## Migrations

`Migrate` applies the embedded DDL under a session advisory lock, records each version
with a SHA-256 of its text, and returns how many it applied. Re-running applies 0.

Three properties, each a way this goes wrong in production:

- **One migrator at a time** — a session advisory lock, so N replicas starting together
  apply the DDL once.
- **Ahead is an error** — a database with a migration this binary does not know means an
  older binary has been pointed at a newer database, which during a rollback is exactly
  the situation that corrupts data.
- **An edited migration is an error** — a version applied with one checksum and presented
  with another. The database and the file disagree, and a startup cannot decide which is
  right.

A replica calls `VerifySchema` instead and refuses to start if behind (AGENTS.md rule 12).

## Not a shared database

`libs/ops` holds code, not schema. Each service applies the `ops` DDL from its own
migrations, in its own database, because the outbox row must be written in the same
transaction as the domain change.

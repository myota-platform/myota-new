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

## Current state

Chunk F1 provides `Partition` only — the FNV-1a 64 partition assignment from
`docs/architecture/messaging.md`. Both languages assert the **same independently
computed expected values**, so the two implementations already agree on the one
thing that exists.

F4 adds the outbox, idempotency, job claim and dead-letter operations.

## Not a shared database

`libs/ops` holds code, not schema. Each service applies the `ops` DDL from its own
migrations, in its own database, because the outbox row must be written in the same
transaction as the domain change.

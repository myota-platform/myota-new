# `libs/ops` conformance suite

Language-neutral fixtures that **both** implementations must pass.

## Why this exists

Polyglot (ADR-0002) means `libs/ops` cannot be one library — a Go service cannot
link a Rust crate. The outbox, idempotency, job-claim and dead-letter logic exists
twice:

```
libs/ops/go/      used by gateway, activity
libs/ops/rust/    used by core, geodata
```

That is duplication the design set out to remove, reintroduced by a different
decision. It cannot be fixed by packaging. It can be made *safe*, by ensuring the
two implementations agree on behaviour even though they share no code.

**The code is duplicated. The contract is not.** These fixtures are the contract.

## What a case is

A case describes a scenario against the `ops` *contract* — not against SQL and not
against a language API — plus the outcome that must hold. Neither implementation
may special-case a fixture.

```json
{
  "name": "outbox-duplicate-no-effect",
  "given": {
    "events": [
      { "id": "e1", "subject": "entity:8f3c", "type": "geodata.entity.published.v1" }
    ]
  },
  "when": [
    { "op": "relay.publish", "times": 2 }
  ],
  "expect": {
    "published": ["e1"],
    "publishedCount": 1
  }
}
```

The operations are the same four the contract exposes everywhere:

| op | meaning |
| --- | --- |
| `outbox.write` | write an event inside the current transaction |
| `outbox.relay` | publish a partition's unpublished events, in `seq` order |
| `job.claim` | take up to N jobs, disjoint from any other live claimer |
| `job.settle` | mark done, retry with backoff, or dead |

## The cases

| File | Asserts |
| --- | --- |
| `outbox-publish-once.json` | an event is published exactly once per partition |
| `outbox-order.json` | events for one subject publish in `seq` order |
| `outbox-duplicate-no-effect.json` | a repeated event id produces one job, one effect |
| `outbox-commit-rollback.json` | an event for a rolled-back transaction is never published |
| `job-claim-disjoint.json` | concurrent claimers take disjoint rows |
| `job-lease-expiry.json` | an expired lease is reclaimable by another replica |
| `job-attempts-dead.json` | eight failures reach `dead`, not infinite retry |
| `job-idempotent-completion.json` | completing a job twice is not an error |
| `idempotency-replay.json` | the same `Idempotency-Key` returns the stored response |
| `idempotency-conflict.json` | the same key with a different body is rejected |

## Runner contract

Each implementation provides a test harness that:

1. starts a real Postgres (containers), applies the `ops` DDL from that language's
   migrations;
2. exposes the four operations through a thin adapter the runner calls;
3. loads every case in `cases/`, runs it, and asserts every key in `expect`;
4. **fails on an unknown case**, so a fixture added for one language cannot be
   silently skipped by the other.

```
libs/ops/go/conformance_test.go       TestConformance
libs/ops/rust/tests/conformance.rs    conformance()
```

Run locally:

```sh
make -C libs/ops/go test       # includes the suite
make -C libs/ops/rust test     # includes the suite
```

Both run in CI whenever `libs/ops/**` changes — see `gateway.yml`, `activity.yml`,
`core.yml`, `geodata.yml`, whose path filters include the relevant implementation.

## Status

**Specified, not yet implemented.** The fixtures above are the design; the two
implementations and their runners arrive with chunk F4. This file exists now so F4
builds against a contract rather than inventing its own tests twice and hoping they
agree.

## What this does not solve

A behaviour fix in one implementation is not a fix in the other. The suite catches
*divergence*, not *duplication*: someone still has to make the second change, and
the suite is what tells them they must.

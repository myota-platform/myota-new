# ADR-0004 — Remove the message broker; use the outbox as the event log

**Status:** accepted · 2026-11-27
**Supersedes:** the NATS JetStream decision inherited from the previous implementation

## Context

The previous implementation used NATS JetStream as a work queue. Measured
surface area in `/root/myota-next`:

| Indicator | Count |
| --- | --- |
| Files mentioning JetStream | 24 |
| Lines mentioning JetStream | 253 |
| Restart-recovery test suites that exist because of broker semantics | 5 |
| Streams | 2 (`MYOTA_ACTIVITY_WORK`, `MYOTA_GEODATA_WORK`) plus fact stream |
| Durable consumers configured | 6+ |

The old relay was already correctly scoped — one instance group per owning
database, each receiving only its own `OUTBOX_DATABASE_URL`
(`myota-next/docs/architecture.md:80-82`). That is not the problem being solved
here. The problem is that a second durable log existed at all, and that
publication stalled when the broker was unavailable even though the event was
already committed.

The cluster runs no other messaging infrastructure. Node capacity is 12 CPU /
64 GB / 256 pods, so the constraint is not capacity — it is the number of stateful
systems to operate, and the number of failure modes to reason about.

Crucially, **the system already had a durable log**: the transactional outbox. The
broker was a second durable log in front of the first. An outbox row was not
marked published until the broker accepted it, so a broker outage stalled
publication even though the event was already durable in Postgres.

## Decision

Remove the broker. Use the outbox as the event log, read by consumers through an
internal HTTP feed with durable per-partition cursors, and `ops.job` with
`FOR UPDATE SKIP LOCKED` for competing-consumer work.

Specification: [docs/architecture/messaging.md](../architecture/messaging.md).

## What the broker was actually providing

| Guarantee | Still provided by | Notes |
| --- | --- | --- |
| Durability until acknowledged | the outbox, committed in the domain transaction | was always the real source of truth |
| At-least-once delivery | job insert idempotent on event id | `ON CONFLICT DO NOTHING` |
| Per-subject ordering | partition assignment from `hash(subject)` | preserved |
| Competing consumers | `FOR UPDATE SKIP LOCKED` on `ops.job` | same primitive the broker used internally |
| Bounded redelivery | `attempt` counter → `dead` | simpler than `max_deliver` + DLQ |
| Fan-out | one checkpoint set per consumer | unchanged |

Nothing the system relied on required a broker. The broker's unique contribution
was decoupling producer *availability* from consumer progress, discussed below.

## Alternatives considered

**Keep JetStream.** Rejected: it is the largest remaining piece of infrastructure
for the smallest functional contribution, and its guarantees were already
duplicated by the outbox. On a single node it provides no availability benefit
over local disk.

**Redis Streams.** Genuinely simpler than JetStream — `XADD`/`XREADGROUP`/`XACK`,
consumer groups created implicitly, no stream or durable lifecycle to manage. But
it adds a stateful server the cluster does not otherwise need, and duplicates the
one thing Postgres already does well here. Would be the first choice if a broker
became necessary.

**Postgres-based bus in a dedicated `myota_bus` database.** Correct and
well-understood, but adds a fourth database to operate, back up, and restore —
which contradicts the three-database decision in
[ADR-0001](0001-service-boundaries-follow-databases.md) for a component that holds
no domain state.

**RabbitMQ / Kafka.** Rejected: significantly more infrastructure and more
concepts than the workload justifies.

## Consequences

**Gained**

- One stateful system fewer: no NATS server, stream config, durable lifecycle, or
  8 GiB PVC.
- One durable log instead of two that must agree.
- Consumer-paced backpressure: a slow consumer creates lag, not broker memory
  pressure.
- ~250 lines of broker glue and 5 recovery test suites deleted.
- Relay sharding (ADR-0005) becomes possible, removing the single-relay ceiling.

**Lost, and this is real**

- **Producer availability couples to consumer progress.** If `core` is down, a
  consumer cannot fetch `core.*` events, though it still processes jobs it holds.
  Since a producer outage means no new events exist, the practical exposure is the
  events produced just before the crash. Accepted knowingly.
- **Ring-buffer retention.** A consumer more than 14 days behind loses events and
  requires a full resync. Mitigated by an ack floor that prevents trimming past a
  live consumer, plus an alert at 24 h lag. A broker could hold retention
  independently of the producer's database.
- **A puller ceiling.** One puller per (consumer, source, partition) handles
  roughly 10⁴ events/s. Beyond that, raise `PARTITIONS`. A broker would scale this
  without intervention.

## Reconsideration triggers

Adopt a broker — Redis Streams first — if any of these hold:

- sustained event volume above 10⁶/day;
- a consumer must survive a multi-day outage without a manual resync;
- services multiply past ~8 and trim/ack bookkeeping becomes its own maintenance;
- a non-Postgres producer must publish into the same stream.

The seam is already in the right place: `GET /internal/events`, the outbox relay,
and the puller. Swapping transport changes those three components and no domain
code.

## Verification

1. Exactly-once per consumer: run two relays over the same partitions, assert one
   publication per entity.
2. Cursor correctness: kill a puller mid-batch, restart, assert no gap and no
   duplicate job.
3. Trim safety: stall a consumer, advance time, assert the producer holds the
   ack floor.
4. Lag observability: `event_lag_seconds` and `outbox_oldest_age_seconds` are
   exported and alert at the documented thresholds.

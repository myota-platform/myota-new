# Inter-service messaging

**There is no message broker.** Integration between services is a durable event
log stored in Postgres, read by consumers through an internal HTTP feed, plus a
job table for work that needs competing consumers.

This replaces NATS JetStream. The reasoning is in
[ADR-0004](../decisions/0004-drop-message-broker.md); this document is the
specification.

## Why the broker was removed

The old system used JetStream as a work queue: streams, durable consumers,
`max_deliver`, `ack_wait`, lease recovery sweeps, and five restart-recovery test
suites that existed only to prove the broker would not lose or duplicate work.
That is roughly 250 lines of broker glue plus a stateful server, a persistent
volume, and a category of failure that has to be reasoned about separately from
the domain.

But the system already had a durable log: the transactional outbox. The broker
was a second durable log in front of the first, and the outbox row was not marked
published until the broker accepted it. Every guarantee the system actually
relied on — durability until acknowledged, at-least-once delivery, per-subject
ordering, bounded redelivery — was either already provided by the outbox or can be
provided by a cursor over it.

## The model

Two mechanisms, both in Postgres, both owned by the service whose data they
describe.

### 1. Facts — the event log

Every service keeps its committed events in `ops.outbox_event`, written in the
same transaction as the domain change.

```
ops.outbox_event
  seq          bigserial    monotonic per producer database
  id           uuid         unique, the event id
  type         text         e.g. geodata.entity.published.v1
  subject      text         the aggregate, e.g. entity:8f3c…
  partition    int          hash(subject) % PARTITIONS
  payload      jsonb        CloudEvents-style envelope
  created_at   timestamptz
```

Each service exposes one internal endpoint:

```
GET /internal/events?after=<seq>&limit=<n>&types=a,b
→ 200 { "events": [ { "seq", "id", "type", "subject", "partition", "time", "data" } ],
        "nextSeq": 1234, "more": true }
```

`seq` is monotonic but **not contiguous**: rows are trimmed, so consumers must
treat it as a cursor, never as a count.

### 2. Work — the job table

Work that needs competing consumers does not need an event feed at all. It is a
row in `ops.job` in the owning database:

```
ops.job
  id            uuid
  kind          text      import.preprocess · import.fetch · entity.enrich
                          entity.delete · notification.send · award.evaluate
                          account.retention
  payload       jsonb
  state         queued | leased | done | dead
  attempt       int
  lease_until   timestamptz
  available_at  timestamptz
```

Workers claim with `FOR UPDATE SKIP LOCKED`, which is exactly what JetStream's
durable pull consumer was doing, minus the server:

```sql
UPDATE ops.job SET state = 'leased', lease_until = now() + interval '300 seconds',
                   attempt = attempt + 1
WHERE id IN (
  SELECT id FROM ops.job
   WHERE state = 'queued' AND available_at <= now()
   ORDER BY available_at
   FOR UPDATE SKIP LOCKED
   LIMIT 20)
RETURNING *;
```

N replicas claim disjoint rows. No coordination beyond the database.

## Consumption

A consuming service runs a **puller** and N **processors**.

```
producer                          consumer
────────                          ────────
ops.outbox_event                  ops.consumer_checkpoint
       │                                  ▲
       │  GET /internal/events?after=…    │ advance
       └──────────────► puller ───────────┤
                            │             │
                            └──► ops.job ─┴──► processors (N)
```

The puller converts events into jobs, idempotently:

```sql
INSERT INTO ops.job (id, kind, payload)
VALUES ($1, $2, $3)
ON CONFLICT (id) DO NOTHING;   -- the event id is the job id
```

That single line is what replaces broker de-duplication. An event processed twice
is a no-op, because the job id *is* the event id.

### Cursor and leases

```
ops.consumer_checkpoint
  consumer      text        e.g. activity
  source        text        e.g. core
  partition     int         0..PARTITIONS-1
  last_seq      bigint      the last event applied for this partition
  lease_owner   text        replica that currently owns the partition
  lease_until   timestamptz
```

A puller claims a partition with `FOR UPDATE SKIP LOCKED`, pulls until caught up,
releases. With `PARTITIONS = 32`, up to 32 pullers run concurrently across all
consumer replicas, and ownership is dynamic: no ordinal, no rebalancing protocol,
no consumer group coordinator.

**Ordering.** `partition = hash(subject) % PARTITIONS`, so every event for one
aggregate lands on one partition and is applied in `seq` order. Cross-aggregate
ordering is not promised, and nothing in the design needs it.

**Consumer scaling.** Add replicas of the consumer service; each claims free
partitions. If all 32 partitions are leased, throughput is capped — raise
`PARTITIONS` in a migration, which is a bigger constant, not a redesign.

## Retention and the safe-trim problem

The producer trims `ops.outbox_event` so the log does not grow without bound. It
must not trim past a consumer's cursor, or that consumer silently loses events.

Consumers therefore push their position back:

```
POST /internal/events/ack   { "consumer": "activity", "seq": 91823 }
→ 204
```

The producer records the lowest acknowledged `seq` per consumer in
`ops.event_ack`, and trims with both bounds:

```sql
DELETE FROM ops.outbox_event
WHERE created_at < now() - interval '14 days'      -- hard ceiling
  AND seq < COALESCE((SELECT min(last_acked_seq) FROM ops.event_ack), 0)
                                                    -- never past a live consumer
  AND seq < (SELECT max(seq) FROM ops.outbox_event) - 1000;
```

Two independent safety rails:

- **Hard ceiling, 14 days.** Bounds storage even if a consumer is stuck or gone.
- **Ack floor.** Never trims past what a live consumer has confirmed.

If a consumer falls behind the hard ceiling, it has lost events. That is a
**detected condition, not a silent one**:

| Signal | Threshold | Action |
| --- | --- | --- |
| `event_lag_seconds{consumer,source}` | > 60 s | warn |
| `event_lag_seconds{consumer,source}` | > 1 h | page |
| `event_lag_seconds{consumer,source}` | > 24 h | resync required |
| `outbox_oldest_age_seconds` | > 12 days | page — trim is being held by a consumer |

Recovery from a lost consumer is a **full resync**: the consumer drops its cursor
and re-reads the producer's current state through the public API, not the event
log. It is slow and manual, which is the correct incentive to keep lag bounded.

## Delivery semantics, stated plainly

| Property | Guarantee | How |
| --- | --- | --- |
| Durability | committed before publish | outbox written in the domain transaction |
| Delivery | at-least-once | job insert is idempotent on event id |
| Duplicates | possible, harmless | job id = event id, `ON CONFLICT DO NOTHING` |
| Ordering | per aggregate | partition by subject hash |
| Fan-out | one cursor per consumer | each consumer has its own checkpoint rows |
| Backpressure | consumer-paced | consumers pull; the producer never pushes |
| Redelivery | bounded | `attempt` counter, then `state = 'dead'` |

## What this costs

The honest comparison with a broker.

**Gained**

- No NATS server, stream configuration, durable lifecycle, or PVC.
- One durable log (the outbox) instead of two that must agree.
- Consumers pace themselves; a slow consumer cannot fill a broker's memory.
- Backpressure is explicit: lag is a metric, not a queue depth.
- Scaling consumers is adding replicas and free partitions.

**Lost**

- **Producer availability is coupled to consumer progress.** If `core` is down, a
  consumer cannot fetch `core.*` events. It can still process jobs it already
  holds. In practice a producer outage means no new events exist, so the
  practical difference is small — but it is real and should be stated.
- **Ring-buffer semantics.** A consumer that falls behind the retention ceiling
  loses events and needs a resync. A broker's retention can be configured larger
  independently of the producer's database.
- **The puller has a ceiling.** One puller per (consumer, source, partition) can
  do roughly 10⁴ events/s; beyond that, raise `PARTITIONS`. A broker would scale
  this out of the box.

## When to reconsider

This design is correct for the current shape: three services, one node, event
volumes in the 10⁴–10⁵ per day range. Reconsider a broker if any of these become
true:

- event volume exceeds ~10⁶ per day sustained, or
- a consumer must survive multi-day outage without a resync, or
- services multiply past ~8 and the ack/trim bookkeeping becomes its own problem,
- or a non-Postgres producer (external system) must publish into the same stream.

If that happens, the seam is already in the right place: the puller's `GET
/internal/events` and the producer's outbox relay are the only two places that
know how events travel. Swapping in Redis Streams or JetStream is a change to
those two components, not to any domain code.

## Subjects and partitions

`partition` must be stable across the producer's lifetime and across replicas.
It is computed from `subject`, never from time or from a counter:

```
partition = fnv1a64(subject) % PARTITIONS
```

`subject` is always `<aggregate>:<id>`, e.g. `entity:8f3c…`, `activation:1b2d…`,
`account:44a1…`. Two events about the same aggregate are therefore always ordered
relative to each other.

## What the consumers subscribe to

Declared in code, not in a table — a consumer's interests are a property of its
logic, and a table would be a second place to keep in sync.

| Consumer | Source | Event types |
| --- | --- | --- |
| `activity` | `core` | `account.created`, `account.closed`, `programme.published`, `programme.archived` |
| `activity` | `geodata` | `entity.published`, `entity.retired` |
| `geodata` | `core` | `programme.published`, `programme.archived`, `account.closed` |
| `core` | `geodata` | `import.completed` (admin notification) |
| `core` | `activity` | `award.issued` (notification), `activation.closed` (optional) |

Unsubscribed types are ignored by the puller and never become jobs.

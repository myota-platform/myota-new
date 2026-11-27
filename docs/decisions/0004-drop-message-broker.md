# ADR-0004 — Remove the message broker; use the outbox as the event log

**Status:** accepted · 2026-11-27
**Provenance:** directed — "see if NATS/JetStream is really needed, adopt a simpler pub/sub solution if viable".
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

## Do you need Redis Streams?

**No. Nothing in this system uses Redis, and nothing needs to.** Redis Streams
appears in this ADR only as a *fallback if the design ever fails* — it is not a
component, not a dependency, and not on the deployment list.

If the question behind the question is *"isn't ordinary pub/sub enough?"*, the
answer is also no, for a reason that is structural rather than a matter of taste.
The word covers three different things with three different guarantees:

| Tier | Examples | Delivery | Survives a subscriber restart | Distributes work across replicas | Sufficient here |
| --- | --- | --- | --- | --- | --- |
| In-process | Go channels, Rust `broadcast`, Node `EventEmitter` | synchronous | n/a — same process | no | **No** |
| Fire-and-forget | Redis Pub/Sub, Postgres `LISTEN`/`NOTIFY`, MQTT QoS 0 | at-most-once | **no** | **no** — every subscriber gets every message | **No** |
| Persistent log | Redis Streams, JetStream, Kafka, **the outbox** | at-least-once | yes | yes | Yes — and this is what we already have |

### Why the middle row fails here

That row is what people usually mean by "simple pub/sub", and it breaks two
requirements the system cannot give up.

**1. Durability.** A worker that restarts loses every message published during the
gap. Redis Pub/Sub holds nothing on disk and has no replay: a subscriber that is
disconnected for 50 ms is simply absent, and the message is gone. Rule 4 says a
restart must never lose an effect, so a transport that loses effects on restart is
not a weaker option — it is a different contract.

**2. Work distribution.** Pub/Sub delivers every message to *every* subscriber.
That is fan-out, which is correct for cache invalidation and wrong for a job
queue. Spreading work across N worker replicas needs partition assignment,
ownership, lease expiry and rebalancing. Building those on top of a fan-out bus is
most of a broker, written by hand, with no tests.

There is also no acknowledgement. Nothing tells the producer the work happened, so
there is no basis for retry — and retry is the entire reason `ops.job` exists.

### The nearest miss: Postgres `LISTEN`/`NOTIFY`

This is the most tempting option because it is already installed. It has one
genuine advantage: notifications are delivered only after the transaction commits,
so it is transactional in exactly the way a domain event needs.

It still fails the same two tests — fire-and-forget, and all listeners receive
every notification — plus two of its own:

- the payload is limited to 8 000 bytes, so any real event has to be a pointer;
- **it is incompatible with PgBouncer in transaction mode**, because `LISTEN`
  needs a session-level connection that survives across statements. Every listener
  replica would need a dedicated direct connection, reintroducing the connection
  pressure that [ADR-0005](0005-horizontal-scaling.md) exists to remove.

*This limitation is documented PgBouncer behaviour, not something measured here —
the host has no PgBouncer installed and no throwaway database to test against.*

`LISTEN`/`NOTIFY` could still serve one narrow purpose: as a wake-up hint so a
puller notices new events in milliseconds instead of waiting for its next poll.
Rejected as not worth it: the polling interval is 2 seconds, and every workflow in
this system — imports measured in minutes, awards evaluated nightly, notifications
in tens of seconds — has a tolerance far above that. A hint mechanism would add a
session connection per replica to save latency nobody can perceive.

### Why Redis Streams was named as the fallback at all

Because if a trigger in the list below ever fires, Redis Streams is the smallest
broker that actually meets the requirement — it has persistence, consumer groups,
and acknowledgements, unlike its own Pub/Sub sibling. But *"the smallest sufficient
broker"* is not the same claim as *"the one we would adopt"*, and naming a product
in advance skips the analysis that should happen when the trigger fires.

**Corrected position:** if a trigger fires, evaluate Redis Streams and Kafka against
the specific constraint that fired — throughput, independent retention, or a
non-Postgres producer. Do not adopt a broker because it is the one written down
here.

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
consumer groups created implicitly, no stream or durable lifecycle to manage. It
meets the durability requirement that Redis Pub/Sub does not. Rejected for now
because it adds a stateful server the cluster does not otherwise need, to replicate
a job Postgres already performs — but it is a legitimate candidate *if* a trigger
below fires. See "Do you need Redis Streams?" above for why its Pub/Sub sibling is
not an option at all.

**Redis Pub/Sub.** Not merely rejected — insufficient. No persistence, no replay,
no acknowledgement, no consumer groups, and every subscriber receives every
message. A worker restart loses the messages published during the gap, which
violates the job contract outright.

**Postgres `LISTEN`/`NOTIFY`.** The most tempting option, since Postgres is already
running and notifications are transactional. Rejected: fire-and-forget, fan-out to
all listeners, an 8 000-byte payload ceiling, and incompatible with PgBouncer
transaction pooling — each listener would need a dedicated session connection. A
viable *wake-up hint*, not a viable transport.

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
- **No new runtime dependency of any kind.** Removing the broker did not require a
  replacement bus, an in-memory cache, or a second store. The pub/sub capability
  the system needs is a table, a cursor and an endpoint.

**Cost** — stated plainly, not buried

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

Reconsider a broker if any of these hold. If one fires, evaluate the options
against the constraint that fired rather than defaulting to a name:

- sustained event volume above 10⁶/day;
- a consumer must survive a multi-day outage without a manual resync;
- services multiply past ~8 and trim/ack bookkeeping becomes its own maintenance;
- a non-Postgres producer must publish into the same stream.

When a trigger fires, the comparison is not "which broker" but "which guarantee did
Postgres stop providing":

| If the trigger is | Then the requirement is | Candidates |
| --- | --- | --- |
| Throughput above ~10⁶ events/day | publish/consume rate | Redis Streams, Kafka, partitioning the outbox further |
| Independent retention | keep events longer than the producer's database allows | Kafka, object-storage archive |
| A non-Postgres producer | an ingest point outside the services | Redis Streams, Kafka |
| Trim/ack bookkeeping becoming error-prone | fewer moving parts in the app | a managed bus, accepting a new dependency |

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

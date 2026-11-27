# ADR-0005 — Every workload is horizontally scalable

**Status:** accepted · 2026-11-27

## Context

The brief: all services, including the front ends, must scale horizontally as load
increases. The previous design had three places that could not:

1. **The outbox relay was "exactly one leader per database"**, elected by a
   Postgres advisory lock. Correct for ordering, but a hard throughput ceiling and
   a single point of failure for event publication.
2. **The gateway rate-limited in memory**, so N gateway replicas meant N times the
   allowed request rate.
3. **Upload bytes were proxied through `geodata-api`**, making the API's memory
   and bandwidth proportional to file size rather than to request count.

There were also two latent problems that would only appear at scale: migrations
running from replicas, and cron loops firing once per replica.

## Decision

**Any replica must serve any request at any moment, and killing any replica at any
moment must be invisible.** Six mechanisms enforce this:

| Concern | Mechanism |
| --- | --- |
| Session state | Postgres (`myota_core.auth_session`); no in-process sessions, no affinity |
| Relay sharding | Lease per partition via `FOR UPDATE SKIP LOCKED`; up to `PARTITIONS` relay replicas |
| Consumer duplication | Job id = event id, `ON CONFLICT DO NOTHING` |
| Rate limiting | Traefik middleware at the single ingress, not in the gateway |
| Migrations | Pre-deploy `Job` under an advisory lock; replicas verify and refuse to start if behind |
| Scheduled work | `ops.job` with `UNIQUE (kind, scheduled_for)`, not in-process timers |

Plus two structural changes:

- **Presigned S3 URLs for uploads** so file bytes never traverse an API replica.
- **PgBouncer in transaction mode** per database, because connections — not CPUs —
  are the first real scaling wall.

Front ends scale trivially: static trees, content-hashed assets, immutable cache
headers, no server-side rendering, no session, no database client.

Specification: [docs/architecture/scaling.md](../architecture/scaling.md).

## Alternatives considered

**Keep the single relay leader.** Rejected: it caps event publication at one
replica's throughput, contradicts the stated requirement, and makes relay restarts
a publication gap. Partitioned leases preserve ordering *and* scale.

**Sticky sessions at the load balancer.** Rejected: it makes replica loss a user-
visible event, prevents even distribution, and moves correctness into an
infrastructure setting. Sessions are already rows in Postgres; affinity buys
nothing.

**In-memory rate limiting with a documented multiplier.** Rejected: "the limit is
N times what you configured" is not a security control.

**Keep proxying uploads and scale the API.** Rejected: the API would need memory
proportional to the largest upload, and adding replicas would not make a single
1 GiB upload faster.

**Server-side rendering for the front ends.** Rejected: it converts a static
artifact into a stateful service with a database connection, for no product
benefit. Static files scale on network throughput alone.

**Service mesh for traffic management.** Rejected: a mesh solves mTLS, retries and
observability for many services on many nodes. Three services on one node do not
need it, and it would add a control plane to operate.

## Consequences

**Gained**

- Relay publication scales to `PARTITIONS` replicas with order preserved.
- Adding a replica is always safe and never requires a coordination protocol.
- Killing any pod is invisible.
- Front ends consume zero database connections, so they scale independently of
  the data tier.

**Cost**

- **PgBouncer is now required infrastructure.** Without it the system fails at
  roughly three replicas per service. It is one small deployment per database, but
  it is a new component and a new failure mode.
- **Transaction pooling constrains session semantics.** No session-level `SET`, no
  `LISTEN`, no advisory lock held across statements. The design complies, but it
  is a rule future code must respect.
- **Partitions are a fixed constant.** Scaling beyond `PARTITIONS` relay or puller
  replicas requires raising it in a migration.
- **No push to browsers.** Live behaviour is polling, with up to one poll
  interval of latency. Accepted in exchange for affinity-free serving.
- **Presigned uploads move trust to the client.** The API must verify size and
  checksum at completion, and the bucket policy must scope writes to a session
  prefix, or a client can upload arbitrary content.

## Verification

| Claim | Test |
| --- | --- |
| Killing any replica is invisible | delete a random replica of each workload under load; no failed request, no lost or duplicated job |
| Connections stay bounded at max replicas | scale everything to its documented maximum; assert Postgres connections < `max_connections` |
| Relays do not duplicate | two relays over the same partitions; exactly one publication per event per consumer |
| No affinity needed | disable session affinity on every Service; end-to-end suite passes |
| Migrations do not race | deploy with 10 replicas; exactly one migration runs, nine wait, all serve |
| Cron does not multiply | 10 replicas attempt the same schedule bucket; exactly one job row exists |

A change that breaks any of these is a regression in the scaling contract.

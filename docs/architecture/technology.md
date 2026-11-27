# Technology choices per service

The previous implementation was uniformly TypeScript on NestJS: twelve
applications, each a full Node runtime. That is a defensible default, but it
imposed the same cost on every service regardless of what the service does. A
proxy that shuffles bytes, a service that parses 200 MB shapefiles, and a service
that evaluates award rules have nothing in common operationally.

This document recommends a language **and framework per service**, then states
the trade-off honestly.

## Summary

| Service | Language | Framework | Runtime footprint | Why |
| --- | --- | --- | --- | --- |
| `gateway` | **Go 1.24+** | **Chi** | ~20 MB, 2–5 ms p99 overhead | Pure I/O multiplexing. Needs a single static binary with the contracts embedded, predictable tail latency, and nothing else. |
| `core` | **Rust** (2024) | **Axum** + **SQLx** | ~30 MB | Highest-risk surface (credentials, sessions, MFA). Compile-time-checked SQL catches schema drift; no GC pause during token validation. |
| `geodata` | **Rust** (2024) | **Axum** + **SQLx** + **geozero** | ~120 MB under load | CPU-bound geometry: shapefile parsing, reprojection, simplification, tiling. Memory safety without a GC matters at 200 MB features. |
| `activity` | **Go 1.24+** | **Echo** + **pgx** | ~40 MB | Throughput and concurrency: QSO ingestion, ADIF streaming, award evaluation fan-out. Straightforward worker pools. |
| `admin-web` | TypeScript | **Vue 3 + Vite** | static | Data-dense tables, forms, charts. Vue's ecosystem for tables and complex forms is the strongest fit. |
| `participant-web` | TypeScript | **Svelte 5 + Vite** | static | Content-forward, mobile-first. Svelte ships the least JavaScript for slow phones. |

## The honest trade-off

Three languages means three toolchains, three CI pipelines, three dependency
audit paths, and three sets of on-call knowledge. That is real cost and it
directly contradicts "remove complexity".

**So there is a default recommendation, and the polyglot split is the option.**

### Default: Go everywhere

If a single choice must be made, **Go 1.24 + Chi (gateway, activity) and
Echo (core, geodata)** is the answer. Reasons:

- One toolchain, one build image, one lint config, one deployment pattern.
- 20–40 MB RSS per process versus 200–400 MB for the old Node services; the
  whole platform fits comfortably in the cluster's existing headroom.
- Single static binary, `CGO_ENABLED=0`, sub-100 ms cold start — rolling restarts
  are imperceptible.
- `sqlc` generates type-safe Go from plain SQL, which recovers most of the
  compile-time safety argument for Rust while staying in one language.
- Geodata in Go uses `orb` (shapefile), `proj` bindings, and `github.com/twpayne/go-geom`.
  Slower than Rust, comfortably fast enough for batch imports.

### Where polyglot earns its keep

Two services have a specific reason to leave Go:

- **`core` → Rust.** Authentication is the surface that ends the project if it
  is wrong. SQLx verifies every query against the live schema at compile time, so
  a migration that drops a column breaks the build rather than production.
  `argon2` and `webauthn-rs` are first-class. This is the one place where the
  extra toolchain is worth its cost.
- **`geodata` → Rust.** The import pipeline is the only genuinely CPU-bound part
  of the system. `geozero` streams shapefiles without loading them, `proj4rs`
  reprojects without a C dependency, and there is no GC to pacify while holding
  a 500 MB feature set. This is where latency is actually felt by an operator
  waiting on a run.

`gateway` and `activity` stay in Go either way.

## Framework notes

### Gateway — Chi, not a full framework

The gateway does four things: route by prefix, enforce edge policy, serve static
contract files, reverse-proxy. Chi is `net/http` with a router — roughly 1 kLOC of
readable code including the contract composition, `httputil.ReverseProxy` for the
proxy, and `embed.FS` for the contracts. A framework would add middleware the gateway does not need.

Rejected: Kong, Traefik-as-gateway, Envoy. All are correct products, but they
move the contract and the playground out of the repository and into configuration
that cannot be unit-tested with the code. The requirement is that the gateway
*holds* the contracts, so the gateway is ours.

### Core — Axum + SQLx

- `axum` 0.8 on Tokio; tower middleware for tracing, timeouts, body limits.
- `sqlx` with the `postgres`, `uuid`, `time`, `json` features; `sqlx::query!`
  macros verified against a migrated database in CI.
- `argon2` for password hashing, `webauthn-rs` for passkeys, `totp-rs` for TOTP.
- `tower-sessions` or a hand-rolled session store — the session table is already
  in `myota_core`; a library that hides it is working against the design.

### Geodata — Axum + SQLx + geo stack

- `geozero` for Shapefile/GPX/GeoJSON streaming, `proj4rs` for reprojection,
  `geo` for geometry predicates, `geojson` for serialisation.
- PostGIS does the heavy spatial work (`ST_Intersects`, `ST_Simplify`, `ST_TileEnvelope`),
  not the application. The Rust worker prepares and validates; the database indexes.
- Tiles are generated with `ST_AsMVT` straight from PostGIS — no tile server.

### Activity — Echo + pgx

- `echo` v5; `pgx` v5 with `pgxpool` and `CopyFrom` for bulk QSO ingest.
- ADIF parsing as a streaming reader, not a full-file load.
- Award evaluation is a rule interpreter over a bounded candidate set, fanned out
  across a worker pool and persisted as `award_progress`.

### Front ends

Both are TypeScript, both built with Vite, both static — no server rendering, no
Node runtime in production.

- **Admin:** Vue 3.5 + Vue Router + Pinia + TanStack Table for grids; ECharts for
  the operational charts. Chosen for its maturity on dense forms and tables.
- **Participant:** Svelte 5 (runes) + SvelteKit in static-adapter mode, or plain
  Vite if routing stays client-side. Chosen because a map-heavy mobile page
  benefits measurably from a ~15 KB framework runtime.

Shared design tokens live in `libs/ui-tokens` (CSS custom properties, no build
step) so both applications stay visually one product without sharing a component
framework.

## Data access and migrations

| Concern | Choice |
| --- | --- |
| Migrations | **Atlas** (declarative, diff-based) or **dbmate** (plain SQL, one file per change). |
| Query layer | `sqlx` (Rust) · `sqlc` + `pgx` (Go) |
| Connection pooling | `sqlx::PgPool` · `pgxpool` |
| Schema verification in CI | Apply migrations to an ephemeral Postgres, then compile-check queries |
| PostGIS | `myota_geo` only; `CREATE EXTENSION postgis` in the baseline migration |

One migration directory per database. The old repository had two files numbered
`004` in `core/`; a squashed baseline plus append-only numbered files removes
that class of problem entirely.

## Messaging

No broker. The outbox is the log; the job table is the queue.

| Concern | Choice |
| --- | --- |
| Transport | HTTP `GET /internal/events?after=<seq>` between services |
| Log | `ops.outbox_event` in each producer's own database |
| Cursor | `ops.consumer_checkpoint`, leased per partition |
| Work queue | `ops.job`, claimed with `FOR UPDATE SKIP LOCKED` |
| Event envelope | CloudEvents 1.0 subset, JSON, in `contracts/events/` |
| Ordering | `partition = hash(subject) % PARTITIONS`, applied in `seq` order |
| Duplicate handling | job id = event id, `ON CONFLICT DO NOTHING` |
| Retention | 14-day ceiling and an ack floor reported by live consumers |

The relay is part of each service binary, not a separate application. It reads
`ops.outbox_event` in batches, leases a partition, and publishes that partition's
events in `seq` order. Up to `PARTITIONS` relay replicas run concurrently, so there
is no single-leader ceiling.

Every language choice above ships an HTTP client and a Postgres driver, so this
removes a dependency rather than adding one: no `async-nats`, no `nats.go`, no
second connection type, no broker-specific error handling.

Why this over JetStream, and what it costs: [ADR-0004](../decisions/0004-drop-message-broker.md).

## Observability

| Signal | Standard |
| --- | --- |
| Logs | Structured JSON to stdout; request id, trace id, service, level |
| Traces | OpenTelemetry OTLP; gateway creates the root span and forwards `traceparent` |
| Metrics | Prometheus `/metrics` on every process; RED metrics plus domain counters |
| Health | `/healthz` (liveness, no dependencies) and `/readyz` (DB + broker reachable) |

The old system enabled OTLP exporters on twelve processes whether or not a
collector was reachable. Here observability is opt-in by environment variable and
the default is stdout logs plus `/metrics`.

## Rejected alternatives

| Option | Why not |
| --- | --- |
| Node.js + NestJS (status quo) | 200–400 MB per process, 12 processes, decorator-heavy; no incremental benefit over Go for any of these workloads. |
| Java + Spring Boot | Correct and fast, but 300 MB+ per process and slow cold start without native-image work. |
| .NET minimal APIs | Genuinely competitive (fast, AOT-capable). A reasonable Go substitute if the team already knows C#. |
| Python + FastAPI | Best geospatial library ecosystem, but 10–30× slower on CPU-bound import work and heavy to package. |
| Elixir + Phoenix | Excellent for the live-activation channel; disproportionate operational learning for the rest. |
| Java + Quarkus native | Fast startup, but the native-image build cost is not repaid at this scale. |
| Kubernetes service mesh (Istio/Linkerd) | The gateway plus three services does not need mTLS mesh complexity on a single-node K3s cluster. |
| Sealed Secrets | Explicitly excluded — see [secrets-and-config.md](secrets-and-config.md). |
| NATS JetStream | Removed. The outbox is already a durable log; the broker was a second one in front of it. See [ADR-0004](../decisions/0004-drop-message-broker.md). |
| Redis / Redis Streams | Would be the first choice if a broker became necessary, but adds a stateful server for a job Postgres already handles. |
| Service mesh (Istio/Linkerd) | Three services on one node do not need mTLS mesh complexity. |

## Decision record

Two ADRs carry the consequences:

- [ADR-0001](../decisions/0001-service-boundaries-follow-databases.md) — service
  boundaries follow the three databases.
- [ADR-0002](../decisions/0002-language-per-service.md) — language and framework
  selection, including the default-if-forced rule.

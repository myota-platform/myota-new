# ADR-0002 — Language and framework per service

**Status:** proposed · 2026-11-27
**Provenance:** the brief asked for *suggestions* of languages and frameworks, not a selection, so this records a recommendation and stays **proposed**. Nothing is implemented against it yet.
**Supersedes:** the single-runtime assumption of the previous implementation

## Context

The previous system was uniformly TypeScript on NestJS: twelve applications, each
a full Node runtime at roughly 200–400 MB resident. That uniformity bought one
toolchain, one build image, and one on-call skill set.

It also imposed identical cost on wildly different workloads: a byte-shuffling
proxy, a service that parses 200 MB shapefiles, and a service that issues session
tokens were all the same runtime, the same memory envelope, and the same cold
start.

The brief asked for modern, fast microservice frameworks, and accepted that the
choice may differ per service.

## Decision

**Default: Go everywhere.** If a single language must be chosen, the answer is
Go 1.24 with Chi for the gateway and Echo for the domain services.

**Accepted option: polyglot where a service has a specific reason.**

| Service | Language | Framework | Reason |
| --- | --- | --- | --- |
| `gateway` | Go 1.24 | Chi | I/O multiplexing; single static binary with contracts in `embed.FS` |
| `core` | Rust 2024 | Axum + SQLx | Compile-time-verified SQL on the schema that guards credentials; no GC during token work |
| `geodata` | Rust 2024 | Axum + SQLx + geozero | CPU-bound geometry; memory safety without GC at 200 MB feature sets |
| `activity` | Go 1.24 | Echo + pgx | Throughput and worker-pool concurrency for QSO ingest and award fan-out |
| `admin-web` | TypeScript | Vue 3 + Vite | Dense tables and forms; strongest ecosystem for that |
| `participant-web` | TypeScript | Svelte 5 + Vite | Smallest runtime for map-heavy mobile pages |

Front ends are static builds in both cases. No server-side runtime in production.

## Rationale for the two Rust services

Only two services have a reason that survives scrutiny:

**`core`** is the surface that ends the project if it is wrong. `sqlx::query!`
verifies every query against a migrated database at compile time, so a migration
that drops a column breaks the build instead of production. `argon2` and
`webauthn-rs` are first-class rather than wrappers.

**`geodata`** is the only genuinely CPU-bound component. `geozero` streams
shapefiles without loading them whole; `proj4rs` reprojects without a C
dependency; there is no GC to pacify while holding a large feature set. This is
where an operator feels latency, waiting on a run.

`gateway` and `activity` have no such argument and stay in Go.

## Alternatives considered

| Option | Verdict |
| --- | --- |
| Node.js + NestJS (status quo) | Rejected: 12 processes, 200–400 MB each, decorator-heavy, no incremental benefit for any of these workloads. |
| Go everywhere | **Accepted default.** One toolchain; 20–40 MB per process; 100 ms cold start; `sqlc` recovers most compile-time safety. |
| Java + Spring Boot | Rejected: correct and fast, but 300 MB+ per process and slow cold start without native-image work. |
| .NET minimal APIs | Viable Go substitute if the team knows C#; AOT-capable and fast. Not chosen to avoid a third ecosystem with no advantage here. |
| Python + FastAPI | Rejected for the import path: 10–30× slower on CPU-bound work. The geospatial library ecosystem is the best available, but the bottleneck is throughput. |
| Elixir + Phoenix | Excellent for a live-activation channel; disproportionate learning cost for the other services. |
| Java + Quarkus native | Rejected: native-image build cost not repaid at this scale. |

## Consequences

**Gained, if the default is taken (Go everywhere)**

- One toolchain, one CI pipeline, one lint config, one deployment pattern.
- Whole platform fits in existing cluster headroom.
- Geodata is slower than Rust but comfortably adequate for batch imports.

**Cost, if the polyglot option is taken**

- Three toolchains, three CI pipelines, three dependency-audit paths, three
  on-call skill sets. This is real cost and it works against the consolidation
  goal, which is why it is an option and not the default.
- Geodata import throughput and `core` query safety improve measurably.

**Either way**

- Database migrations are declarative and diff-based (Atlas) or plain SQL
  (dbmate); the language does not change that.
- The event contract is JSON read over an internal HTTP feed, regardless of
  producer language. Every language above already ships an HTTP client and a
  Postgres driver, so no broker-specific client library is needed.
- Observability is OpenTelemetry, so traces cross language boundaries.

## Decision rule going forward

A service may only leave the default language for a **measured** reason that is
recorded in an ADR: a latency budget it cannot meet, a memory ceiling it cannot
stay under, or a correctness property it cannot express. Preference is not a
reason.

## Verification

This ADR is **proposed**, so there is nothing to verify yet. What ratification
requires:

| Question | Answer needed before implement |
| --- | --- |
| Is polyglot wanted, or Go everywhere? | one sentence from the owner of the project |
| If polyglot: is a JRE-free CI acceptable with three toolchains? | yes/no, and who carries the on-call cost |
| If Go everywhere: does `sqlc` give enough compile-time safety on the credential path? | a spike, or an explicit acceptance of the difference |

Until those are answered, rule 6 in `AGENTS.md` records this as the working
assumption and implementation should not assume either answer is final.

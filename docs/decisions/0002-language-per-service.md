# ADR-0002 — Language and framework per service

**Status:** accepted · 2026-11-27
**Provenance:** directed — "Record decision on ADR-0002 as Polyglot as proposed", closing [issue #22](https://github.com/myota-platform/myota-new/issues/22). The earlier `proposed` status reflected that the brief had asked only for suggestions; the selection has now been made explicitly.
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

**Polyglot, as proposed.** Each service uses the language that fits the shape of
its work, rather than the whole system sharing one runtime for organisational
convenience.

**Go everywhere remains the escape hatch** if the three-toolchain cost proves
greater than the fit is worth — Go 1.24 with Chi for the gateway and Echo for the
domain services. Reverting to it is a new ADR, not a quiet decision.

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

**Cost — and one of them is architectural, not just operational**

Polyglot means `libs/ops` cannot be one library. A Go service cannot link a Rust
crate. The outbox, idempotency, job-claim and dead-letter implementation — the
single thing this design was most proud of removing duplication from — now exists
**twice**, once per language.

That is a real contradiction of a goal, and it is not fixable by clever packaging:

| | Go implementation | Rust implementation |
| --- | --- | --- |
| Used by | `gateway`, `activity` | `core`, `geodata` |
| Language | Go | Rust |
| Shares code with the other | no | no |

**The mitigation is behavioural, not structural: one conformance suite that both
must pass.** `libs/ops/conformance/` holds the fixtures and the expected outcomes —
the same event published exactly once per partition, the same duplicate producing
no second effect, the same expired lease becoming reclaimable. Each implementation
runs the suite in its own test harness. The *code* is duplicated; the *contract*
is not, and the contract is what the rest of the system depends on.

What that does not give you: a bug fixed in one implementation is not fixed in the
other. The suite catches divergence in behaviour, not in implementation detail, and
someone still has to make the second change.

Operationally, polyglot also means:

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

| Claim | Check |
| --- | --- |
| Every service builds and tests in CI | the per-service workflows (`gateway.yml`, `core.yml`, `geodata.yml`, `activity.yml`) |
| No service silently changes language | a `Cargo.toml` in a Go service directory, or `go.mod` in a Rust one, fails the workflow's scaffold check |
| `libs/ops` has exactly two implementations | `libs/ops/go` and `libs/ops/rust`, and nothing else |
| Both implement the same behaviour | `libs/ops/conformance/` run by each language's test suite — see below |

### The conformance suite

`libs/ops/conformance/` is language-neutral: a directory of fixtures and expected
outcomes, plus a small runner contract each implementation's tests must satisfy.

```
libs/ops/conformance/
  README.md                 the contract, and how to run it from each language
  cases/
    outbox-publish-once.json        an event is published once per partition
    outbox-duplicate-no-effect.json a repeated event id produces no second effect
    job-claim-disjoint.json         N concurrent claims yield disjoint rows
    job-lease-expiry.json           an expired lease becomes reclaimable
    job-attempts-dead.json          8 failures then dead, not infinite retry
    idempotency-replay.json         the same Idempotency-Key returns the stored response
```

A case is `{setup, act, expect}` against the `ops` contract, not against SQL. Both
implementations must pass every case. When a new case is added, both fail until
both are fixed — which is the point: the divergence is made visible rather than
discovered in production.

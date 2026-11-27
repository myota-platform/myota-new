# AGENTS.md

Operational rules for anyone — human or agent — working in **myota-new**.

This repository is the single home for MyOTA development. The previous
repositories (`myota-next`, `myota-platform`, `myota`, `myota-deploy`, and the
per-service repositories) are archived and read-only. Do not open issues, PRs,
or commits against them.

The rules below are not preferences. A change that violates one should be
rejected in review regardless of how convenient the shortcut is.

## 1. API First

Every service is defined by its HTTP contract before it is implemented.

- The contract is an OpenAPI 3.1 document committed under `contracts/`, and it
  is the source of truth — not the handlers.
- Every service serves its own documentation and interactive playground:
  Swagger UI at `/docs` and the raw document at `/openapi.json`, in every
  environment including production.
- Handlers are written against generated types/interfaces from the contract.
- Contract tests compare the live service's `/openapi.json` and responses to the
  committed document. A drift between code and contract fails CI.
- REST conventions: plural resource nouns, HTTP verbs for semantics, stable
  identifiers, RFC 9457 `application/problem+json` for errors, cursor
  pagination for collections, no verbs in paths.

## 2. Microservices

- One deployable per bounded context, under `apps/<service>`.
- A service owns its data. Its database, schema, and migrations are private: no
  other service reads or writes them, and there is no shared database.
- Cross-service data is obtained through the published API or a published
  event — never through the other service's storage.
- Each service carries its own health (`/healthz`), readiness (`/readyz`),
  metrics, migrations, and deployment manifest. One service can be deployed,
  scaled, and rolled back without redeploying the others.

## 3. Communication between services via pubsub

- Asynchronous integration goes through NATS JetStream. The broker is the
  integration fabric, not an implementation detail of one service.
- Event payloads are versioned schemas committed under `contracts/events/`
  (CloudEvents-style envelope: `id`, `type`, `source`, `subject`, `time`,
  `datacontenttype`, `data`). Schema changes follow the same review as API
  changes.
- Publishers write to a durable stream; consumers use durable, explicit-ack
  consumers with a dead-letter path. At-least-once delivery is assumed, so every
  consumer is idempotent.
- Synchronous HTTP between services is the exception, not the default: it is
  allowed only when a caller genuinely needs an immediate answer from another
  service, and must be documented as a dependency in that service's README.
  Service-to-service HTTP call chains are not used to compose a response for a
  user request.

## 4. Job persistence external

- No job state lives only in process memory.

- Work that must survive a restart is persisted before it is acked: a job row in
  the owning service's database (transactional outbox), then a JetStream
  message, then picked up by a worker.
- Workers are idempotent and safe to run concurrently; a job carries its own
  identifiers, attempt count, and status.
- Restarting or rescheduling any pod must never lose or duplicate an effect.
- Long-running and scheduled work uses the same persisted path — not in-process
  timers or unlogged `setTimeout`/cron loops inside an API container.

## 5. Monorepo

One repository, one review surface, one commit history:

```
apps/            deployable services and web front ends
contracts/       OpenAPI documents and event schemas (source of truth)
libs/            shared TypeScript libraries (no service-owned data access)
deploy/          Helm charts, Fleet definitions, values per environment
docs/            architecture, runbooks, decision records
tools/           repo scripts and code generation
```

Shared code goes in `libs/` and is consumed by path/workspace reference. A
library must not reach into an app's database or bypass the contract.

## 6. Deployment on this local K3s cluster

- Target: the local K3s cluster, Fleet workspace **`myota`**, Kubernetes
  namespace **`myota`**.
- Fleet definitions live in `deploy/fleet/myota/` and are tracked by a
  `GitRepo` resource named `myota` in the `myota` workspace. Deploys are GitOps:
  the cluster converges to what is committed here; nobody applies manifests by
  hand to make a release.
- Images are published to `ghcr.io/myota-platform/myota-new-<service>` and
  referenced by immutable digest in values, not by a floating tag.
- Environment configuration is supplied by Kubernetes Secrets and ConfigMaps;
  no secret is committed to this repository.
- The retired `myota-test` namespace, the `myota-next-test` GitRepo, and the
  archived repositories' fleet definitions must not be reintroduced.

## 7. Test-driven development

- Write the failing test first, then the smallest implementation that makes it
  pass, then refactor. A commit that adds behaviour without a test for it is
  incomplete.
- Test layers, all runnable locally and in CI:
  - **unit** — pure logic, no I/O;
  - **contract** — service matches `contracts/` OpenAPI/event schemas;
  - **integration** — real Postgres and real NATS (containers), including
    outbox → stream → consumer paths;
  - **end-to-end** — through the gateway against a deployed stack.
- Tests are deterministic and independent: no shared mutable fixtures, no
  ordering dependencies, no reliance on wall-clock sleeps.
- CI gates on the full suite; a red test is never bypassed to ship.

## Working agreement

- Read this file before changing anything, and update it when a rule changes —
  an undocumented exception is a bug.
- Prefer the smallest coherent change; keep unrelated refactors out of the same
  commit.
- Report what was verified, not what was intended: paste the command and its
  result when claiming something works.

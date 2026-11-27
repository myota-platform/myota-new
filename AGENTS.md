# AGENTS.md

Operational rules for anyone — human or agent — working in **myota-new**.

This repository is the single home for MyOTA development. The previous
repositories (`myota-next`, `myota-platform`, `myota`, `myota-deploy`, and the
per-service repositories) are archived and read-only. Do not open issues, PRs,
or commits against them.

The rules below are not preferences. A change that violates one should be
rejected in review regardless of how convenient the shortcut is.

Read [docs/architecture/](docs/architecture/README.md) before making structural
changes. The reasoning behind each rule is recorded there and in
[docs/decisions/](docs/decisions/).

---

## 1. API First

Every service is defined by its HTTP contract before it is implemented.

- The contract is an OpenAPI 3.1 document committed under `contracts/`, and it is
  the source of truth — not the handlers.
- **The gateway is the only holder of the published contract set.** It embeds the
  composed document and serves:
  - `GET /openapi.json` — the composed contract
  - `GET /docs` — an interactive playground with authorisation and try-it-out

  No other service publishes its contract externally. Each service exposes
  `GET /internal/openapi.json` for build-time composition and contract tests only.
- Handlers are written against generated types/interfaces from the contract.
- Contract tests compare the live service's `/internal/openapi.json` and its
  responses to the committed document. Drift fails CI.
- REST conventions: plural resource nouns, HTTP verbs for semantics, stable
  opaque identifiers, RFC 9457 `application/problem+json` for errors, cursor
  pagination (`{ items, nextCursor }`, never a bare array). Resource paths are
  nouns; a **state transition** may be a sub-resource action
  (`POST /v1/imports/{id}/complete`, `/promote`) when it is not expressible as a
  field update. Do not invent `/getFoo` or `/doBar`-style endpoints.
- Every mutating operation documents and honours `Idempotency-Key`.

## 2. Microservices

**A service is defined by the database it exclusively owns.** This is the single
most important rule in this repository.

| Service | Database | Modes |
| --- | --- | --- |
| `core` | `myota_core` | api · worker · relay |
| `geodata` | `myota_geo` (PostGIS) | api · worker · relay |
| `activity` | `myota_activity` | api · worker · relay |

- One deployable image per service, run in three modes that share one codebase,
  one migration set, and one domain library. Do not create a separate application
  for a worker loop.
- A service owns its schema. No other service connects to its database, and its
  deployment mounts exactly one database secret.
- **Identity and Programmes live together in `core`** because programme
  configuration *is* authorization input: `account_role` and `approver_scope` are
  scoped to a programme. One service makes the check a transactional join instead
  of a cross-service round trip.
- Cross-service references are opaque UUIDs. **No cross-database foreign keys, no
  distributed transactions.**
- Each service carries its own `/healthz`, `/readyz`, `/metrics`, migrations, and
  deployment manifest. One service deploys, scales, and rolls back alone.

## 3. Communication between services via pubsub

Pub/sub semantics, **no message broker**. There is no NATS, no Redis, no
RabbitMQ, no Kafka. The durable log is the outbox; consumers pull from it.

- **Publish after commit only.** The event is written to the owning database's
  `ops.outbox_event` in the same transaction as the domain change. A relay
  publishes it afterwards. Nothing is acked before the domain transaction commits.
- **Consumers pull.** A service exposes `GET /internal/events?after=<seq>`; a
  consuming service pulls with a durable cursor in its own
  `ops.consumer_checkpoint` and converts events into `ops.job` rows. The producer
  never pushes.
- **The job id is the event id**, and the insert is `ON CONFLICT DO NOTHING`.
  That single rule is the entire de-duplication story: at-least-once delivery
  with idempotent application.
- **Ordering is per subject.** `partition = hash(subject) % PARTITIONS`; each
  partition is consumed in `seq` order. Cross-aggregate ordering is not promised
  and nothing depends on it.
- **Retention has two rails**: a 14-day ceiling and an ack floor reported by live
  consumers. The producer never trims past a consumer that still needs events.
  Lag is a metric (`event_lag_seconds`), not a queue depth.
- Event payloads are versioned schemas under `contracts/events/` (CloudEvents 1.0
  subset: `id`, `type`, `source`, `subject`, `time`, `datacontenttype`, `data`).
  Schema changes get the same review as API changes.
- **Synchronous HTTP between services is not used to compose a user response.**
  A user request is served by exactly one service; if a screen needs two domains,
  the front end makes two calls.
- Three narrow synchronous calls are allowed because they are authorization or
  integrity checks, not composition. Each must have a 2 s timeout, a documented
  fallback, and be listed here:
  1. `geodata`/`activity` → `core` for scope introspection (cached per session).
  2. `activity` → `geodata` for entity geometry when closing an activation.
  3. `geodata` → `activity` for deletion impact before retiring an entity.
  A chain of more than one synchronous hop is forbidden.

Adding a broker back requires an ADR. If event volume passes ~10⁶/day, or a
consumer must survive a multi-day outage without a resync, that ADR is justified —
Redis Streams first. See [ADR-0004](docs/decisions/0004-drop-message-broker.md)
and [messaging.md](docs/architecture/messaging.md).

## 4. Job persistence external

- No job state lives only in process memory.
- Work that must survive a restart is persisted before it is acked: an
  `ops.job` row in the owning database, then a worker claim. There is no queue
  server; `FOR UPDATE SKIP LOCKED` is the claim.
- One job table with a `kind` discriminator: `import.preprocess`, `import.fetch`,
  `entity.enrich`, `entity.delete`, `notification.send`, `award.evaluate`,
  `account.retention`. Do not add a per-feature queue table.
- Workers are idempotent and safe to run concurrently. A job carries its own
  identifier, attempt count, lease, and status.
- Restarting or rescheduling any pod must never lose or duplicate an effect.
- Long-running and scheduled work uses the same persisted path — never an
  in-process timer or an unlogged `setTimeout`/cron loop inside an API container.

## 5. Monorepo

One repository, one review surface, one commit history:

```
apps/            deployable services (core, geodata, activity, gateway)
contracts/       OpenAPI documents and event schemas (source of truth)
libs/            shared libraries — ops, ui-tokens, limits
web/             admin and participant front ends
deploy/          Helm charts, Fleet definitions, SOPS secret files
docs/            architecture, decisions, UI mockups, runbooks
tools/           repo scripts and code generation
```

- Shared code goes in `libs/` and is consumed by workspace reference.
- `libs/ops` holds the outbox, idempotency, consumer, and dead-letter
  implementation — **once**, parameterised by database URL and owner name. Do not
  reimplement these in a service.
- A library must not reach into a service's database or bypass the contract.
- `libs/limits` holds numeric limits (upload size, batch size, page size) that are
  imported by both the contract generator and the validators, so the contract
  cannot disagree with enforcement.

## 6. Deployment on this local K3s cluster

- Target: the local K3s cluster, Fleet workspace **`myota`**, Kubernetes namespace
  **`myota`**.
- Fleet definitions live in `deploy/fleet/myota/` and are tracked by a `GitRepo`
  named `myota` in the `myota` workspace. Deploys are GitOps: the cluster
  converges to what is committed; nobody applies manifests by hand to ship.
- Images are `ghcr.io/myota-platform/myota-new-<service>` and are referenced by
  immutable digest in values, never by a floating tag.
- Six deployable units — `gateway`, `core`, `geodata`, `activity`, `admin-web`,
  `participant-web` — running as twelve workloads: each domain service is
  `-api`, `-worker`, and `-relay` from one image, plus the gateway and the two
  front ends. Replica ranges are in `docs/architecture/README.md`.

### Secrets — Sealed Secrets is prohibited

**Do not use Sealed Secrets.** No Myota secret may be a `SealedSecret`, and no
workload may depend on the sealed-secrets controller.

- Secrets are SOPS-encrypted files under `deploy/myota/secrets/*.sops.yaml`,
  decrypted by an operator workflow (`sops --decrypt … | kubectl apply -f -`).
- Encrypted files stay **outside** Fleet's reconciled paths. An automated sync
  that can decrypt secrets is an automated sync that can leak them.
- Non-secret configuration is a ConfigMap, reconciled by Fleet and reviewable in
  the pull request that changes it.
- One secret per service. A service mounts only its own. No duplicated
  `registry-credentials` per namespace — there is one namespace, and images are
  public.
- Never commit plaintext credentials, private keys, age private keys, or bearer
  tokens. Enforced by `gitleaks` in pre-commit and CI, not by discipline.

See [ADR-0003](docs/decisions/0003-no-sealed-secrets.md) and
[docs/architecture/secrets-and-config.md](docs/architecture/secrets-and-config.md).

## 7. Separate admin and participant surfaces

Two front ends, two login surfaces, one identity store.

- **Participant:** `POST /v1/auth/login` · audience `participant` · cookie
  `__Host-myota_sid` · MFA optional.
- **Admin:** `POST /v1/admin/auth/login` · audience `admin` · cookie
  `__Host-myota_admin_sid` · **MFA required**.

The audience is a property of the session, enforced server-side by the gateway
against the route class. A participant session presented to an admin route is
rejected because the audience does not match — not because of a role check that
could be misconfigured. The reverse is also true.

Do not "simplify" this into one cookie with a role check. Do not path-scope the
cookies and call it separation: path scoping is enforced by the browser, and a
non-browser client ignores it.

See [docs/architecture/auth-model.md](docs/architecture/auth-model.md).

## 8. Test-driven development

- Write the failing test first, then the smallest implementation that makes it
  pass, then refactor. A commit that adds behaviour without a test is incomplete.
- Test layers, all runnable locally and in CI:
  - **unit** — pure logic, no I/O;
  - **contract** — service matches `contracts/` OpenAPI and event schemas;
  - **integration** — real Postgres (a container, migrated from the same
    migrations as production), including outbox → pull → job → handler paths;
  - **multi-replica** — the scaling contract: run two or more replicas of the
    component under test, kill one mid-flight, assert no lost or duplicated work
    (see [scaling.md](docs/architecture/scaling.md), Validation);
  - **end-to-end** — through the gateway against a deployed stack.
- Tests are deterministic and independent: no shared mutable fixtures, no
  ordering dependencies, no reliance on wall-clock sleeps.
- CI gates on the full suite; a red test is never bypassed to ship.

## 9. Horizontal scaling is a correctness property

**Any replica must serve any request at any moment, and killing any replica at
any moment must be invisible.** This applies to every workload, including both
front ends. A change that introduces local state, session affinity, or a
per-replica timer is a bug, not a trade-off.

The rules that make it true:

| Concern | Rule |
| --- | --- |
| Session state | In Postgres. Never in process memory, never in a cookie the server cannot verify. |
| Affinity | None anywhere. Do not configure `sessionAffinity` on a Service. |
| Migrations | A pre-deploy `Job` under an advisory lock. A replica never migrates; it verifies the schema version and refuses to start if behind. |
| Scheduled work | `ops.job` with `UNIQUE (kind, scheduled_for)`. Never a timer in a container. |
| Outbox relay | Partition-leased: claim a partition with `FOR UPDATE SKIP LOCKED`, publish that partition in `seq` order. Up to `PARTITIONS` relay replicas. Never "one leader". |
| Event puller | Same partition lease. Duplicates are harmless because the job id is the event id. |
| Rate limiting | Traefik middleware at the single ingress. **Never in the gateway** — in-process counters multiply the limit by the replica count. |
| Uploads | Presigned S3 URLs. File bytes must never traverse an API replica. |
| Caches | Local, TTL-bounded, never authoritative. No cross-replica invalidation protocol. |
| Real-time | Polling with a cursor or `ETag`. No WebSocket, no SSE, no connection pinned to a replica. |
| DB connections | PgBouncer in transaction mode per database. Application pools are bounded per replica, not per replica count. |

Front ends are static trees: content-hashed asset filenames, `immutable` cache
headers on `/assets/*`, `no-cache` on `index.html`, no server-side rendering, no
database client. They scale on network throughput alone.

Before you declare a change done, answer: *what happens if I run three of these?*
If the answer is "the limit triples", "we migrate three times", "the job runs
three times", or "one replica has the state", the change is incomplete.

Full contract: [scaling.md](docs/architecture/scaling.md),
[ADR-0005](docs/decisions/0005-horizontal-scaling.md).

---

## Findings from rebuilding this design

These are the concrete problems found in the previous implementation and the
rules that prevent them recurring. The full ledger is
[docs/architecture/consolidation.md](docs/architecture/consolidation.md).

**Twelve applications became four backend codebases.** `activity-notifications`,
`email-worker`, `identity-maintenance`, `geodata-importer`, and `outbox-worker`
each existed to run one loop. Each is now a job kind or a run mode of an
existing service. Pod count is roughly unchanged — what disappears is twelve
CI pipelines and the duplicated outbox/retry/dead-letter code in each.

**Publishing required a broker that duplicated the outbox.** The relay was
already scoped correctly — one instance group per database — but it existed only
to move rows from `ops.outbox_event` into NATS. That is a second durable log in
front of the first, and publication stalled when the broker was down even though
the event was committed. The broker is gone; the relay now writes nothing, and
consumers pull from the log.

**Notifications existed in two databases.** Core had `email_delivery` +
`email_delivery_attempt` + `email_dead_letter` + `email_dead_letter_redrive`;
Activity had `activity_notification` + `activity_notification_redrive`. One
responsibility, two ledgers, two redrive tools. Now: one ledger in `core`.

**Programmes existed twice** — `core.programme` (uuid) and
`core.programme_record` (text), plus `entity_type` in both core and geo. One
concept, three tables. Now: one programme table, one entity-type table, in `core`.

**Legacy monolith tables were never dropped.** `myota_core` still contained
`activation`, `qso`, `award`, and `award_rule` from before the service split.
Those belong to `activity`. A fresh baseline prevents this.

**Two import states had no writer.** `UPLOAD_PENDING` and
`COMPLETED_WITH_ERRORS` were declared in the type and could only be reached by a
direct database update. Errors are a count plus error rows, not a state.

**A message broker duplicated the outbox.** NATS JetStream sat in front of a
durable log the system already had. It cost a stateful server, a PVC, ~250 lines
of client glue, and five restart-recovery test suites — for guarantees the outbox
plus an idempotent job insert already provided. Removed. The one thing it uniquely
gave (decoupling producer availability from consumer progress) is stated openly in
[ADR-0004](docs/decisions/0004-drop-message-broker.md), along with the triggers
that would justify bringing a broker back.

**Three things could not scale past one replica.** The relay was "exactly one
leader per database" — a throughput ceiling and a publication gap on restart. The
gateway rate-limited in memory, so N replicas meant N× the limit. Uploads were
proxied through the API, making its memory proportional to file size. All three
are fixed by rule 9, and the connection wall (600 connections needed at 20
replicas, 100 available) is closed by PgBouncer.

**The contract was declared but never served.** One `openapi.yaml` existed and
nothing exposed it: the gateway had no `/docs` or `/openapi.json` route, and there
was no Swagger UI anywhere in the deployment. API-first was enforced by a script
that compared routes to the file, not by an interface anyone could call. The
gateway now embeds and serves the composed contract with a playground.

**The upload limit contradicted itself.** The contract said 20 MiB; the code
defaulted to 1 GiB. Numeric limits now live in `libs/limits` and are imported by
both the contract generator and the validator.

**Conflation endpoints required only a bearer token**, not the `geodata.review`
scope they documented. Authorization is checked in one place in `core` and
re-exposed by introspection; services do not re-implement role logic.

**A 1 530-row Unicode case-folding table was shipped as data.** Replaced by
`citext` plus a normalised email key column.

---

## Working agreement

- Read this file and the architecture docs before changing anything, and update
  them when a rule changes — an undocumented exception is a bug.
- Prefer the smallest coherent change; keep unrelated refactors out of the same
  commit.
- Report what was verified, not what was intended: paste the command and its
  result when claiming something works.
- When a rule needs to be broken, that is an ADR, not a commit message.

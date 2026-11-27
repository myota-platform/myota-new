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
  pagination (`{ items, nextCursor }`, never a bare array), no verbs in paths.
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

- Asynchronous integration goes through NATS JetStream. The broker is the
  integration fabric, not an implementation detail of one service.
- **Synchronous HTTP between services is not used to compose a user response.**
  The gateway fans out only when one screen genuinely needs two domains, and it
  calls at most two services.
- Event payloads are versioned schemas under `contracts/events/`
  (CloudEvents 1.0 subset: `id`, `type`, `source`, `subject`, `time`,
  `datacontenttype`, `data`). Schema changes get the same review as API changes.
- Publishers write to a durable stream; consumers use durable pull consumers with
  explicit ack, a max-delivery bound, and a dead-letter path.
- **At-least-once delivery is assumed.** Every consumer is idempotent by
  `(consumer, event_id)`.
- Publish-after-commit only: the event is written to the owning database's outbox
  in the same transaction as the domain change. The relay publishes after commit.
  Nothing is acked before the domain transaction commits.

## 4. Job persistence external

- No job state lives only in process memory.
- Work that must survive a restart is persisted before it is acked: a `ops.job`
  row in the owning database, then a JetStream message, then a worker claim.
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
  front ends. Scaling is in `docs/architecture/README.md`. The relay runs as
  exactly one leader per database, elected by a Postgres advisory lock.

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
  - **integration** — real Postgres and real NATS (containers), including
    outbox → stream → consumer paths;
  - **end-to-end** — through the gateway against a deployed stack.
- Tests are deterministic and independent: no shared mutable fixtures, no
  ordering dependencies, no reliance on wall-clock sleeps.
- CI gates on the full suite; a red test is never bypassed to ship.

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

**`outbox-worker` held credentials for all three databases** — a boundary
violation disguised as a convenience. The relay is per-database and part of the
owning service.

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

**The contract was declared but never served.** Four `openapi.yaml` files existed
and nothing exposed them — there was no Swagger UI anywhere in the deployment.
The gateway now embeds and serves the composed contract with a playground.

**The upload limit contradicted itself.** The contract said 20 MiB; the code
defaulted to 1 GiB. Numeric limits now live in `libs/limits` and are imported by
both the contract generator and the validator.

**Conflation endpoints required only a bearer token**, not the `geodata.review`
scope they documented. Authorization is checked in one place in `core` and
re-exposed by introspection; services do not re-implement role logic.

**A 1 540-row Unicode case-folding table was shipped as data.** Replaced by
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

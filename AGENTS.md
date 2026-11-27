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
- **The gateway verifies the session token, and refuses when it cannot.**
  `core` publishes its Ed25519 public keys as a JWKS at
  `GET /internal/auth/jwks`; the gateway caches them by `kid` and refetches on an
  unknown one, so a key rotation needs no gateway redeploy. With no usable key the
  gateway answers 401 on `/v1/admin/**` rather than trusting an unverified `aud`
  claim. See [ADR-0008](docs/decisions/0008-gateway-token-verification-keys.md).
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
- **The contract is composed, never hand-edited.**
  `python3 tools/compose_contracts.py --check` reads the three service documents
  and `contracts/shared/components.yaml`, writes the composed document in memory,
  validates it as OpenAPI 3.1, and fails when the committed
  `contracts/public.openapi.yaml` is stale. It also fails when an `operationId`, a
  path, a tag, or a routed prefix is claimed by two services — duplication that a
  reviewer would otherwise have to notice.
- **Every published prefix has exactly one owning service.**
  `python3 tools/compose_contracts.py --routes` prints the table the gateway
  routes on; F3 generates its route list from that output rather than keeping a
  second copy. A route that belongs to nobody is `404`, never a default upstream.
- Shared types live in `contracts/shared/components.yaml` and are `$ref`ed, never
  copied into a service document. Two copies cannot be caught by any uniqueness
  check, and they drift the first time one is edited.
- Event payloads have a JSON Schema per type under `contracts/events/`, listed in
  `contracts/events/index.json`. `tools/check_events.py` fails when a type named in
  `messaging.md` has no schema, when the index and the directory disagree, or when
  a schema's own example does not validate against it. Adding an event type is
  three edits in one commit: the document, the schema, the index.

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

See [ADR-0001](docs/decisions/0001-service-boundaries-follow-databases.md).

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

Adding a broker back requires an ADR, and so does adding any pub/sub library.
"Simple pub/sub" is not a lighter version of this: fire-and-forget transport (Redis
Pub/Sub, `LISTEN`/`NOTIFY`) loses every message published during a restart and
cannot distribute work across replicas, because every subscriber receives every
message. The trigger for that ADR is a specific guarantee Postgres stopped
providing — see the trigger table in
[ADR-0004](docs/decisions/0004-drop-message-broker.md#reconsideration-triggers)
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
libs/            shared libraries — ops, limits
web/             admin and participant front ends
deploy/          Helm charts, Fleet definitions, SOPS secret files
docs/            architecture, decisions, UI screens, runbooks
tools/           repo scripts: check_diagrams.py, contract composition
```

- Shared code goes in `libs/` and is consumed by workspace reference.
- `libs/ops` holds the outbox, idempotency, job-claim and dead-letter
  implementation, parameterised by database URL and owner name. Do not reimplement
  these in a service.
  **Polyglot means it exists twice** — `libs/ops/go` and `libs/ops/rust` — because
  a Go service cannot link a Rust crate. That is duplication polyglot reintroduced,
  and it is paid for with a shared conformance suite in `libs/ops/conformance/`
  that both implementations must pass. Add a fixture there before fixing one side
  only; the suite is what makes the second change non-optional.
- A library must not reach into a service's database or bypass the contract.
- `libs/limits` holds numeric limits (upload size, batch size, page size) that are
  imported by both the contract generator and the validators, so the contract
  cannot disagree with enforcement.

### CI is per-component, and sparse

A monorepo with one pipeline is a monorepo where a typo in a README rebuilds six
images. Each component has its own workflow with its own `paths` filter:

| Workflow | Paths it owns |
| --- | --- |
| `gateway.yml` | `apps/gateway/**`, `contracts/**`, `libs/ops/go/**`, `libs/limits/go/**` |
| `core.yml` | `apps/core/**`, `contracts/**`, `libs/ops/rust/**`, `libs/limits/rust/**`, `deploy/myota/db/core/**` |
| `geodata.yml` | `apps/geodata/**`, `contracts/**`, `libs/ops/rust/**`, `libs/limits/rust/**`, `deploy/myota/db/geo/**` |
| `activity.yml` | `apps/activity/**`, `contracts/**`, `libs/ops/go/**`, `libs/limits/go/**`, `deploy/myota/db/activity/**` |
| `admin-web.yml` | `web/admin/**`, `libs/ui-tokens/**` |
| `participant-web.yml` | `web/participant/**`, `libs/ui-tokens/**` |
| `docs.yml` | `docs/**`, `tools/**`, `contracts/**`, `AGENTS.md`, `README.md` |
| `smoke-test.yml` | `deploy/fleet/myota-test/**`, manual |

- **A new component gets a new caller workflow**, not an entry in a matrix. A
  matrix gives every component the union of all paths, which is precisely the
  behaviour being avoided.
- **Shared build logic goes in a reusable workflow** (`_service-go.yml`,
  `_service-rust.yml`, `_web.yml`) called with `uses:`. The caller stays ~30 lines
  and holds only its trigger paths.
- **A component lists only its real dependencies.** `libs/ops/go` feeds the Go
  services; `libs/ops/rust` feeds the Rust ones. A shared-library change rebuilding
  everything means a path filter is too broad.
- **`deploy/**` is in no component's paths.** That is why the digest-bump commit CI
  makes cannot retrigger CI.
- **Pin actions to a major tag, and keep the major current.** Every action here is
  at its latest major; `docs.yml` is the place to notice when one falls behind.
- **CI holds no cluster credential.** Delivery is a commit that Fleet reconciles;
  `smoke-test.yml` verifies the result over public HTTPS. See
  [ci-secrets.md](docs/architecture/ci-secrets.md).

## 6. Technology

**Polyglot, per service, chosen for the shape of its work.** Accepted in
[ADR-0002](docs/decisions/0002-language-per-service.md) — this is the decision, not
a recommendation awaiting one.

**Go everywhere is the escape hatch**, and reverting to it is a new ADR, not a
quiet drift.

| Service | Language | Framework | Why |
| --- | --- | --- | --- |
| `gateway` | Go 1.24 | Chi | I/O multiplexing; contracts in `embed.FS` |
| `core` | Rust 2024 | Axum + SQLx | compile-time-checked SQL on the credential path |
| `geodata` | Rust 2024 | Axum + SQLx + geozero | CPU-bound geometry, no GC |
| `activity` | Go 1.24 | Echo + pgx | throughput, worker pools |
| `admin-web` | TypeScript | Vue 3 + Vite | dense tables and forms |
| `participant-web` | TypeScript | Svelte 5 + Vite | smallest runtime on a phone |

If polyglot proves more expensive than it is worth, Go everywhere is the fallback
(Chi for the gateway, Echo for the domain services, `sqlc` for queries). Choosing
it means superseding ADR-0002.

- **A service may leave the default language only for a measured reason** — a
  latency budget it cannot meet, a memory ceiling it cannot stay under, or a
  correctness property it cannot express. Preference is not a reason, and the
  reason goes in an ADR.
- **Front ends are static builds.** No server-side runtime in production, because
  that would turn a static artifact into a stateful service with a database
  connection for no product benefit.
- **One build image per language, pinned by digest.** No `latest`, anywhere.

## 7. The UI style is 04‑Organic

**`docs/ui/styles/04-organic/` is the preferred and only UI direction.** It is
not one option among four — the other three were samples that informed the choice
and are now historical. See
[ADR-0007](docs/decisions/0007-ui-direction-organic.md).

```
docs/ui/styles/04-organic/
  style.css                     the design system — tokens, components, dark variant
  admin-01…06-*.html            every admin screen
  participant-01…06-*.html      every participant screen
  renders/                      generated PNGs, committed so they are viewable
```

### Rules

- **New UI work starts from `04-organic/style.css`.** Do not extend the baseline
  in `docs/ui/`, and do not revive `01-editorial`, `02-brutalist` or `03-aurora`.
  They stay in the repository as the record of what was considered, not as a menu.
- **The design system is the stylesheet, not a component library.** The custom
  properties at the top of `style.css` are the contract — colour, radius, shadow,
  spacing. A new component uses those tokens and never invents a literal value;
  if a needed value is missing from the scale, add a token.
- **The visual grammar, stated so it can be checked:**
  - **no container borders** — cards, panels and controls separate by surface and
    radius, and sit apart by shadow. A 1 px `var(--line)` hairline is allowed
    *inside* a surface as a row divider; it is never the outline of a container
  - **warm palette** — sand, terracotta, sage, ochre carry the chrome. Cool hues
    appear only where they *describe a thing* — the sea on a map, an informational
    state — never as surface, border or accent for the interface itself
  - **generous leading and whitespace** — density is the enemy
  - **radius carries hierarchy, and surfaces use tokens** — never square corners;
    `--r-ctl` 14 px for controls and inputs, `--r-lg` 28 px for cards, `--r-xl`
    34 px for the page shell. A literal radius is acceptable only on a decorative
    micro-element with no hierarchical meaning (a 2 px hairline, a bar cap, a
    30 px avatar); it must never appear on a surface, card, control or panel
- **Colour carries meaning, not position.** `.tint-good`, `.tint-warn`,
  `.chip-status.s-ok` and friends are chosen per metric. Never tint by
  `nth-child`: an arbitrary red KPI reads as an error state.
- **Both colour schemes are required.** Every screen must work in
  `prefers-color-scheme: dark`. The dark variant is warm charcoal, not blue-black,
  so the palette still reads as sand-and-clay at night.
- **Every screen must survive 390 px.** Below 560 px, wide tables become stacked
  blocks rather than sideways-scrolling tables, and the nav wraps rather than
  scrolling out of view. A hidden state column or a hidden current page is a bug.
- **Add every new screen to `SHOTS` in `tools/render_mockups.mjs`,** or its image
  will not exist and `--check` will not notice.

### Verifying

```sh
node tools/render_mockups.mjs          # regenerate the images
node tools/render_mockups.mjs --check  # fails if an image is stale
node tools/check_ui.mjs                # every page x 3 viewports x 2 schemes
```

The render check is **byte-exact**, so it is pinned to the Chromium that Playwright
resolves through `package-lock.json`. Two Chromium builds render the same page
identically most of the time and not always — upgrading Playwright means
regenerating all renders in the same commit.

`check_ui.mjs` is the gate that matters. For **every** page under `docs/ui` it
asserts, at 390 / 768 / 1440 px, in light and dark:

- no JavaScript errors
- no external network requests — the screens are self-contained
- no horizontal page overflow
- **no clipped content**

That last one is why the tool exists rather than a manual look. A card with
`overflow: hidden` swallows a too-wide table without ever making the page scroll,
so an overflow check alone reports success while the user sees a truncated
column. This is not hypothetical: it happened, and the overflow check passed.
`sr-only` text and decorative absolutely-positioned pseudo-elements are excluded,
because clipping those is the point.

Both tools need Playwright with a Chromium build. `PLAYWRIGHT_CHROMIUM` forces a
specific binary; otherwise an already-downloaded build is discovered.

## 8. Deployment on this local K3s cluster

- Target: the local K3s cluster, Fleet workspace **`myota`**, in one of two
  namespaces:

  | Environment | Namespace | Fleet bundle | Participant host | Admin host |
  | --- | --- | --- | --- | --- |
  | **test** | `myota-test` | `deploy/fleet/myota-test/` | `myota-test.ea7klk.es` | `myota-admin-test.ea7klk.es` |
  | production | `myota` | `deploy/fleet/myota/` | not yet | not yet |

  Both live in the `myota` workspace. The workspace was decommissioned on
  2026-11-27 and is recreated by chunk **F6**.
- **Every chunk deploys to test as soon as it is verifiable** — `myota-test`
  exists before the first feature lands so there is never an excuse to skip it.
  Production is not a target until the test environment is whole.
- Fleet definitions live under `deploy/fleet/<environment>/` and are tracked by a
  `GitRepo` of the same name in the `myota` workspace. Deploys are GitOps: the
  cluster converges to what is committed; nobody applies manifests by hand to
  ship. A `GitRepo` uses `clientSecretName: github-auth` from the workspace.
- Charts and values live in `deploy/myota/`; secrets in
  `deploy/myota/secrets/*.sops.yaml`; configuration in `deploy/myota/config/`.
- Hostnames follow the existing cluster convention — `aprs.ea7klk.es` and
  `aprs-admin.ea7klk.es` — so participant and admin are always a pair, never one
  host with two audiences behind a path.
- `IngressRoute` uses `entryPoints: [websecure]` and
  `tls.certResolver: acmeresolver`. There is no cert-manager and no external-dns
  on this cluster; DNS for both test hostnames already resolves to it.
- Images are `ghcr.io/myota-platform/myota-new-<service>`, **public**, referenced
  by immutable digest in values, never by a floating tag. The repository is public
  too: nothing here requires an account to read, clone or pull.
- Six deployable units — `gateway`, `core`, `geodata`, `activity`, `admin-web`,
  `participant-web` — running as twelve workloads: each domain service is
  `-api`, `-worker`, and `-relay` from one image, plus the gateway and the two
  front ends. Replica ranges are in `docs/architecture/README.md`.

### Configuration and secrets are separate stores

**Keep them apart. They have different lifecycles, different reviewers, and
different blast radii, and mixing them destroys both.**

| | Configuration | Secrets |
| --- | --- | --- |
| Files | `deploy/myota/config/*.yaml` — **plaintext** | `deploy/myota/secrets/*.sops.yaml` — encrypted |
| Applied by | **Fleet, automatically** | an operator or pipeline step, deliberately |
| Change is | a reviewable pull request | resealed ciphertext |
| Contains | behaviour: log level, TTLs, endpoints, switches, replica counts | credentials only |

- **Configuration goes through Fleet.** That is the intended path and the right
  default: a change is a diff, a rollback is a revert, and nothing is applied by
  hand.
- **Secrets never go through Fleet.** Encrypted files stay outside the reconciled
  paths. An automated sync that can decrypt is an automated sync that can leak.
- **A DSN goes in the secret store** because it embeds a password. Host, port and
  database name are configuration. If splitting one value across two stores feels
  awkward, use discrete variables instead of one URL — better practice anyway.
- **No key may exist in both** a service's ConfigMap and its Secret. `envFrom`
  resolves the later entry first, so an overlap is a silent precedence bug. CI
  parses both and fails on intersection.
- **A service mounts its own secrets only.** No shared database credentials, no
  duplicated `registry-credentials` per namespace — there is one namespace, and
  images are public.
- **The endpoint is configuration; the credential is a secret.** A value belongs in
  the secret store only when it grants access to something.

### CI credentials are a third store

**GitHub Actions holds credentials for the pipeline, never for the cluster.**

- CI must need **zero stored secrets** to build, test and publish: `GITHUB_TOKEN`
  with `packages: write` covers GHCR, and a deploy is a git commit that Fleet
  reconciles. A workflow that needs a cluster credential means the deploy design
  is wrong.
- When a stored secret is genuinely needed (a real SMTP account for integration
  tests, a module proxy token), put it in a **GitHub environment**, not at
  repository level, and set it with `gh secret set` reading from stdin.
- Never `echo` a secret, pass one as an argument, or write one to the workspace.
- Never use `pull_request_target` with a checkout of the PR head — that gives
  untrusted code the secrets. Fork PRs get none, and that default stays.
- Pin third-party actions to a commit SHA: a mutable tag is a supply-chain path
  into whatever the job can read.

### Never in the repository

Plaintext credentials, private keys, age private keys, bearer tokens, a credential
in a ConfigMap, a secret in a workflow file. Enforced by `gitleaks` in pre-commit
and CI, not by discipline.

See [ADR-0003](docs/decisions/0003-no-sealed-secrets.md),
[docs/architecture/secrets-and-config.md](docs/architecture/secrets-and-config.md),
and [docs/architecture/ci-secrets.md](docs/architecture/ci-secrets.md).

## 9. Separate admin and participant surfaces

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

### Two user-administration sections, never one

The console has **two** account-management areas, because they fail differently:

| | Admins | Participants |
| --- | --- | --- |
| Scope | `identity.admins.manage` | `identity.participants.manage` |
| Route | `/v1/admin/identity/admins` | `/v1/admin/identity/participants` |
| Risk | privilege escalation | support error at volume |
| Actor MFA | required, re-auth ≤ 5 min | not required |
| Role grants | yes | **no** — by construction |

- **Do not merge them into one "Users" screen.** A merged list gives an operator
  a single action surface for two activities with different failure modes, and
  makes the escalation path indistinguishable from the support path.
- **You cannot grant a role you do not hold.** Check `granted ⊆ actor_scopes` at
  write time; "has the manage scope" alone is equivalent to root.
- **You cannot remove your own last admin role.**
- **Callsign verification lives only in the participant section.** It is a support
  action, not an authority grant.
- **Deactivation is reversible; hard deletion is a retention job**, never a button
  in either section.

See [docs/architecture/auth-model.md](docs/architecture/auth-model.md).

## 10. Test-driven development

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

## 11. Email flows are security-critical, not plumbing

Every account-lifecycle flow lives in `core`. There is one notification ledger,
one delivery worker, one SMTP credential.

| Flow | Token | Lifetime | Ends with |
| --- | --- | --- | --- |
| Registration → activation | `verify_email` | 24 h | `pending` → `active` |
| Password recovery | `reset_password` | 30 min | new password, all sessions revoked |
| Email change | `change_email` | 2 h | address changed, old address notified |

Rules that must not be relaxed:

- **One token table, one purpose per row.** `verification_token.purpose ∈
  (verify_email, reset_password, change_email)`. A token presented to the wrong
  endpoint is **rejected**, never reinterpreted. Without this, every welcome email
  is an account takeover.
- **Store the hash, never the token.** The raw value exists only in the email.
  A database dump must not allow a password reset.
- **Re-check `email` on use.** If the address changed after issue, the token no
  longer applies — this is what stops an attacker who controls the old address.
- **Single use, and issuing invalidates prior tokens** for the same
  `(account_id, purpose)`. `WHERE used_at IS NULL` so a race cannot redeem twice.
- **No enumeration.** Registration and recovery-request must return identical
  status, body and **timing** whether or not the address exists. Do the token
  write on the miss path too, or the fast path is detectable with a stopwatch.
- **Email is never sent inline.** Write a `notification` row in the domain
  transaction; `core-worker` delivers with bounded retries (8 attempts, then
  `dead`). The token and the email commit together.
- **`template` is an allowlisted id**, never caller-supplied text. Otherwise the
  notification system is an open relay.
- **Password reset revokes every session.** A reset means the password was lost or
  compromised; leaving an attacker's session alive defeats it.
- **Email change requires the current password, notifies the old address with a
  one-click cancel, and does not take effect until the new address confirms.**
- **Deliverability is an availability concern.** SPF, DKIM and DMARC on a dedicated
  sending subdomain. If password resets land in spam, users cannot recover
  accounts.

See [docs/architecture/email-flows.md](docs/architecture/email-flows.md).

## 12. Horizontal scaling is a correctness property

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
| Object storage | Three buckets per environment on the cluster's Silo server; one credential per environment, never per service. See section 16. |
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

## 13. Diagrams are part of the change, not documentation debt

The system is described twice, deliberately:

| Representation | File | Authority |
| --- | --- | --- |
| **C4 model** | `docs/architecture/structurizr/workspace.dsl` | **Authoritative.** People, containers, external systems, relationships, deployment. |
| **Mermaid blocks** | inside `docs/**.md` | A projection, for reading in a Markdown file. Rendered by GitHub, no build step. |

**On any decision, change, or implementation that alters what the system is, update
both in the same commit.**

That includes, and is not limited to: adding, merging, splitting, or removing a
service or container; changing a technology choice; adding or removing a database,
table, or relationship; changing a synchronous or asynchronous path; changing
authentication or the audience model; changing the deployment shape, replica
ranges, or a scaling mechanism; adding or removing an external system.

A commit that changes an architectural fact and leaves either representation stale
is **incomplete**, regardless of whether the code works.

### Which diagram to update

| The change touches | Update |
| --- | --- |
| A service, its technology, or its dependencies | `workspace.dsl` containers + `docs/architecture/README.md` flowchart |
| A table, column, or ownership boundary | `workspace.dsl` (if a container changed) + the relevant `erDiagram` in `data-model.md` |
| A synchronous or asynchronous path | `workspace.dsl` relationships + `messaging.md` sequence |
| An event type or delivery rule | `messaging.md` and `contracts/events/` |
| Import stages, states, or the promotion flow | `geodata-imports.md` state and sequence diagrams |
| Login, session, or audience handling | `auth-model.md` sequence |
| Replica ranges, leases, pooling, or autoscaling | `scaling.md` flowchart and tables |
| The deployment environment, namespace, or Fleet workspace | `workspace.dsl` deployment view |

### Rules

- **The DSL is authoritative.** If the Mermaid and the model disagree, the model
  is right and the diagram is stale — fix the diagram, not the model, unless the
  model is genuinely wrong.
- **A new view is cheap; a redrawn picture is not.** Add a Structurizr view rather
  than hand-drawing a new diagram.
- **Every dynamic-view step must be a declared relationship.** The checker enforces
  this, and it exists because a flow diagram that invents a connection is a
  diagram that lies.
- **Diagrams are Mermaid blocks, not committed images.** Do not add `.svg` or
  `.png` *diagrams* to `docs/`. GitHub renders the Mermaid; a committed image of a
  diagram is a second copy that drifts.
- **UI mockups are the exception, and the opposite trade.** GitHub shows `.html`
  as source, so the PNGs in `docs/ui/renders/` are the only viewable form. They
  are committed, they are generated, and they must be regenerated whenever the
  markup or the stylesheet changes:

  ```sh
  node tools/render_mockups.mjs          # writes docs/ui/renders/*.png
  node tools/render_mockups.mjs --check  # fails if a render is stale
  ```

  Screens live in `docs/ui/styles/04-organic/` — see rule 7. Adding a screen means
  adding it to `SHOTS` in the render script, or its image will not exist and
  `--check` will not notice.

  A screenshot taken by hand drifts the moment someone edits the CSS. Use the
  script or the images are wrong.
- **Run the check before committing:**

  ```sh
  python3 tools/check_diagrams.py            # local: report gaps, fail on defects
  python3 tools/check_diagrams.py --strict   # CI gate: Mermaid must be rendered
  python3 tools/check_diagrams.py --full     # CI gate: the DSL must compile too
  ```

  Mockup renders have their own gate: `node tools/render_mockups.mjs --check`.

  `--full` compiles `workspace.dsl` with `structurizr-cli`. **Use it** — the
  heuristic fallback cannot see identifier resolution, view-key syntax, or
  environment matching, and a model that passed heuristics has already shipped
  broken once. Requires a Java runtime and the CLI (`STRUCTURIZR_CLI`, or
  `/opt/structurizr/structurizr.sh`, or on `PATH`).
- **Do not claim the DSL is valid without compiling it.** A check that prints
  success for input it never parsed is worse than no check. The summary line says
  which layer ran — "compiled" or "checked heuristically" — and that difference is
  real.

### Structurizr grammar you will trip over

- **Tags are one comma-separated string**, not separate arguments:
  `"External"`, or `"Element,Database"`.
- **`softwareSystem` has no technology field.** Signature is
  `<name> [description] [tags]`. Only `container`, `component`, `deploymentNode`
  and `infrastructureNode` take technology.
- **The DSL is line-oriented.** `{` must end the line; `deployment s "Live" { include * }`
  is a token error.
- **`!identifiers hierarchical`** makes containers inside a software system
  resolvable only as `<system>.<name>` from outside it. Omitted here on purpose.
- **View keys must match `[a-zA-Z0-9_-]`**, and a deployment view's environment
  name doubles as its key.
- **`theme default` fetches over the network.** Omitted so the file validates
  offline.

### Why this is a rule and not a preference

Two representations drift. The drift is invisible in review, silent in production,
and discovered months later by someone who trusted a diagram. Making the update
part of the commit — and the check part of the gate — is the only version of this
that survives contact with a busy week.

The permanent fix is to generate the Mermaid from the DSL, which needs a Java
runtime in CI. Until then the rule is the mechanism.

See [ADR-0006](docs/decisions/0006-diagrams-as-code.md) and
[docs/architecture/structurizr/README.md](docs/architecture/structurizr/README.md).

## 14. Work is tracked in the project, and every step is recorded in the issue

**Source of work:** [project #3](https://github.com/orgs/myota-platform/projects/3),
linked to this repository. Issues are the work items; each is a chunk small
enough to finish in one sitting and written to be executable without further
briefing.

```
Project fields:  MVP = Admin | Participant | Shared
                 Area = Foundation | Core | Geodata | Activity | Gateway | Frontend | Deploy | Contracts
                 Order = 5 … 260, the intended sequence
```

Chunk keys are stable and used in commit messages: `F1…F7` foundation,
`A1…A8` MVP Admin, `P1…P6` MVP Participant, `X…` findings.

### When you pick up an issue

1. **Move it to In Progress** in the project before touching code.
2. **Comment with the plan** — the files you expect to touch and the acceptance
   criteria you will check. This is the record if the approach changes.
3. **Comment after each completed step.** Not a summary at the end: a line per
   step, with the command run and what it returned. If a step failed and you
   changed approach, say so and why — that is the most useful line in the whole
   issue.
4. **Paste the `Verify` output verbatim in the closing comment**, before closing.
   "It works" is not evidence; the command and its result is.
5. **Close with "Closes #N"** in the commit or PR, so the project moves itself.

### When you find something not in the plan

Create an issue — do not fix it silently and do not leave it in a comment.

- Title it with a finding prefix (`X1`, `X2`, …) or a service prefix.
- Label it: `risk` for a hazard, `decision-needed` for something requiring an ADR,
  `bug` for a defect, plus the MVP label it belongs to.
- Add it to the project with the correct `MVP`, `Area`, and an `Order` near the
  work that surfaced it.
- Link it from the issue that found it ("Found while working #N").

A finding that is real but unrecorded is a finding that will be rediscovered.

### Definition of done for a chunk

- The acceptance criteria in the issue are met, each one demonstrably.
- The `Verify` block was run and its output pasted into the issue.
- If the chunk touched architecture, the Mermaid blocks *and* `workspace.dsl` were
  updated in the same commit (rule 13).
- If it changed a decision, an ADR exists or was amended — see the process in
  `docs/decisions/README.md`.
- Testable behaviour arrived with a test first (rule 10).
- Names and comments follow rule 15: a reviewer should not have to ask what a
  variable is for, or why a non-obvious line is there.
- The test environment reflects it: `https://myota-test.ea7klk.es` and
  `https://myota-admin-test.ea7klk.es` still answer 200 on `/healthz`.

### Commit messages

```
A4 · grant roles with the subset rule

Refs #14
```

The chunk key first, because `git log --oneline` then reads as the plan.

## 15. Clean code: names, comments, and the idioms of the language

**Names carry the design.** A name that needs a comment to explain it is the wrong
name. Say what the thing *is for*, not how it is built or where it came from.

```
// wrong — says nothing about purpose
data, handle, process, tmp, item2, svc, doStuff, result

// right — the reader knows what this is without opening the function
pendingImportRuns, callsignEvidenceToReview, verifyCallsign,
retireEntity, outboxBacklogAge, entityId
```

- **Functions say what they do**, in the imperative: `claimNextJob`, not
  `jobHandler`. If a function needs "and" in its name, it does two things.
- **Booleans read as assertions**: `isPublished`, `hasConfirmedSecondFactor`,
  `canGrantScope` — never `flag` or `status2`.
- **Comments explain *why*.** The code already says what it does. Write the reason
  for the choice, the constraint that forced it, or the invariant being relied on —
  never a restatement of the next line.

  ```
  // wrong
  // increment attempts
  attempts++

  // right
  // Bounded at 8 to match the dead-letter threshold in ADR-0004; the alert fires
  // on the same number, so changing one without the other silences the alert.
  attempts++
  ```
- **Comment the surprising, not the obvious.** A non-obvious workaround always
  gets a comment naming the constraint and, where possible, a link to the issue or
  ADR that explains it. Six months from now the "obvious" fix is what breaks it.

### Go

[The Go Proverbs](https://go-proverbs.github.io/) are the style guide. The ones
that actually bite in this codebase:

| Proverb | What it means here |
| --- | --- |
| *Clear is better than clever.* | If a reviewer has to trace it, rewrite it. |
| *A little copying is better than a little dependency.* | Do not add a module to save fifteen lines. |
| *The bigger the interface, the weaker the abstraction.* | One or two methods. Accept interfaces, return structs. |
| *Errors are values. Don't just check errors, handle them gracefully.* | Wrap with context and `%w`; no bare `return err`; no discarded errors. |
| *Make the zero value useful.* | A struct that works before it is populated needs no constructor. |
| *Don't panic.* | `panic` only for programmer error at start-up, never for input. |
| *Gofmt's style is no one's favorite, yet gofmt is everyone's favorite.* | Non-negotiable — CI fails on unformatted code. |

### Rust

- **[The Rust Style Guide](https://doc.rust-lang.org/style-guide/)** — enforced by
  `cargo fmt --check` in CI. Non-negotiable.
- **[The Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)** on
  naming: types are `UpperCamelCase`, functions and variables `snake_case`,
  conversions follow `as_`/`to_`/`into_` by cost.
- **Express invariants in the type system before writing a comment about them.** A
  newtype and a constructor that validates is worth more than a comment asking the
  caller to be careful.
- **`clippy -D warnings` in CI** — a lint is an argument someone already had and
  wrote down. Do not `#[allow]` without a comment saying why.
- **Document `# Errors`** on any public function returning `Result`, and
  `#[must_use]` on anything whose ignored return value would be a bug.
- **No `unwrap()` outside tests and start-up**, where a failure should be loud and
  immediate.

### What CI already enforces

`gofmt`, `go vet`, `cargo fmt --check`, `cargo clippy -- -D warnings` — in the
per-service workflows. The naming and commenting rules above are not machine-checkable,
which is exactly why they are written down: they are review findings, not lint
findings.

---

## 16. Files live in Silo, under documented buckets

Object storage is the cluster's Silo server (a PGSTY/MInIO fork) in namespace
`silo`, shared with other tenants. Myota is one tenant and holds no administrative
access to it.

- **Three buckets per environment**, named `myota-test-{imports,content,records}`
  and `myota-prod-{imports,content,records}`. One user per environment —
  `myota-test`, `myota-prod` — with policy `myota-<env>-only`. Never one bucket,
  never one credential per service. Reasoning: [ADR-0009](docs/decisions/0009-three-buckets-per-environment.md).
- **A service writes only its own prefix**, inside the bucket its data class lives
  in: `geodata` → `imports/` and `entities/`, `activity` → `logs/` and
  `certificates/`, `core` → `evidence/`. The credential is bucket-scoped, so this is a
  code rule, not a policy one: build `object_key` from ids, in the owning service.
- **Object keys are ids, never user input.** `imports/<uploadId>/source`,
  `entities/<entityId>/<attachmentId>`, `certificates/<issuanceId>`,
  `evidence/<accountId>/<callsignId>`. The original filename and the media type live
  in the database. A user-supplied key brings traversal, unicode normalisation
  differences and personal data into a proxy log.
- **Two endpoints, and they are not interchangeable.** `S3_ENDPOINT` is the
  in-cluster ClusterIP, for streaming bytes. `S3_SIGNING_ENDPOINT` is the public
  hostname, and it is the only one that can presign: a signature covers its `Host`,
  and a browser cannot reach `silo.silo.svc.cluster.local`. Signing with the wrong one
  fails as `403 SignatureDoesNotMatch`.
- **Path-style addressing, region `us-east-1`.** Silo sits behind one hostname with no
  wildcard ingress rule, so virtual-host addressing fails at TLS.
- **Never return a credential to a caller.** The only client path is a presigned URL
  for one key, one method, one expiry.
- **The imports bucket expires at 30 days**; the other two do not. Anything added to
  `imports/` is expected to disappear, so never store the only copy of something there.
- `python3 tools/provision_silo.py --check` must pass: it holds the inventory, the
  policies and the document to each other. Adding a bucket means editing all three.

Full specification: [object-storage.md](docs/architecture/object-storage.md).
Provisioning: [deploy/silo/README.md](deploy/silo/README.md).

## 17. Three databases, one pooler each, migrations in a Job

Each service owns exactly one database and no table appears in two of them (ADR-0001).

| Database | Owner | Image | Volume |
| --- | --- | --- | --- |
| `myota_core` | `core` | `postgres:16.4-alpine` | 10 Gi |
| `myota_geo` | `geodata` | `postgis/postgis:16-3.4` | 20 Gi — **the only one with PostGIS** |
| `myota_activity` | `activity` | `postgres:16.4-alpine` | 10 Gi |

- **Every service reaches PostgreSQL through its pooler**, `myota-<db>-pgbouncer:6432`.
  The only direct clients are the pooler itself and the migration Job, and both are
  enforced twice: `tools/check_deploy.py` fails if any ConfigMap a workload reads names a
  `-postgres` host, and a NetworkPolicy per database refuses ingress from anything but its
  pooler, the migration Job, and the node (for kubelet probes).
- **Transaction pooling constrains the application.** No advisory lock held across
  requests, no prepared statement outside a transaction, no `SET` that outlives a
  statement. The endpoint ConfigMap carries `DATABASE_POOL_MODE` so the constraint is
  visible where a driver is configured.
- **The connection budget is fixed at 200 / 25 / 500** — `max_connections` on PostgreSQL,
  `DEFAULT_POOL_SIZE` and `MAX_CLIENT_CONN` on PgBouncer. Changing one means changing
  `tools/check_deploy.py` too, so the change is visible in a diff rather than buried in a
  template.
- **A replica never migrates.** The migration Job runs under a session advisory lock, in
  one transaction, as the **application role** — so the objects the service queries are
  owned by the role that queries them. A replica verifies the schema version and refuses
  to start if behind.
- **Every migration is idempotent.** `IF NOT EXISTS` throughout, so applying the set twice
  changes nothing. Verified by object count, not by exit code.
- **Configuration and credentials stay apart.** The Secret holds `superuser-password` and
  `app-password`; the ConfigMap holds host, port, database and user. There is no
  `DATABASE_URL` with a password in it.
- **`tools/check_db_schema.py` holds the DDL to the document in both directions**: every
  table `data-model.md` names exists, and no baseline invents one it does not describe. It
  also asserts no table appears in two databases, and that PostGIS works on `myota_geo`
  and fails on the other two.
- **Append-only tables carry `created_at` and no `updated_at`**, and must say so in the DDL
  with an `-- append-only: <why>` comment. A table with `updated_at` omits the trigger
  silently otherwise.

Full detail: [deploy/myota/README.md](deploy/myota/README.md),
[data-model.md](docs/architecture/data-model.md).

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
are fixed by rule 12, and the connection wall (600 connections needed at 20
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

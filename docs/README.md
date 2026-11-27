# MyOTA documentation

Start here. Everything below is committed to this repository and intended to be
read in this order.

## Architecture

| Document | What it answers |
| --- | --- |
| [architecture/README.md](architecture/README.md) | What the system is: services, boundaries, integration, deployment. Start here. |
| [architecture/consolidation.md](architecture/consolidation.md) | What was removed and why. The ledger of every simplification. |
| [architecture/data-model.md](architecture/data-model.md) | Every table, grouped by the database that owns it. |
| [architecture/geodata-imports.md](architecture/geodata-imports.md) | The import pipeline end to end, five stages and five states. |
| [architecture/gateway-and-contracts.md](architecture/gateway-and-contracts.md) | Why the gateway holds the contract and serves the playground. |
| [architecture/messaging.md](architecture/messaging.md) | Inter-service messaging without a broker: cursors, partitions, retention, failure modes. |
| [architecture/scaling.md](architecture/scaling.md) | The horizontal-scaling contract: what breaks at N replicas and what prevents it. |
| [architecture/auth-model.md](architecture/auth-model.md) | Separate admin and participant logins, and how the audience is enforced. |
| [architecture/secrets-and-config.md](architecture/secrets-and-config.md) | SOPS + age. Why Sealed Secrets is not used. |
| [architecture/technology.md](architecture/technology.md) | Language and framework per service, with the trade-off stated. |

### Diagrams

| Diagram | Shows |
| --- | --- |
| [architecture/system-context.svg](architecture/system-context.svg) | Clients → gateway → services → databases, with integration infrastructure. |
| [architecture/erd.svg](architecture/erd.svg) | Entity relationships across the three databases, including cross-database ids. |
| [architecture/scaling.svg](architecture/scaling.svg) | The replica fan-out and what makes each tier safe to run at N replicas. |

Both are hand-written SVG: no build step, no external assets, readable in a diff.

## Decisions

| ADR | Decision |
| --- | --- |
| [0001](decisions/0001-service-boundaries-follow-databases.md) | Service boundaries follow the databases. Identity and Programmes merge into `core`. |
| [0002](decisions/0002-language-per-service.md) | Language and framework per service, with Go-everywhere as the default. |
| [0003](decisions/0003-no-sealed-secrets.md) | No Sealed Secrets; SOPS with age instead. |
| [0004](decisions/0004-drop-message-broker.md) | Remove the message broker; the outbox is the event log. |
| [0005](decisions/0005-horizontal-scaling.md) | Every workload is horizontally scalable. |

An ADR is required when a rule in [AGENTS.md](../AGENTS.md) needs to be broken.

## UI mockups

[**ui/index.html**](ui/index.html) — open it in a browser to browse all eight
screens.

Four admin screens and four participant screens, static and responsive:

| Admin | Participant |
| --- | --- |
| [Operations overview](ui/admin-01-operations.html) | [Discover](ui/participant-01-home.html) |
| [Geodata review](ui/admin-02-geodata-review.html) | [Map explorer](ui/participant-02-map.html) |
| [Import run detail](ui/admin-03-import-run.html) | [Live activation](ui/participant-03-activation.html) |
| [Programmes and awards](ui/admin-04-programmes-awards.html) | [Awards](ui/participant-04-awards.html) |

They share one stylesheet ([ui/assets/mockup.css](ui/assets/mockup.css)), contain
no JavaScript framework, make no network requests, and follow
`prefers-color-scheme`. They exist to fix layout and information density before
implementation — they are not application code.

## Conventions used throughout

- **`myota_core`, `myota_geo`, `myota_activity`** — one database per service, no
  cross-database foreign keys.
- **api · worker · relay** — the three run modes of every domain service.
- **`ops.*`** — the operational schema present in each database: outbox,
  idempotency, consumer checkpoint, dead letter, job.
- **audience** — the session property (`participant` or `admin`) that keeps the
  two login surfaces separate.
- **stage / state** — an import run has five stages and five states; they are not
  the same axis and are documented separately.

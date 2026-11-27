# MyOTA documentation

Start here. Everything below is committed to this repository and intended to be
read in this order.

## Architecture

| Document | What it answers |
| --- | --- |
| [architecture/README.md](architecture/README.md) | What the system is: services, boundaries, integration, deployment. Start here. |
| [architecture/consolidation.md](architecture/consolidation.md) | What was removed and why. The ledger of every simplification. |
| [architecture/data-model.md](architecture/data-model.md) | Every table, grouped by the database that owns it. |
| [architecture/geodata-imports.md](architecture/geodata-imports.md) | The import pipeline end to end: five stages, five states plus `failed`. |
| [architecture/gateway-and-contracts.md](architecture/gateway-and-contracts.md) | Why the gateway holds the contract and serves the playground. |
| [architecture/messaging.md](architecture/messaging.md) | Inter-service messaging without a broker: cursors, partitions, retention, failure modes. |
| [architecture/scaling.md](architecture/scaling.md) | The horizontal-scaling contract: what breaks at N replicas and what prevents it. |
| [architecture/auth-model.md](architecture/auth-model.md) | Separate admin and participant logins, and how the audience is enforced. |
| [architecture/email-flows.md](architecture/email-flows.md) | Registration, activation, password recovery, email change — tokens and delivery. |
| [architecture/secrets-and-config.md](architecture/secrets-and-config.md) | Configuration (Fleet) and secrets (SOPS + age) as two separate stores. Why Sealed Secrets is not used. |
| [architecture/ci-secrets.md](architecture/ci-secrets.md) | GitHub Actions credentials: what CI needs, how to set it, and why that is almost nothing. |
| [architecture/technology.md](architecture/technology.md) | Language and framework per service, with the trade-off stated. |
| [architecture/structurizr/](architecture/structurizr/README.md) | The authoritative C4 model, and how to render it. |

### Diagrams

Diagrams are **Mermaid blocks inside the documents** — no build step, no committed
image, and GitHub renders them inline. There is nothing to regenerate and nothing
to drift.

| Diagram | Where | Type |
| --- | --- | --- |
| System context | [architecture/README.md](architecture/README.md) | flowchart |
| Entity relationships | [architecture/data-model.md](architecture/data-model.md) | three `erDiagram` blocks, one per database |
| Import pipeline | [architecture/geodata-imports.md](architecture/geodata-imports.md) | sequence |
| Import run state machine | [architecture/geodata-imports.md](architecture/geodata-imports.md) | state |
| Login and audience enforcement | [architecture/auth-model.md](architecture/auth-model.md) | sequence |
| Event delivery | [architecture/messaging.md](architecture/messaging.md) | sequence |
| Horizontal scaling | [architecture/scaling.md](architecture/scaling.md) | flowchart |

The **C4 model** behind these lives in
[architecture/structurizr/workspace.dsl](architecture/structurizr/workspace.dsl).
It is the authoritative description; the Mermaid blocks are a projection of it.

Check everything with:

```sh
python3 tools/check_diagrams.py            # local
python3 tools/check_diagrams.py --strict   # CI: Mermaid must render
python3 tools/check_diagrams.py --full     # CI with Java: DSL must compile too
```

## Decisions

| ADR | Decision |
| --- | --- |
| [0001](decisions/0001-service-boundaries-follow-databases.md) | Service boundaries follow the databases. Identity and Programmes merge into `core`. |
| [0002](decisions/0002-language-per-service.md) | Language and framework per service, with Go-everywhere as the default. |
| [0003](decisions/0003-no-sealed-secrets.md) | No Sealed Secrets; SOPS with age instead. |
| [0004](decisions/0004-drop-message-broker.md) | Remove the message broker; the outbox is the event log. |
| [0005](decisions/0005-horizontal-scaling.md) | Every workload is horizontally scalable. |
| [0006](decisions/0006-diagrams-as-code.md) | Diagrams as code: Mermaid projections over a Structurizr model. |

An ADR is required when a rule in [AGENTS.md](../AGENTS.md) needs to be broken.

## UI mockups

**[docs/ui/RENDERS.md](ui/RENDERS.md) — the gallery.** Ten screens, rendered as
PNG so they are visible in the repository. GitHub shows HTML as source, so the
images are the viewable form and the `.html` files are the interactive originals.

| Admin | Participant |
| --- | --- |
| [Operations overview](ui/renders/admin-01-operations.png) | [Discover](ui/renders/participant-01-home.png) |
| [Geodata review](ui/renders/admin-02-geodata-review.png) | [Map explorer](ui/renders/participant-02-map.png) |
| [Import run detail](ui/renders/admin-03-import-run.png) | [Live activation](ui/renders/participant-03-activation.png) |
| [Programmes and awards](ui/renders/admin-04-programmes-awards.png) | [Awards](ui/renders/participant-04-awards.png) |
| | [Check your email](ui/renders/participant-05-check-email.png) |
| | [Set a new password](ui/renders/participant-06-reset-password.png) |

Interactive versions: [`ui/index.html`](ui/index.html) — open locally in a browser.

**[docs/ui/styles/STYLES.md](ui/styles/STYLES.md) — four design directions.**
The same problem in four deliberate visual languages: editorial, neo-brutalist,
aurora glass, and warm organic. Each lives in its own directory with its own
stylesheet and its own renders, sharing nothing on purpose.

The first three are sampled — one admin and one participant screen, enough to
judge the language. **Warm organic was chosen and taken to completion:** all ten
screens, both colour schemes, three viewports.

| Direction | Admin | Participant | Scope |
| --- | --- | --- | --- |
| Editorial | [render](ui/styles/01-editorial/renders/admin.png) | [render](ui/styles/01-editorial/renders/participant.png) | 2 screens |
| Neo-brutalist | [render](ui/styles/02-brutalist/renders/admin.png) | [render](ui/styles/02-brutalist/renders/participant.png) | 2 screens |
| Aurora glass | [render](ui/styles/03-aurora/renders/admin.png) | [render](ui/styles/03-aurora/renders/participant.png) | 2 screens |
| **Warm organic** (selected) | [operations](ui/styles/04-organic/renders/admin-01-operations.png) · [review](ui/styles/04-organic/renders/admin-02-geodata-review.png) · [import](ui/styles/04-organic/renders/admin-03-import-run.png) · [programmes](ui/styles/04-organic/renders/admin-04-programmes-awards.png) | [discover](ui/styles/04-organic/renders/participant-01-discover.png) · [map](ui/styles/04-organic/renders/participant-02-map.png) · [activation](ui/styles/04-organic/renders/participant-03-activation.png) · [awards](ui/styles/04-organic/renders/participant-04-awards.png) · [check email](ui/styles/04-organic/renders/participant-05-check-email.png) · [reset](ui/styles/04-organic/renders/participant-06-reset-password.png) | **10 screens** |


They share one stylesheet ([ui/assets/mockup.css](ui/assets/mockup.css)), contain
no JavaScript framework, make no network requests, and follow
`prefers-color-scheme`. Regenerate the images with `node tools/render_mockups.mjs`.

## Conventions used throughout

- **`myota_core`, `myota_geo`, `myota_activity`** — one database per service, no
  cross-database foreign keys.
- **api · worker · relay** — the three run modes of every domain service.
- **`ops.*`** — the operational schema present in each database: outbox,
  idempotency, consumer checkpoint, dead letter, job.
- **audience** — the session property (`participant` or `admin`) that keeps the
  two login surfaces separate.
- **stage / state** — an import run has five stages and five non-terminal states plus `failed`; they are not
  the same axis and are documented separately.

# myota-new

MyOTA — a programme-agnostic outdoor activation platform for amateur radio
initiatives. This repository is the monorepo for all MyOTA services, web front
ends, API contracts, and deployment definitions.

**Development has moved here.** The previous repositories in this organisation
(`myota-next`, `myota-platform`, `myota`, `myota-deploy`, `myota-docs`,
`myota-contracts`, `myota-admin-web`, `myota-web`, `myota-geodata-service`,
`myota-activity-service`, `myota-programme-service`, `myota-identity-service`,
`myota-email-service`, `myota-operations-service`) are archived and read-only.
All new work — issues, branches, pull requests, releases — belongs in this
repository.

## Status

The architecture is designed and documented; implementation has not started.
Start at [**docs/**](docs/README.md).

## The design in one screen

```
                    ┌──────────────┐   ┌──────────────┐
                    │ Admin console│   │Participant app│     separate logins
                    └──────┬───────┘   └──────┬───────┘
                           └────────┬─────────┘
                                    ▼
                            ┌───────────────┐
                            │  API gateway  │  holds the contract
                            │  /docs        │  serves the playground
                            └───┬───┬───┬───┘
                ┌───────────────┘   │   └───────────────┐
                ▼                   ▼                   ▼
          ┌──────────┐        ┌──────────┐        ┌──────────┐
          │   core   │        │ geodata  │        │ activity │
          └────┬─────┘        └────┬─────┘        └────┬─────┘
               ▼                   ▼                   ▼
          myota_core           myota_geo          myota_activity
         (PostgreSQL)      (PostgreSQL+PostGIS)    (PostgreSQL)
```

Three services, three databases, one gateway. A service owns its database
exclusively; nothing else connects to it. Services exchange events through their
own outbox tables — there is no message broker. Every workload scales
horizontally: add a replica, no coordination, no affinity.

## Layout

```
apps/        deployable services (core, geodata, activity, gateway)
contracts/   OpenAPI documents and event schemas — source of truth
libs/        shared libraries: ops, limits, ui-tokens
web/         admin and participant front ends
deploy/      Helm charts, Fleet definitions, SOPS secret files
docs/        architecture, decisions, UI mockups
tools/       scripts and code generation
```

## Rules

The non-negotiable engineering rules are in [`AGENTS.md`](AGENTS.md):

- API First — OpenAPI 3.1, with the gateway serving `/docs` and `/openapi.json`
- Microservices — one service per database, each owning its schema
- Pub/sub between services — a durable event log in Postgres, no broker;
  at-least-once, idempotent consumers
- Job persistence external to the process
- Monorepo
- Deployment on the local K3s cluster — namespace `myota`, Fleet workspace `myota`
- No Sealed Secrets — SOPS with age
- Separate admin and participant logins, enforced by session audience
- Test-driven development
- **Horizontal scaling is a correctness property** — every workload, including
  both front ends, runs at N replicas with no session affinity

## Documentation

- [docs/README.md](docs/README.md) — index of everything
- [docs/architecture/README.md](docs/architecture/README.md) — the design
- [docs/architecture/consolidation.md](docs/architecture/consolidation.md) — what was removed and why
- [docs/architecture/messaging.md](docs/architecture/messaging.md) — messaging without a broker
- [docs/architecture/scaling.md](docs/architecture/scaling.md) — the horizontal-scaling contract
- [docs/ui/RENDERS.md](docs/ui/RENDERS.md) — ten UI screens, rendered and viewable
- [docs/ui/styles/STYLES.md](docs/ui/styles/STYLES.md) — the same two screens in four design directions
- [docs/decisions/](docs/decisions/README.md) — seven ADRs and the process that governs them
- [docs/architecture/structurizr/](docs/architecture/structurizr/README.md) — the C4 model

Diagrams are Mermaid blocks inside the documents; the C4 model behind them is the
authoritative description. Check both with `python3 tools/check_diagrams.py`.

## License

MIT — see [`LICENSE`](LICENSE).

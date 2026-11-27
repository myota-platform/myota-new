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

The monorepo is being established. Structure, service inventory, and the first
vertical slice are tracked in `docs/`.

## Engineering rules

The non-negotiable rules for this repository are in [`AGENTS.md`](AGENTS.md):

- API First — OpenAPI 3.1 contracts with served Swagger UI documentation and
  playground
- Microservices — one deployable per bounded context, each owning its data
- Communication between services via pub/sub (NATS JetStream)
- Job persistence external to the process
- Monorepo layout: `apps/`, `contracts/`, `libs/`, `deploy/`, `docs/`, `tools/`
- Deployment on the local K3s cluster — namespace `myota`, Fleet workspace
  `myota`
- Test-driven development

## Layout

```
apps/        deployable services and web front ends
contracts/   OpenAPI documents and event schemas
libs/        shared libraries
deploy/      Helm charts and Fleet definitions (deploy/fleet/myota)
docs/        architecture, runbooks, decision records
tools/       scripts and code generation
```

## License

MIT — see [`LICENSE`](LICENSE).

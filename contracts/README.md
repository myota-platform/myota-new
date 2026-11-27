# contracts

The API and event contracts. This directory is the source of truth for both:
handlers are written against it, and contract tests fail when a service drifts from
it.

## Current state

Empty. Chunk **F2** adds:

```
contracts/
  public.openapi.yaml     composed document, served by the gateway at /openapi.json
  core.openapi.yaml
  geodata.openapi.yaml
  activity.openapi.yaml
  events/                 CloudEvents 1.0 subset, one file per event type
```

`tools/compose_contracts.py` (also F2) merges the four documents and fails on a
duplicate `operationId`.

## Rules

- OpenAPI 3.1, and the published document is composed rather than hand-written.
- Every operation has an `operationId`, a summary, and one example.
- Every error response is the shared `Problem` schema (`application/problem+json`).
- Collections return `{ items, nextCursor }` — never a bare array.
- Every mutating operation documents `Idempotency-Key`.
- Numeric limits come from `libs/limits`, so the contract cannot disagree with the
  validator.

See `docs/architecture/gateway-and-contracts.md`.

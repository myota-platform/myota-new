# contracts

The API and event contracts. This directory is the source of truth for both:
handlers are written against it, and contract tests fail when a service drifts
from it.

```
contracts/
  public.openapi.yaml     GENERATED — the composed document the gateway serves
  composition.yaml        the composed document's info and servers
  core.openapi.yaml       identity, sessions, programmes, notifications
  geodata.openapi.yaml    entities, proposals, imports, sources, tiles
  activity.openapi.yaml   activations, QSOs, log imports, awards, statistics
  shared/
    components.yaml       Problem, CursorPage, EventFeed, the shared parameters
  events/
    v1.schema.json        the CloudEvents 1.0 subset envelope
    index.json            type -> file, the catalogue
    <type>.json           one file per event type
```

## Regenerating the composed document

```sh
python3 tools/compose_contracts.py --write    # after editing a service document
python3 tools/compose_contracts.py --check    # what `make verify` runs
python3 tools/compose_contracts.py --routes   # the gateway's prefix -> service table
```

`--check` fails when the committed `public.openapi.yaml` is stale, when a service
document is missing, when an `operationId` is used twice, when a path or a tag is
claimed by two services, when a published prefix has two owners, and when the
composed result does not validate as OpenAPI 3.1.

That last check matters more than it sounds: it is the only thing that catches a
`$ref` to a schema that no longer exists, and a dangling `$ref` renders as an
empty schema rather than as an error.

## Rules

- OpenAPI 3.1, and the published document is composed rather than hand-written.
- Every operation has an `operationId`, a summary, and one example. The composer
  enforces all three; operations that return no body are exempt from the example,
  because inventing a payload for a `204` is worse than having none.
- Every error response is the shared `Problem` schema
  (`application/problem+json`).
- Collections return `{ items, nextCursor }` — never a bare array.
- Every mutating operation documents `Idempotency-Key`, or states with
  `x-idempotency: not-applicable` and `x-idempotency-reason` why a key would be
  wrong. The exemption is reviewable; an allowlist inside the checker would not be.
- Numeric limits match `libs/limits`, so the contract cannot disagree with the
  validator: 1 GiB import upload, 300 candidates per promotion, 50-item default
  page, 200-item maximum page.
- Shared types live in `shared/components.yaml` and are referenced, never copied.
  Two copies of `Problem` cannot be caught by an `operationId` check — they have no
  id to collide on — and they drift the first time one of them is edited.

## Internal operations

Each service document also declares its `/internal/**` operations, marked
`x-internal: true`. They belong to the contract because the composer fetches
`/internal/openapi.json` and the contract test compares it, but they are filtered
out of `public.openapi.yaml` during composition. The composer fails if anything
under `/internal/` reaches the published document.

## Events

`contracts/events/` holds one JSON Schema per event type, plus the CloudEvents
envelope they all extend. `tools/check_events.py` fails when a type in
`docs/architecture/messaging.md` has no schema, when the index and the directory
disagree, when a file's `type` constant does not match its filename, and when a
schema's own example does not validate against it.

Adding an event type is three edits in one commit: the document that names it, the
schema file, and the index.

See `docs/architecture/gateway-and-contracts.md` and
`docs/architecture/messaging.md`.

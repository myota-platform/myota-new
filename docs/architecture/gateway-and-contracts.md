# Gateway and contracts

One gateway. It holds the contract, serves the playground, and routes. It has no
domain logic and no database.

## Why the gateway holds the contracts

The previous system had a gateway that was a pure prefix proxy, and each service
kept its own `openapi.yaml`. Nothing served those files, so:

- there was no Swagger UI anywhere in the deployment, despite API-first being a
  stated rule;
- the contract and the handlers drifted, because only a test compared them;
- a new consumer had to find four separate documents to understand the API.

Putting the composed contract in the gateway makes the rule enforceable: the
gateway binary embeds the contract, so a route that is not in the contract does
not exist.

## Layout

```
gateway/
  main.go              chi router, embedded contracts, proxy
  contracts/
    public.openapi.yaml     composed — served at /openapi.json
    core.openapi.yaml
    geodata.openapi.yaml
    activity.openapi.yaml
    events/                 event schemas
  middleware.go
  playground/            static Scalar bundle (embedded)
```

The service documents are composed into `public.openapi.yaml` at build time by a
small generator (`tools/compose-contracts`). Each service exposes
`GET /internal/openapi.json`, and the generator fetches it in CI, so a handler that
is not documented fails the build rather than silently existing.

## Routes

```
GET  /openapi.json            composed contract (public)
GET  /docs                    interactive playground
GET  /docs/{service}          playground filtered to one service
GET  /healthz  /readyz        gateway health
GET  /metrics                 Prometheus

/v1/auth/**            → core       (participant audience)
/v1/admin/auth/**      → core       (admin audience)
/v1/admin/**           → core | geodata | activity  (admin audience)
/v1/me/**              → core
/v1/accounts/**        → core
/v1/programmes/**      → core
/v1/proposals/**       → geodata
/v1/entities/**        → geodata
/v1/imports/**         → geodata
/v1/tiles/**           → geodata
/v1/activations/**     → activity
/v1/qsos/**            → activity
/v1/awards/**          → activity
/v1/statistics/**      → activity
/v1/public/**          → core | geodata | activity   (anonymous)
```

A request that matches no prefix is `404` with `problem+json`, never a guess.

## Edge responsibilities

The gateway does exactly these, and nothing else:

| Concern | Implementation |
| --- | --- |
| Routing | prefix table → upstream |
| Audience enforcement | verify `aud` against the route class before proxying |
| Token verification | signature and expiry; forwards the token unchanged |
| Request id | generate `X-Request-ID` if absent, echo in response |
| Trace propagation | create root span, forward `traceparent` |
| Body limit | 2 MiB for JSON routes; uploads go direct to the import endpoints with their own limit |
| CORS | explicit allowlist, credentials allowed only for the two web origins |
| Rate limit | per-IP on auth routes, per-session elsewhere |
| Problem responses | normalise upstream 5xx into `problem+json` |
| Health aggregation | `/readyz` reports upstream reachability |

It does **not**: aggregate responses from several services, hold business rules,
cache domain data, or terminate TLS (Traefik does that).

## The playground

`GET /docs` serves a Scalar-based page — a single static HTML file with an
embedded script, no CDN, no telemetry. It offers:

- the full composed API with per-service tags;
- authorisation input for both audiences, so an operator can try participant and
  admin calls in one place;
- try-it-out against the live gateway;
- the event schemas as a separate tab.

Swagger UI is the fallback if Scalar's licence terms ever become a problem; the
contract is standard OpenAPI 3.1 and either renderer works.

## Contract rules

1. Every operation has an `operationId`, a summary, and at least one example.
2. Every error response references the shared `Problem` schema.
3. Every collection response uses `{ items, nextCursor }` — no bare arrays.
4. Every mutating operation documents `Idempotency-Key`.
5. Breaking changes bump the path version; additive changes do not.
6. The contract is generated into the gateway binary; there is no runtime fetch.

## Why not a gateway product

Kong, Traefik, Envoy, and APISIX all solve the routing problem, and Traefik already
terminates TLS on this cluster. Each of them moves the contract and the playground
to a separate configuration surface that cannot be:

- unit-tested with the code that implements routing;
- deployed atomically with a route change;
- reviewed in the same pull request as a handler change.

The requirement was explicit: one gateway that *holds* the contracts. That means
it is code. The routing itself is 200 lines of Go over `net/http`; a product would
add a control plane, a CRD, and an admin API to replace those 200 lines.

## Failure behaviour

| Upstream state | Gateway response |
| --- | --- |
| Connection refused | `503` `problem+json`, `type=about:blank#upstream-unavailable` |
| Timeout (10 s default) | `504` |
| 5xx from upstream | passed through with request id attached |
| Circuit open (5 failures / 10 s) | `503` immediately, retried after 30 s |

The gateway never retries a mutating request — idempotency is the caller's
contract, expressed with `Idempotency-Key`, and services enforce it.

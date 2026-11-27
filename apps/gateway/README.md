# gateway

MyOTA API gateway. One Go binary, no database, no state.

| | |
| --- | --- |
| Language | Go 1.24, Chi |
| Owns | no database |
| Run modes | api only — no worker, no relay |
| Chunk | **F3** implements it |

**Owned database: no database.** The gateway is stateless by design: it routes, serves
the composed contract and enforces the session audience. A gateway with a database
would be a fourth service, not a router.

## What it does

```
GET  /openapi.json           the composed contract, embedded
GET  /docs                   the Scalar reference, embedded
GET  /docs/scalar.js         Scalar 1.85.0, vendored — no CDN
GET  /docs/boot.js           the mount call
GET  /event-schemas          the catalogue of event schemas
GET  /event-schemas/{type}   one JSON Schema
GET  /healthz  /readyz       liveness; per-upstream reachability
GET  /metrics                Prometheus

/v1/**                       prefix → upstream, or 404 problem+json
```

Routing is a prefix table **generated from the contract**
(`internal/routes/table_gen.go`, written by `tools/gen_gateway_routes.py`), not a
hand-maintained list. A prefix the contract does not declare is a 404; there is no
default upstream and no guessing.

The one authorisation decision at the edge: a participant credential is never accepted
under `/v1/admin/`. Bearer tokens are verified (EdDSA, keys fetched from `core` — see
[ADR-0008](../../docs/decisions/0008-gateway-token-verification-keys.md)); a
participant session cookie is refused on sight; an administrator cookie is forwarded
because only `core` can verify an opaque session. Everything else is the service's
business.

It does **not** aggregate responses, cache domain data, retry, count requests (Traefik
does, ADR-0005), terminate TLS (Traefik does), or hold business rules.

## Layout

```
main.go                    wiring, key loading, graceful shutdown
internal/config/           environment → Config, validated at startup
internal/routes/           generated prefix table + longest-prefix lookup
internal/session/          token verification, StaticKeys and JWKS sources
internal/upstream/         reverse proxy, deadline, 5xx normalisation, health probe
internal/playground/       /openapi.json, /docs, event schemas
internal/edge/             request id, traceparent, body limit, panic recovery
internal/problem/          RFC 9457 writer — one error envelope
internal/gateway/          router assembly, /healthz, /readyz, /metrics
internal/assets/           embed.FS: contract (JSON), event schemas, Scalar bundle
```

`internal/assets/openapi/` and `internal/assets/events/` are **generated** by
`tools/gen_gateway_assets.py` and checked in. `go:embed` cannot reach outside the
module, so the alternative was a volume or a build-time copy; copying at build time
would mean the binary in production could be built from a contract nobody reviewed.
`make verify` fails when a copy is stale.

## Running it

```sh
make build
GATEWAY_UPSTREAM_CORE=http://127.0.0.1:9001 \
GATEWAY_UPSTREAM_GEODATA=http://127.0.0.1:9002 \
GATEWAY_UPSTREAM_ACTIVITY=http://127.0.0.1:9003 \
GATEWAY_JWT_PUBLIC_KEY_FILE=/path/to/gateway.pub \
GATEWAY_JWT_KEY_ID=test-key \
./bin/gateway
```

| Variable | Default | Notes |
| --- | --- | --- |
| `GATEWAY_LISTEN_ADDR` | `:8080` | |
| `GATEWAY_UPSTREAM_CORE` | — | required; one per service in the route table |
| `GATEWAY_UPSTREAM_GEODATA` | — | required |
| `GATEWAY_UPSTREAM_ACTIVITY` | — | required |
| `GATEWAY_REQUEST_TIMEOUT` | `10s` | one proxied exchange, headers and body |
| `GATEWAY_JWT_ISSUER` | `myota-core` | the `iss` a session token must carry |
| `GATEWAY_JWKS_URL` | derived from `GATEWAY_UPSTREAM_CORE` | `/internal/auth/jwks` |
| `GATEWAY_JWT_PUBLIC_KEY_FILE` | — | local and tests only; PEM or base64url Ed25519 |
| `GATEWAY_JWT_KEY_ID` | — | the `kid` the file's key answers for |

There is no secret here. The production key source is the JWKS over the internal
network, which is why the deployment has nothing worth encrypting.

A missing upstream variable is a startup failure naming the variable. A missing key is
**not** — the public API works without it, `/readyz` says so, and `/v1/admin/**` answers
401 until it is fixed.

## Tests

```sh
make test        # go test -race ./...  (the gate)
make check       # fmt + vet + test
make cover       # total coverage
```

`internal/gateway/gateway_test.go` is the acceptance suite: a real router, real
middleware and real proxying, against three fake upstreams. It asserts the 404 shape,
the wrong-audience refusal for every administrator prefix, the token-free login paths,
the cookie rules, the body limit, the metric labels, the CSP, and that `/docs`
references nothing off-origin.

`tools/check_playground.mjs` goes further and renders `/docs` in Chromium, because a
page that returns 200 and shows nothing is indistinguishable from a working one to
every assertion made in Go.

## Why Go, and why not a gateway product

It multiplexes I/O and terminates nothing: a single static binary with the contract in
`embed.FS` and no runtime to warm up is the whole requirement (ADR-0002).

Kong, Envoy and APISIX would move the contract and the playground to a configuration
surface that cannot be unit-tested with the routing code, deployed atomically with a
route change, or reviewed in the same pull request as a handler change.

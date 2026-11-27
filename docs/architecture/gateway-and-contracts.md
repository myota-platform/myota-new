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

The service documents are composed into `public.openapi.yaml` at build time by
`tools/compose_contracts.py`. Each service exposes its own document at
`GET /internal/openapi.json`; `--fetch <service>=<url>` composes from the live
service instead of the committed file and reports every operation the two
disagree on, so a handler that is not documented fails the build rather than
silently existing.

Composition is offline by default (`--check` reads the committed documents), which
is what lets the contract gate run before any service is deployed. From F3 onward
the same flag pair runs against the deployed test environment.

## Routes

The gateway's own endpoints:

```
GET  /openapi.json            composed contract (public)
GET  /docs                    interactive playground
GET  /docs/{service}          playground filtered to one service
GET  /healthz  /readyz        gateway health
GET  /metrics                 Prometheus
```

Every API prefix belongs to **exactly one** service. That is what makes the
routing table derivable from the contract instead of maintained beside it:

```
$ python3 tools/compose_contracts.py --routes
/v1/activations/                   activity
/v1/admin/auth/                    core
/v1/admin/award-requests/          activity
/v1/admin/awards/                  activity
/v1/admin/email-suppressions/      core
/v1/admin/identity/                core
/v1/admin/notifications/           core
/v1/admin/programmes/              core
/v1/admin/proposals/               geodata
/v1/admin/qso-corrections/         activity
/v1/admin/sources/                 geodata
/v1/auth/                          core
/v1/awards/                        activity
/v1/entities/                      geodata
/v1/imports/                       geodata
/v1/log-imports/                   activity
/v1/me/                            core
/v1/programmes/                    core
/v1/proposals/                     geodata
/v1/qsos/                          activity
/v1/statistics/                    activity
/v1/tiles/                         geodata
```

`/v1/admin/` is three segments deep for exactly one reason: every service has
administrator endpoints, so a single `/v1/admin/` prefix would belong to all three
at once. Two segments everywhere else.

A request that matches no prefix is `404` with `problem+json`, never a guess — and
never a fall through to a default upstream.

### Audiences per prefix

| Prefix | Audience |
| --- | --- |
| `/v1/admin/**` | `admin` only. Every admin prefix, without exception. |
| `/v1/auth/**`, `/v1/proposals/**` | `participant`, or anonymous for registration and recovery |
| `/v1/activations/**`, `/v1/qsos/**`, `/v1/log-imports/**` | `participant` |
| `/v1/me/**` | either audience — own-account operations |
| `/v1/entities/**`, `/v1/imports/**` | anonymous read of published entities; `admin` for writes and imports |
| `/v1/awards/**` | anonymous for definitions; `participant` for `mine` and requests |
| `/v1/programmes/**`, `/v1/tiles/**`, `/v1/statistics/**` | anonymous |

Two endpoints were moved out of `/v1/me/` during contract composition so the table
above stays a partition: a participant's awards live at `/v1/awards/mine`, not
`/v1/me/awards`. They are `activity`'s rows, and putting them under a prefix owned
by `core` would have required the gateway to carry a longest-prefix exception for
two routes — a rule that holds everywhere except where someone remembered it.

## Edge responsibilities

The gateway does exactly these, and nothing else:

| Concern | Implementation | Chunk |
| --- | --- | --- |
| Routing | prefix table → upstream, generated from the contract | F3 |
| Audience enforcement | `aud` against the route class, before proxying | F3 |
| Token verification | EdDSA signature and expiry, keys fetched from `core` ([ADR-0008](../decisions/0008-gateway-token-verification-keys.md)) | F3 |
| Request id | generate `X-Request-ID` if absent, echo, and forward it upstream | F3 |
| Trace propagation | forward a valid `traceparent`, create a root one when absent | F3 |
| Body limit | 2 MiB, declared length and streamed reads. File bytes never pass through the gateway: uploads are presigned S3 URLs issued by `geodata-api`. | F3 |
| Problem responses | normalise an upstream 5xx that is not already `problem+json` | F3 |
| Health aggregation | `/readyz` reports per-upstream reachability | F3 |
| Metrics | request count and latency by route prefix; upstream health gauge | F3 |
| CORS | explicit allowlist, credentials allowed only for the two web origins | F6 |
| Request counting | **Not here.** Traefik owns rate limiting — a counter in a replica multiplies the limit by the replica count. | — |
| Circuit breaking | `// TODO` — see the note below | — |

It does **not**: aggregate responses from several services, hold business rules,
cache domain data, touch a database, count requests (Traefik does that), or
terminate TLS (Traefik does that too).

Token revocation is the one thing it deliberately leaves alone: the gateway checks
signature and expiry, and `core` checks `auth_revocation` on admin routes. A gateway
that queried Postgres would be a database client with a replica-count problem.

CORS is the exception worth explaining: it stays in the gateway because it is
stateless configuration, not a counter. Two replicas answering CORS identically is
correct; two replicas each counting to 100 is not.

The sentence that used to follow — "anything that accumulates state across requests,
rate limits *and circuit breakers*, lives in Traefik" — is wrong for breakers, and
the failure table above promises one. A rate limit that is per-replica breaks a
security control; a circuit breaker that is per-replica protects each replica and
multiplies nothing. Traefik's open-source middleware set has no circuit breaker
either, so the promise had nowhere to live. Tracked as **X1**; until it lands, an
upstream that is down is discovered by every request timing out rather than by the
first five failing.

### The administrator audience

The gateway enforces one authorisation rule, and it is the one that has to be
server-side: a participant credential is never accepted under `/v1/admin/`.

| Request | Result |
| --- | --- |
| Bearer token, `aud=admin` | proxied |
| Bearer token, `aud=participant` | `401 about:blank#wrong-audience` |
| Bearer token that does not verify | `401 about:blank#unauthorized` |
| No bearer, `__Host-myota_sid` (participant cookie) | `401 about:blank#wrong-audience` |
| No bearer, `__Host-myota_admin_sid` | proxied — `core` owns the session table and verifies it |
| Nothing at all, on a token-free path such as `/v1/admin/auth/login` | proxied |
| Nothing at all, anywhere else under `/v1/admin/` | `401 about:blank#unauthorized` |

The cookie row is the one that matters. The two audiences have had different cookie
names since the redesign, but until F3 that difference was a browser convention: the
server did not care which cookie arrived. It does now.

Everything outside `/v1/admin/` is `AudienceOpen`: the gateway forwards and the
service decides. It is deliberately not stricter than that, because a route class is
coarser than the operations under it — `/v1/entities/` is anonymous to read and
administrator-only to write, so any per-prefix rule that judged `aud` there would
refuse a participant browsing the map with their own session cookie.

## The playground

`GET /docs` serves a Scalar-based page. Three files, all from the gateway's own
binary:

```
GET /docs                the page — a banner, an empty #app, two script tags
GET /docs/scalar.js      Scalar 1.85.0 standalone, vendored, MIT, 4.5 MB (1.35 MB gzipped)
GET /docs/boot.js        the twelve-line call that mounts it
```

- the full composed API with per-service tags;
- authorisation input for both audiences, so an operator can try participant and
  admin calls in one place;
- try-it-out against the live gateway;
- links to `/event-schemas` and `/event-schemas/{type}`, which serve the CloudEvents
  schemas as JSON Schema.

**No third party, enforced rather than promised.** The page is served with
`Content-Security-Policy: script-src 'self'; connect-src 'self'; frame-ancestors
'none'`, so the browser refuses an off-origin script *and* an off-origin fetch even if
someone later edits the page. There is no inline script, which is why the mount call
is a served file rather than a `<script>` block: a nonce would work too, and would
weaken the statement from "one origin" to "one origin plus whatever we signed today".

`tools/check_playground.mjs` starts the real binary, loads the page in Chromium, and
fails on a console error, a failed request, a request to another origin, or a mount
point that stays empty. A page that returns 200 and renders nothing passes every
string comparison and is useless to the person who needed it.

`/openapi.json` carries a validator and `Cache-Control: max-age=300`, not
`immutable`: the gateway is redeployed, and a client holding an API description that
no longer matches the deployment is worse than one that revalidates.

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
it is code. The routing itself is a few hundred lines of Go over `net/http`; a
product would add a control plane, a CRD, and an admin API to replace them.

## Failure behaviour

| Upstream state | Gateway response |
| --- | --- |
| Connection refused | `503` `problem+json`, `type=about:blank#upstream-unavailable` |
| Timeout (10 s default) | `504` |
| 5xx from upstream | passed through with request id attached |
| Circuit open (5 failures / 10 s) | `503` immediately, retried after 30 s |

The gateway never retries a mutating request — idempotency is the caller's
contract, expressed with `Idempotency-Key`, and services enforce it.

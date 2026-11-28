# ADR-0013 — Service-to-service authorisation is one decision endpoint

**Status:** accepted · 2026-11-28
**Provenance:** directed in part — chunk A2's brief names `POST /internal/authorize`, "audience `service`, Ed25519-signed, 60 s TTL" — with the token exchange, the fail-closed default and the cache-invalidation mechanism chosen here.

## Context

`geodata` and `activity` have to answer questions like "may this account review entities
in programme A?". They own their own databases and cannot read `myota_core`, so the answer
has to come from `core` over the network.

Three constraints shape what that call looks like.

1. **There is no service mesh, no SPIFFE, no mTLS.** Services are Deployments in one
   namespace on one node; the only identity a pod reliably has is what it is given.
2. **The gateway is not in the path.** An internal call goes service to service, so the
   gateway's audience check — the mechanism that separates the two login surfaces — does
   not apply to it.
3. **An access token's scopes are a snapshot.** A token carries the scopes it was minted
   with. A revocation that only reached the token would take up to ten minutes to matter,
   which for a privilege change is the same as never.

`docs/architecture/auth-model.md` already stated the intent: "Internal calls between
services use short-lived JWTs signed by `core` … obtained with a client-credentials grant
over the internal network."

## Decision

**One endpoint decides, it reads the database, and callers authenticate with a
sixty-second token obtained by a client-credentials exchange.**

```
POST /internal/token      {service, secret}  → {accessToken, expiresAt, audience: service}
POST /internal/authorize  {accountId, scope, programmeId?, sessionId?}
                                             → {allowed, effectiveScopes[], reason?}
```

- **The decision is computed from the open grants on every call.** The token's `scp`
  claim is never consulted for authorisation, so a revoked role or a revoked session is
  effective on the next request rather than at token expiry.
- **The endpoint answers `200` with `allowed: false`.** It is a question, not an action;
  a refusal is an answer. The `reason` distinguishes `scope_not_held`, `session_revoked`,
  `account_not_active` and `account_not_found`, because a caller does different things
  with each.
- **Service tokens carry `aud: "service"` and the caller's name in `svc`.** The
  *lifetime* is checked, not just the expiry: `verify_service` refuses a token whose
  signed `iat`/`exp` span more than sixty seconds, because a long-lived token would
  verify perfectly and defeat the point.
- **Credentials are per service, in one Secret** (`myota-core-service-clients`,
  `clients.json`). Comparison is constant-time over SHA-256, and one message covers
  "unknown service" and "wrong secret" so the endpoint cannot enumerate services.
- **With nothing configured, every service token is refused.** Fail-closed: a caller that
  was never issued a credential cannot obtain authority by asking.
- **`/internal/**` is not published.** Every such operation carries `x-internal: true`, so
  the composer leaves it out of `public.openapi.yaml`; the gateway's route table is
  generated from that document, so the paths do not exist there at all. A gateway test
  asserts it, and a service token would be refused there anyway — the gateway accepts
  exactly two audiences.
- **A role change publishes `core.role.changed.v1`** into `ops.outbox_event` in the same
  transaction, carrying the target's open session ids. That is what lets a consumer drop
  a cache it already holds.

## Alternatives considered

**Let callers use the account's access token and trust its `scp` claim.** Rejected: it is
the cheapest option and the one that breaks the guarantee. A revoked administrator would
keep their authority for up to ten minutes, and the check would be answering from a value
the caller can replay.

**A per-target audience (`aud=core`).** Rejected: it turns one audience value into one per
service, so every new service is a new value in a verifier, and a token minted for `core`
is useless to `geodata` for no gain. The caller's identity is in `svc`; the audience says
"this is a service call", which is the only distinction that matters.

**A shared static API key, no expiry.** Rejected: it is a credential with no revocation
path other than redeploying every caller, and it makes every internal call equally
privileged forever. The exchange costs one indexed insert-free query pair per minute per
service.

**mTLS or SPIFFE.** The right answer at scale, and rejected here as disproportionate: it
needs a certificate authority, distribution, rotation and an admission mechanism, on a
single-node cluster, to protect a decision endpoint that already reads only its own
database. The token exchange needs no infrastructure at all.

**Authorise inside each service, reading a replicated copy of the role tables.** Rejected
by AGENTS.md rule 2: a service is the database it owns, and a replica of another service's
authority tables is a second answer that drifts.

**Have `core` push decisions ("provision these scopes into geodata").** Rejected: it
inverts the dependency and makes `core` responsible for every consumer's consistency. Pull
per request is slower and simpler to reason about.

## Consequences

**Cost**

- **`core` is a hard dependency for any decision.** If it is down, `geodata` and
  `activity` cannot authorise writes. They can still serve anonymous reads — that is the
  intended degradation — but a write path fails closed. Two replicas and a readiness probe
  bound the risk; nothing removes it.
- **One long-lived secret per calling service** is still a shared credential. Rotating it
  is a Secret update plus a rollout, and there is no per-caller least privilege: any
  configured service can ask about any account. The compromise is bounded by the endpoint
  being read-only and by the caller already being inside the namespace.
- **A token per call, or a cache that defeats the TTL.** A service making many decisions
  will cache the token, which means up to sixty seconds of extra validity. That is the
  design's own tension, accepted: the alternative is an unsigned internal call.
- **Consumers must consume the event to drop a cache.** `core` cannot reach into another
  service. A consumer that caches without consuming `core.role.changed.v1` will hold a
  stale decision until its own TTL, and the architecture note's promise of immediate
  invalidation is only as good as that consumer.
- **The role catalogue is schema, not data.** Five roles are seeded by the baseline.
  Adding a bespoke role is a migration; that is deliberate (a role is authority, and
  authority configured outside review is authority nobody audited) and it is inconvenient
  for one-off grants.
- **`account_role.scopes` (a jsonb narrowing column) is not consulted.** The contract's
  `RoleGrantRequest` does not expose it, so a grant's authority comes from its role alone.
  A per-grant narrowing would need a decision about merge semantics, and shipping one
  without that decision would be guessing.

**Gains**

- **One implementation of the decision.** A console request and a service question go
  through the same function, so they cannot disagree about who may do what.
- **Revocation is immediate**, in both directions: revoking a role and revoking a session
  both take effect on the next call, because the decision is read, not remembered.
- **The blast radius of a stolen service secret is one minute of read-only questions.**
  Compare with a static key that never expires.
- **No new infrastructure.** No mesh, no CA, no broker: one endpoint, one Secret, and the
  outbox that already existed.

## Verification

- `apps/core/tests/authority_flow.rs` — thirteen tests through the real router: a service
  token is required and an administrator's token is refused; the exchange refuses a wrong
  secret and an unknown service with the same message; a held scope is allowed and an
  unheld one refused with `scope_not_held`; a programme-scoped grant does not answer for
  another programme; a revoked session is refused even though the account holds every
  scope; a revoked role takes effect on the next decision; the grant of a scope the actor
  does not hold is `403 about:blank#scope-not-held` and leaves no row; the actor cannot
  revoke their own last administrator role; and a grant writes
  `core.role.changed.v1` naming the target's open sessions.
- `apps/gateway/internal/routes/routes_test.go` — no `/internal/**` path and no prefix in
  the generated table.
- On the cluster: `POST /internal/authorize` through the public hostname is `404` from the
  gateway, and the same call through a port-forward with a real service token answers.

Implementation: [apps/core/src/authority.rs](../../apps/core/src/authority.rs),
[apps/core/src/http/authority.rs](../../apps/core/src/http/authority.rs).

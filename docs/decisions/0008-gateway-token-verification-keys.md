# ADR-0008 — The gateway verifies tokens with keys published by `core`

**Status:** accepted · 2026-11-27
**Provenance:** implementation — chunk F3 needed a decision the architecture left unstated.

## Context

The architecture requires the gateway to verify a token's signature and enforce its
audience per route class (`gateway-and-contracts.md`, "Edge responsibilities";
`auth-model.md`, "Token shape"). `core` signs sessions with an Ed25519 private key.

`secrets-and-config.md` states that key's custody precisely — `core-session-key`,
`ed25519-private` plus `kid`, *mounted only by `core`*, rotated every 180 days with
two keys valid during the overlap. What it does not say is how the gateway obtains
the **public** half.

That gap is not cosmetic. Three consequences follow from it:

- without a key the gateway cannot verify anything, so the audience check would be
  reading an unverified claim — a value any caller can write;
- a public key shipped in the gateway's configuration is frozen at deploy time, so a
  `core` key rotation would need a gateway redeploy, and during the overlap period
  the gateway would reject tokens signed with the new key — an outage caused by
  following the documented rotation procedure;
- the deployment has nowhere to put the key material: ADR-0003 removed Sealed
  Secrets and the gateway holds no secret of its own.

## Decision

**`core` publishes its verification keys as a JWKS at `GET /internal/auth/jwks`; the
gateway fetches them, caches by `kid`, and refetches when it meets a `kid` it does not
know.**

- The endpoint is `/internal/**`, so it is not published: it is excluded from
  `public.openapi.yaml` and reachable only on the cluster network.
- The document is RFC 7517 with only `kty: OKP`, `crv: Ed25519` entries used; another
  key type in the same document is ignored rather than rejected, so `core` can publish
  keys for another purpose later.
- The gateway caches the set for 15 minutes and refetches on an unknown `kid`, at most
  once per request. A failed refresh keeps the last good set: a brief `core` restart
  must not start refusing tokens.
- **A rotation needs no gateway redeploy.** Both keys appear in the document during
  the overlap, tokens signed with either verify, and the old key disappears when the
  rollover finishes.
- The URL is derived from the configured `GATEWAY_UPSTREAM_CORE`, so the gateway
  cannot be pointed at one `core` for traffic and another for keys.
  `GATEWAY_JWKS_URL` overrides it, and `GATEWAY_JWT_PUBLIC_KEY_FILE` exists for a
  local run and for tests, where `core` is not up.

**With no usable key the gateway refuses.** `/v1/admin/**` answers 401 and `/readyz`
reports `verification: "no verification key…"`. It does not fall back to parsing the
token without checking the signature. A gateway that trusts an unsigned `aud` claim is
worse than one that refuses traffic: the first is silently open, the second is
obviously broken.

## Alternatives considered

**A static public key in the gateway's configuration.** Rejected. It works right up
until the first rotation, at which point the two options are "reject half the tokens"
or "redeploy both services together". A configuration file also cannot express "two
keys valid during rollover" without inventing a format the JWKS already defines.

**HS256 with a shared secret.** Rejected for two reasons. Anyone able to *verify* a
token could then *mint* one — a shared secret makes every verifier a signer, so the
gateway would hold the ability to issue administrator sessions. And it invites
algorithm confusion: a verifier that accepts `HS256` and also holds an Ed25519 public
key can be tricked into validating a token signed with that public key as the HMAC
secret. `session.Verify` pins `EdDSA` for exactly this reason, and a test presents the
classic forgery to prove it is refused.

**No verification at the edge; let `core` decide.** Rejected. The audience split is the
one authorisation decision the architecture puts at the gateway, and doing it on an
unverified claim is worse than not doing it, because it looks like a control. Leaving
verification entirely to `core` also means every administrator request pays a round
trip to `core` before it can be rejected — the failure mode the edge exists to avoid.

**Fetching on every request.** Rejected: it makes `core` a dependency of every request
and a bottleneck on the path it was meant to protect.

## Consequences

**Cost**

- **The gateway gains a runtime dependency on `core` for its key material.** It is a
  cached, background dependency rather than a per-request one, and a stale set keeps
  working, but a `core` that has never been reachable means administrator routes stay
  closed. That is the right direction for the failure — closed, not open — and it is
  still a coupling worth naming.
- **`core` publishes a key document, so its key lifecycle is now an API.** Rotating a
  key is no longer a secret-only operation: the document has to list both keys for the
  overlap window, and getting that wrong is visible as rejected tokens rather than as
  a failed deployment.
- **One more thing to watch.** A JWKS that stops updating leaves the gateway verifying
  against an old key set. `/readyz` reports whether verification is configured at all,
  but not whether the cached set is stale; a metric for the last successful refresh is
  the obvious follow-up.

- `core` gains one endpoint and must serve it before it accepts traffic. A1 owns that.
- The gateway holds no secret. Its deployment has nothing worth encrypting, which is
  why `GATEWAY_JWT_PUBLIC_KEY_FILE` is a development affordance and not a production
  path.
- A `core` that publishes a malformed key document fails verification for *new* keys
  while the cached set keeps working. That is the intended asymmetry: a bad rotation
  degrades rather than breaks, and the log line names the key that failed to decode.
- The absolute worst case is bounded: an attacker who can serve the JWKS is an
  attacker inside the cluster network, who already has what the keys protect.

## Verification

`apps/gateway/internal/session` covers the source of truth for this decision: caching,
refetch on an unknown `kid`, keeping the last good set when a refresh fails, ignoring
key types it cannot use, and refusing everything when the source is empty. The forgery
cases — `alg: none`, and HS256 signed with the public key — are refused by test.

Specification: [docs/architecture/gateway-and-contracts.md](../architecture/gateway-and-contracts.md),
[docs/architecture/auth-model.md](../architecture/auth-model.md).

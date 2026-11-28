# ADR-0012 — `core` holds the token signing key and encrypts TOTP seeds

**Status:** accepted · 2026-11-28
**Provenance:** proposed and implemented in chunk A1 (issue #8), which required "Argon2id for passwords" and "no session without a second factor" without naming where the keys live. The placement below is the implementation's choice, recorded because a future contributor will ask why.

## Context

Chunk A1 needs three pieces of secret material that are not user passwords:

1. **A signing key for access tokens.** The gateway verifies every administrator
   request, so the key has to be reachable from both services.
2. **The TOTP seed**, per administrator. `account_mfa.secret_ref` is documented in
   `data-model.md` as "a reference into the secret store, never the secret itself: this
   table is read by an admin console and backed up, and a TOTP seed in either is a
   second factor that is no longer a factor."
3. **Recovery-code hashes.** Hashed, not encrypted — the plaintext never needs to come
   back.

The cluster has no KMS, no Vault, and no cloud secret manager. It has Kubernetes
Secrets, SOPS-encrypted files applied by hand (ADR-0003), and whatever a service holds
in its own environment.

## Decision

**Both live in `core`, in one Kubernetes Secret (`myota-core-signing`), applied by hand
from a SOPS file.**

- `jwt-seed-base64` — a 32-byte Ed25519 seed. `core` signs with it and publishes the
  public half at `/internal/auth/jwks`; the gateway caches that document and refetches
  on an unknown `kid` (ADR-0008). The gateway never holds the private key, and a key
  rotation is one `core` deploy plus a key-id bump rather than two coordinated ones.
- `mfa-key-base64` — a 32-byte AES-256-GCM key. Each TOTP seed is stored as
  `local:v1:<base64(nonce || ciphertext || tag)>` in `account_mfa.secret_ref`. The
  `local:` provider prefix is the seam a real KMS slots into: the column already names
  a provider, so moving to one changes one module and no schema.

Recovery codes are SHA-256 of 48 random bits, single-use. Nothing outside `core` needs
them, and the verification path is bounded to five attempts per challenge.

## Alternatives considered

**Ship the public key in the gateway's configuration instead of a JWKS.** Rejected: the
key would be frozen at deploy time, so the documented 180-day rollover would mean
redeploying the gateway, and during the overlap it would reject half the tokens. This is
ADR-0008's argument, restated here because it is the alternative someone proposes first.

**Store TOTP seeds in the database in plaintext, and rely on the database being
trusted.** Rejected: it makes every database backup, every `pg_dump` handed to a
consultant, and every read-replica a working second factor for every administrator. The
column's own documentation says the opposite.

**Store seeds as Kubernetes Secrets, one per account.** Rejected: a Secret per
administrator means a `kubectl create secret` per enrolment, which the enrolment
endpoint cannot perform (it has no API access), and the RBAC to create Secrets inside
`myota-test` is broader than the RBAC to read one Secret.

**Encrypt seeds with a key derived from the token signing seed.** Rejected: one key for
two purposes means a rotation of either re-enrols every administrator, and a compromise
of either exposes both. Two keys cost one more Secret field.

**An external secrets manager (Vault, OpenBao, a cloud KMS).** Rejected for now, not
forever: it is another stateful service on a single node, with its own backup, unseal
and upgrade story, to protect two 32-byte values. The envelope format keeps the door
open — `local:v1:` becomes `kms:v1:` with one module changed.

## Consequences

**Cost**

- **The key lives in an environment variable.** Anyone who can `kubectl exec` into a
  `core` pod, read its environment, or read the Secret can decrypt every TOTP seed and
  mint tokens for any audience. The mitigation is RBAC on one namespace, and it is
  weaker than a KMS with an audit log. Recorded rather than glossed.
- **Rotating the MFA key re-enrols everyone.** There is no key-id on the envelope, so a
  new key cannot decrypt old seeds. A rotation is therefore "rotate, then every
  administrator enrols again". A versioned envelope would fix it; it is not worth the
  complexity for a system with tens of administrators, and it is the first thing to add
  if that changes.
- **Rotating the signing key needs a key-id bump** and a window in which the gateway
  holds both keys. The JWKS supports that — one entry per key — but `core` serves only
  the key it signs with today, so a rotation is a short overlap the operator has to
  arrange (add the new key, wait for the gateway's cache to refresh, then remove the
  old one). Not automated.
- **Two secrets to apply by hand** per environment, and a `kubectl exec` for the first
  administrator. Provisioning is deliberately manual; see below.

**Gains**

- **A database backup is not a second factor.** The property the schema promises is now
  true: reading `account_mfa` yields ciphertext, and reading `account_recovery_code`
  yields hashes.
- **No new infrastructure.** Two 32-byte values in a Secret the deployment already
  needed, with no unseal, no leader election and no extra backup.
- **The gateway holds nothing secret.** It fetches a public key over the cluster
  network and caches it, so a compromised gateway leaks no signing capability.
- **The seam exists for a KMS.** `secret_ref` names a provider, so the database does not
  change when the provider does.

## Verification

- `apps/core/src/secretbox.rs` — a sealed value never contains its plaintext or its
  base64 form; a tampered envelope fails to decrypt rather than yielding a different
  seed; another key cannot open it.
- `apps/core/src/token.rs` — the published JWKS carries the same 32 bytes the verifier
  holds; a token signed by another key, with another `kid`, or with `alg: none` is
  refused.
- `apps/core/tests/auth_flow.rs` — the audit-payload test asserts that no
  `security_event` row contains the password or the TOTP secret.
- On the cluster: `kubectl -n myota-test get secret myota-core-signing` has exactly
  `jwt-seed-base64` and `mfa-key-base64`, and
  `select secret_ref from account_mfa` returns a `local:v1:` envelope, never a base32
  seed.

Provisioning: [apps/core/README.md](../../apps/core/README.md), "Provisioning the first
administrator".

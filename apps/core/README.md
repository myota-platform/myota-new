# core

MyOTA identity and programme service: the administrator sign-in flow, sessions, and
the signing keys the gateway verifies against.

| | |
| --- | --- |
| Language | Rust 1.85 (Axum 0.8, SQLx 0.8) |
| Owns | myota_core |
| Run modes | api (one process; worker and relay arrive with the outbox work) |
| Chunk | **A1** implemented the sign-in flow |

**Owned database: myota_core.** It holds accounts, credentials, sessions, MFA, callsigns, roles, programmes, content, policy, and the single notification ledger — plus the `ops` schema.

No other service connects to it, and it connects to no other service's database.
That rule is the whole of ADR-0001: a service *is* the database it exclusively
owns.

## What it serves

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/v1/admin/auth/login` | password → a second-factor challenge, or a refusal |
| `POST` | `/v1/admin/auth/mfa/challenge` | answer the challenge → a session |
| `POST` | `/v1/admin/auth/refresh` | rotate the session |
| `POST` | `/v1/admin/auth/logout` | revoke it |
| `POST` | `/v1/admin/auth/mfa/enrol` | start a TOTP enrolment |
| `POST` | `/v1/admin/auth/mfa/enrol/confirm` | confirm it with a code |
| `GET` | `/internal/auth/jwks` | the public keys the gateway verifies tokens with |
| `GET` | `/internal/openapi.json` | this service's contract, for `compose_contracts --fetch` |
| `GET` | `/healthz`, `/readyz` | liveness; readiness also checks the database |

The contract is `contracts/core.openapi.yaml`; it is the source of truth, and the
document served at `/internal/openapi.json` is generated from it by
`tools/gen_core_contract.py`.

## The guarantee

**No administrator session exists without a confirmed second factor.** It is not a
check at the end of `login`: `login` returns a challenge or a refusal, and
`complete_challenge` is the only function that creates a session. There is no flag,
environment variable or development mode that skips it.

Supporting properties, each checked by a test:

- Argon2id at the OWASP parameters; the salt is per hash, so two accounts with the
  same password look different in the table.
- An unknown address and a wrong password take the same time and return the same
  body, so the endpoint is not an account enumeration oracle.
- Five wrong passwords lock the account for fifteen minutes **in the database**, so
  every replica agrees. The lock is checked before the password, so it cannot be
  probed.
- Five sign-in attempts per key per fifteen minutes, keyed by address and by client IP.
- A wrong TOTP code costs one of five attempts; the fifth burns the challenge.
- A TOTP code reused inside its own 30-second step is refused.
- A recovery code is single-use.
- A token of the participant audience is refused on every administrator route, and
  the gateway refuses it before the request reaches this service.
- Every outcome writes a `security_event` row. The payload never contains a
  password, a code, a token or a seed — there is a test for that.

## Running it

```sh
# A database with the baselines applied.
psql "$ADMIN_URL" -c 'CREATE DATABASE myota_core'
for f in deploy/myota/db/shared/*.sql deploy/myota/db/core/*.sql; do
  psql -v ON_ERROR_STOP=1 "$DATABASE_URL" -f "$f"
done

export CORE_DATABASE_URL="$DATABASE_URL"
export CORE_JWT_SEED_BASE64="$(head -c 32 /dev/urandom | base64 -w0)"
export CORE_MFA_KEY_BASE64="$(head -c 32 /dev/urandom | base64 -w0)"
cargo run --bin core
```

Configuration is the environment and only the environment; `src/config.rs` lists every
variable with its default and refuses a value it cannot use — a key of the wrong
length, a zero lifetime — instead of surfacing it as a 500 later.

In the cluster the same values arrive from two Secrets: `myota-core-db-credentials`
(the password, as `CORE_DB_PASSWORD`) and `myota-core-signing` (`jwt-seed-base64`,
`mfa-key-base64`). The database is reached through the pooler, and the pool is built
with `statement_cache_capacity(0)` — pgBouncer in transaction mode does not keep a
prepared statement between transactions.

## Provisioning the first administrator

There is no self-service admin registration, and there cannot be: enrolment needs a
session and a session needs a confirmed factor. The first administrator is provisioned
by whoever runs this:

```sh
kubectl -n myota-test exec -i deploy/core -- ./core provision-admin \
  --email admin@example.org --name "Administrator" --password-stdin <<< "$PASSWORD"
```

It prints the TOTP secret, the `otpauth://` URI and ten recovery codes — once. The seed
is stored encrypted (AES-256-GCM, key from `CORE_MFA_KEY_BASE64`) and the codes are
stored hashed, so neither can be recovered from the database. `A2` replaces this with
the admin-management endpoints.

## Tests

```sh
make -C apps/core test         # unit tests always; the flow tests need a database
make -C apps/core test-db      # requires CORE_TEST_DATABASE_URL, fails without it
```

The flow tests drive the real router against a real PostgreSQL: they assert the
property, not that a handler called a function. Without `CORE_TEST_DATABASE_URL` they
say so and pass, which is the same rule `make checks-ops` follows.

## Layout

- `src/config.rs` — the environment, read once, failing loudly.
- `src/problem.rs` — RFC 7807 responses; the only place a status code is chosen.
- `src/password.rs`, `src/totp.rs`, `src/secretbox.rs`, `src/token.rs` — the
  primitives, none of which knows about HTTP or SQL.
- `src/session.rs` — the session row, the refresh token and the cookie.
- `src/auth/` — the flow, where the primitives meet the tables.
- `src/http/` — routing, extraction, and the mapping from a typed failure to a
  response.
- `src/bootstrap.rs` — provisioning an administrator, and what the operator sees.
- `src/db.rs` — the pool, and why it is configured the way it is.

## What A1 deliberately left out

- **WebAuthn.** The contract names it, the enum accepts it, and enrolment refuses it
  with a named reason. A passkey is a second flow through the same challenge table;
  shipping half of it would be a factor nobody can use.
- **Refresh-token reuse detection.** Rotation replaces the hash, so a replayed old
  token is refused — but the session family is not revoked, because that needs a
  rotation history. Tracked as a finding.
- **Role evaluation beyond "holds a role with a scope".** `A2` owns the catalogue, the
  scope vocabulary and the rule that a role is granted only by someone who holds it.

## Why it is not in the other language

This is the surface that ends the project if it is wrong: an authorisation bug here is
not a bug in one screen. ADR-0002 accepts the extra toolchain for this service and
`geodata`.

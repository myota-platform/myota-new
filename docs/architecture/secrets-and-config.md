# Secrets and configuration

**Configuration and secrets are two separate stores with two separate lifecycles.
They are never mixed, never share a key, and never share a delivery mechanism.**

| | Configuration | Secrets |
| --- | --- | --- |
| What | behaviour: log level, TTLs, feature switches, endpoints, replica counts | credentials: passwords, keys, connection URLs with a password |
| Where | `deploy/myota/config/*.yaml` — **plaintext in Git** | `deploy/myota/secrets/*.sops.yaml` — encrypted in Git |
| Applied by | **Fleet, automatically** | an operator or pipeline step, deliberately |
| Reviewable | yes — a config change is a diff in a pull request | ciphertext only |
| Rotating it | a commit | reseal and re-apply |
| If it leaks | a behaviour change, visible in review | a compromise |

Provisioning configuration through Fleet is the intended path and is the right
default. Provisioning **secrets** through Fleet is not: an automated sync that can
decrypt is an automated sync that can leak.

CI has its own, third store — **GitHub Actions secrets** — and it holds credentials
for the pipeline only, never for the cluster. See
[ci-secrets.md](ci-secrets.md).

Postgres DSNs live in secrets because they embed a password. Host, port and
database name are configuration. If splitting one DSN across two stores feels
awkward, that is a signal to give the service separate discrete variables rather
than one URL — which is better practice anyway.

**Sealed Secrets is not used.** The cluster still runs the controller for other
applications; `myota-new` does not depend on it, and no Myota secret is a
`SealedSecret`.

## Configuration — provisioned by Fleet

Configuration lives in Git as plain YAML and is reconciled by Fleet like any other
resource. A change is a pull request; a rollback is a revert.

```
deploy/
  myota/                    charts, values and secrets
    config/
      core.yaml             ConfigMap
      geodata.yaml          ConfigMap
      activity.yaml         ConfigMap
      gateway.yaml          ConfigMap
      web.yaml              ConfigMap
    secrets/
      core.sops.yaml        Secret, encrypted — NOT reconciled by Fleet
  fleet/
    myota-test/             bundle → namespace myota-test
    myota/                  bundle → namespace myota (production)
```

The same chart serves both namespaces; only the values and the namespace differ.
A secret is applied **per namespace**, from the same encrypted file.

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: core-config
  namespace: myota
  labels:
    fleet.spainip.es/workspace: myota
data:
  LOG_LEVEL: info
  SESSION_ACCESS_TTL: 15m
  SESSION_REFRESH_IDLE: 720h
  REGISTRATION_ENABLED: "true"
  OTEL_EXPORTER_OTLP_ENDPOINT: ""
```

```yaml
# deployment excerpt
envFrom:
  - configMapRef:
      name: core-config      # non-secret, reconciled by Fleet
  - secretRef:
      name: core-secrets     # secret, applied separately
```

`envFrom` keeps the two visibly separate in the manifest. The rule that goes with
it:

> **No key may exist in both the ConfigMap and the Secret for a service.**
> Kubernetes resolves the later `envFrom` entry first, so an overlap is a silent
> precedence bug waiting to happen. A CI check parses both and fails on
> intersection.

One ConfigMap per service, with defaults that are safe to run without any
override. Nothing in the design requires an environment variable to be set in
order to boot — a missing secret fails readiness with a clear message instead of
starting with an empty value.

This removes a real failure mode: the legacy code gated participant registration
on `MYOTA_PARTICIPANT_PUBLIC_URL` being a valid HTTPS URL and returned `503`
otherwise, which looked like an outage rather than a configuration choice.

### What belongs where

| Value | Store | Why |
| --- | --- | --- |
| `LOG_LEVEL`, `OTEL_*` endpoint | config | behaviour, no credential |
| Session TTLs, refresh windows | config | behaviour |
| Replica counts, resource limits | config (Helm values) | behaviour |
| Feature switches (`REGISTRATION_ENABLED`) | config | behaviour, reviewable |
| IdP issuer URL, audience | config | public by definition |
| SMTP **host and port** | config | not secret; the account is |
| Database **host, port, name** | config | connectivity, not a credential |
| Database **password** | secret | credential |
| Full `DATABASE_URL` containing a password | secret | embeds a credential |
| `core-session-key` (Ed25519 private) | secret | signing key |
| `s3-credentials` | secret | access key and secret |
| SMTP user and password | secret | credential |
| WebAuthn RP secret | secret | credential |

The pattern: **the endpoint is configuration, the credential is a secret.** A value
goes in the secret store only when it grants access to something.

## Secrets — never provisioned by Fleet

The rest of this document covers the secret store: why Sealed Secrets is out, what
replaces it, what the inventory is, and how it is applied.

### Why not Sealed Secrets

Sealed Secrets requires a controller that holds a private key inside the cluster.
That key is the only thing standing between an encrypted blob in Git and a
plaintext credential, it cannot be rotated without resealing every secret, and
recovery depends on a resource that lives in the same cluster as the workloads it
protects. That is a single point of failure sitting inside the blast radius.

The infra repository already reached this conclusion —
`infra/REMOVE-SEALED-SECRETS.md` is a runbook for removing it, and `sops` and
`age` are installed with a configured recipient in `infra/.sops.yaml`.

### What replaces it

**SOPS with `age`.** Encrypted files live in Git; decryption happens in an
operator workflow or a pipeline, never in the cluster.

```
myota-new/
  deploy/
    myota/
      config/                 ConfigMaps — plain, non-secret
      secrets/
        core.sops.yaml        encrypted
        geodata.sops.yaml     encrypted
        activity.sops.yaml    encrypted
        gateway.sops.yaml     encrypted
    .sops.yaml                recipient policy
```

`.sops.yaml`:

```yaml
creation_rules:
  - path_regex: "deploy/myota/secrets/.*\\.sops\\.yaml$"
    encrypted_regex: "^(data|stringData)$"
    age: "age1<operator recipient>"
```

Applying:

```sh
sops --decrypt deploy/myota/secrets/core.sops.yaml | kubectl apply -f -
```

Fleet does not decrypt SOPS. The encrypted files are therefore **outside** the
paths Fleet reconciles, and a separate, deliberate step applies them. That is the
point: an automated sync that can decrypt secrets is an automated sync that can
leak them.

### Secret inventory

Small and explicit. Every entry has an owner and a rotation path.

| Secret | Keys | Owner | Rotation |
| --- | --- | --- | --- |
| `core-db` | `url` | core | On compromise; 90-day scheduled |
| `geodata-db` | `url` | geodata | On compromise; 90-day scheduled |
| `activity-db` | `url` | activity | On compromise; 90-day scheduled |
| `core-session-key` | `ed25519-private`, `kid` | core (mounted only by core) | 180 days, two keys valid during rollover |
| `nats-credentials` | `url`, `user`, `password` | all | 180 days |
| `s3-credentials` | `access-key`, `secret-key`, `endpoint`, `bucket-prefix` | geodata, activity | 180 days |
| `smtp-credentials` | `host`, `port`, `user`, `password`, `from` | core | On provider change |
| `webauthn-rp` | `rp-id`, `rp-name`, `origin` | core | On domain change |

Eight secrets. The legacy deployment had `registry-credentials` duplicated into
16 namespaces (`infra/REMOVE-SEALED-SECRETS.md`), plus per-service URL bundles,
SMTP credentials, NATS credentials, and object-storage keys scattered across
`myota-auth`, `myota-database-urls`, `myota-database-migration-urls`,
`myota-nats-credentials`, `myota-object-storage`, `myota-postgres-auth`,
`myota-seaweedfs-s3-config`, `myota-smtp`, and
`myota-smtp-qualification-server`.

#### One namespace, one secret per concern

There is no per-namespace duplication because there is one namespace. Each service
mounts its own database secret plus only the shared secrets it actually uses:

| Service | Mounts |
| --- | --- |
| `core` | `core-db`, `core-session-key`, `smtp-credentials`, `webauthn-rp` |
| `geodata` | `geodata-db`, `s3-credentials` |
| `activity` | `activity-db`, `s3-credentials` |
| `gateway` | nothing — it verifies signatures with a public key compiled in |

A service never mounts another service's database secret or the core session key,
so a compromised `activity` pod cannot read credentials for `myota_core`.

#### No registry credentials in the cluster

Images are public on GHCR for now. If a private registry is ever needed, it is one
`imagePullSecret` on the namespace's `default` service account, not fourteen
copies.

## What must never appear in the repository

Enforced by a pre-commit hook and a CI job (`gitleaks`), not by discipline:

- connection strings with passwords
- private keys of any kind
- `age` private keys
- bearer tokens or API keys
- `kubectl`-applied plaintext Secret manifests
- a credential in a ConfigMap
- a secret in a workflow file, even base64-encoded or split across lines

A `.sops.yaml` recipient is public and belongs in Git. The matching private key
never does; it lives on the operator's machine at
`~/.config/sops/age/keys.txt` and in the password manager's recovery record.

## Recovery

The split makes recovery two clearly ordered steps, and the order matters:

1. **Configuration** arrives by itself. Fleet reconciles it from Git as soon as the
   cluster is registered — nothing to restore by hand.
2. **Secrets** are applied deliberately, from the encrypted files.

| Scenario | Recovery |
| --- | --- |
| Operator loses the `age` key | Restore from the password-manager record; reseal from the last known values |
| Cluster lost | Register the cluster with Fleet — config reconciles itself — then apply decrypted secrets |
| Config wrong | Revert the commit; Fleet reconciles the previous state |
| Secret compromised | Rotate at the source, reseal, re-apply, restart consumers |
| SOPS unavailable | `sops --decrypt` is the only requirement; no controller, no CRD |

Recovery does not depend on any in-cluster component. That is the whole argument.

## Checks that enforce all of this

| Check | Where | Fails when |
| --- | --- | --- |
| `gitleaks` | pre-commit + CI | a credential is committed in any form |
| Config/secret key intersection | CI | a key exists in both a service's ConfigMap and its Secret |
| SOPS recipient present | CI | a `*.sops.yaml` file does not match the policy in `.sops.yaml` |
| No plaintext `kind: Secret` | CI | a Secret manifest is committed unencrypted |
| No cluster credential in workflows | CI | a workflow references a kubeconfig or service-account token |

The key-intersection check is the one that catches the most likely real mistake:
someone adds `SMTP_PASSWORD` to the ConfigMap because the app was reading it from
the environment and the ConfigMap was the obvious place.

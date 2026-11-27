# Secrets and configuration

**Sealed Secrets is not used.** The cluster still runs the controller for other
applications; `myota-new` does not depend on it, and no Myota secret is a
`SealedSecret`.

## Why

Sealed Secrets requires a controller that holds a private key inside the cluster.
That key is the only thing standing between an encrypted blob in Git and a
plaintext credential, it cannot be rotated without resealing every secret, and
recovery depends on a resource that lives in the same cluster as the workloads it
protects. That is a single point of failure sitting inside the blast radius.

The infra repository already reached this conclusion —
`infra/REMOVE-SEALED-SECRETS.md` is a runbook for removing it, and `sops` and
`age` are installed with a configured recipient in `infra/.sops.yaml`.

## What replaces it

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

## Secret inventory

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

### One namespace, one secret per concern

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

### No registry credentials in the cluster

Images are public on GHCR for now. If a private registry is ever needed, it is one
`imagePullSecret` on the namespace's `default` service account, not fourteen
copies.

## Configuration

Everything non-secret is a ConfigMap, reconciled by Fleet and reviewable in the
pull request that changes it.

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: core-config
  namespace: myota
data:
  LOG_LEVEL: info
  OTEL_EXPORTER_OTLP_ENDPOINT: ""
  SESSION_ACCESS_TTL: 15m
  SESSION_REFRESH_IDLE: 720h
  REGISTRATION_ENABLED: "true"
```

One ConfigMap per service, with defaults that are safe to run without any
override. Nothing in the design requires an environment variable to be set in
order to boot — a missing secret fails readiness with a clear message instead of
starting with an empty value.

This removes a real failure mode: the legacy code gated participant registration
on `MYOTA_PARTICIPANT_PUBLIC_URL` being a valid HTTPS URL and returned `503`
otherwise, which looked like an outage rather than a configuration choice.

## What must never appear in the repository

Enforced by a pre-commit hook and a CI job (`gitleaks`), not by discipline:

- connection strings with passwords
- private keys of any kind
- `age` private keys
- bearer tokens or API keys
- `kubectl`-applied plaintext Secret manifests

A `.sops.yaml` recipient is public and belongs in Git. The matching private key
never does; it lives on the operator's machine at
`~/.config/sops/age/keys.txt` and in the password manager's recovery record.

## Recovery

| Scenario | Recovery |
| --- | --- |
| Operator loses the `age` key | Restore from the password-manager record; reseal from the last known values |
| Cluster lost | New cluster; apply ConfigMaps from Fleet, then apply decrypted secrets |
| Secret compromised | Rotate at the source, reseal, re-apply, restart consumers |
| SOPS unavailable | `sops --decrypt` is the only requirement; no controller, no CRD |

Recovery does not depend on any in-cluster component. That is the whole argument.

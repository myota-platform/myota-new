# ADR-0003 — No Sealed Secrets; SOPS with age instead

**Status:** accepted · 2026-11-27

## Context

The cluster runs the Sealed Secrets controller, and other applications use it.
Seventeen `SealedSecret` resources were inventoried on 2026-11-04, most of them a
`registry-credentials` copy duplicated per namespace.

The infra repository already documents removal (`REMOVE-SEALED-SECRETS.md`) and
already carries a SOPS policy in `.sops.yaml` with an age recipient. `sops` and
`age` are installed on the host.

## Decision

`myota-new` does not use Sealed Secrets. No Myota credential is a `SealedSecret`.
Secrets are SOPS-encrypted files in Git, decrypted by an operator workflow and
applied with `kubectl`. The controller remains installed for other applications
and is not a dependency of this one.

## Reasons

1. **The decryption key lives inside the blast radius.** Sealed Secrets requires a
   controller holding a private key in the same cluster as the workloads it
   protects. Compromise of the cluster compromises every sealed value.
2. **Rotation is all-or-nothing.** Rotating the controller key requires resealing
   every secret. There is no per-secret path.
3. **Recovery depends on an in-cluster component.** If the cluster is lost, the
   recovery procedure starts by rebuilding the thing that decrypts the recovery
   material.
4. **SOPS has no in-cluster component at all.** Decryption is `sops --decrypt` on
   an operator machine. Recovery does not depend on anything running.
5. **It removes 14 duplicated `registry-credentials` secrets**, because there is
   one namespace and images are public.

## Alternatives considered

| Option | Verdict |
| --- | --- |
| Sealed Secrets (status quo) | Rejected for the reasons above. |
| **SOPS + age** | **Chosen.** No controller, plain files in Git, auditable diffs of the ciphertext, recovery without the cluster. |
| External Secrets Operator + Vault | Rejected: adds a controller, a Vault deployment, and an auth path to reach the same outcome. Justified only when there is a real secret store to integrate with. |
| `kubectl create secret` by hand, never in Git | Rejected: no history, no review, no recovery record. |
| Sealed Secrets with an external KMS key | Rejected: still an in-cluster controller, and adds a cloud dependency to a local cluster. |

## Consequences

**Gained**

- No Myota secret depends on an in-cluster decryption component.
- Recovery: restore ConfigMaps from Fleet, apply decrypted secrets, start services.
- Eight secrets total, each with a named owner and rotation interval.

**Cost**

- Applying secrets is a deliberate manual (or pipeline) step. Fleet does not
  reconcile them, by design — an automated sync that can decrypt secrets is an
  automated sync that can leak them.
- The operator needs the age private key. It lives in the password manager's
  recovery record, outside the repository.
- Encrypted files are outside Fleet's reconciled paths, so drift is possible and
  must be checked by a periodic job rather than converging automatically.

## Verification

1. `kubectl get sealedsecrets -A` returns nothing in namespace `myota`.
2. No file in `deploy/myota/` contains `SealedSecret`.
3. `deploy/.sops.yaml` has a rule matching `deploy/myota/secrets/*.sops.yaml`.
4. A CI job decrypts a fixture with a test key to prove the policy is valid.

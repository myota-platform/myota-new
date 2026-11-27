# ADR-0009 — Three buckets per environment, one credential per environment

**Status:** accepted · 2026-11-27
**Provenance:** directed in part — "one user and set of credentials for test, one for prod" — with the bucket layout chosen here.

## Context

Files have to live somewhere: import sources up to 1 GiB, log files, entity
attachments, awarded certificates, callsign evidence. The cluster already runs Silo
(a PGSTY fork of MinIO) as a shared tenant store, so the question is not *which*
server but *how to divide it*.

What is being stored differs along three axes that matter operationally:

| Data | Size | Lifetime | If lost |
| --- | --- | --- | --- |
| Import sources and log files | up to 1 GiB each | useful for one import run | re-fetched from the provider, or re-uploaded |
| Entity attachments | ≤ 8 MiB | as long as the entity | re-uploaded by a user |
| Award certificates | small | forever | a participant's earned record, unrecoverable |
| Callsign evidence | ≤ 8 MiB | until the account is erased | verification cannot be defended |

The node has one disk: 436 GiB, 144 GiB free at the time of writing, shared with
Loki, container images and k3s itself. Silo was already at 65 % of that device. An
imports set that never shrinks is therefore not a tidiness problem — it is an outage
with a date on it.

The instruction fixed one constraint: a single user and credential pair per
environment. That rules out per-service credentials, and with them any permission
boundary between `geodata`, `activity` and `core`.

## Decision

**Three buckets per environment** — six in total — and **one user per environment**:

```
myota-<env>-imports     imports/, logs/          expire after 30 days
myota-<env>-content     entities/                no expiry
myota-<env>-records     certificates/, evidence/ no expiry; certificates are write-once
```

with `<env>` ∈ `test`, `prod`. One IAM policy per user (`myota-<env>-only`), granting
read-write on that environment's three buckets and nothing else, following the shape
this server already uses for `bminfo`.

The split is chosen so that each distinction is enforced by a **mechanism**:

| Distinction | Mechanism |
| --- | --- |
| Bulk expires, records never do | one lifecycle rule, on `imports` only |
| A bug cannot delete a certificate | the credential has no `s3:DeleteObject` on `records/certificates/*` |
| Account erasure is one operation | `records/evidence/<accountId>/` is a prefix delete |
| Import growth is visible | `mcli du` per bucket; the bucket that grows is the one being measured |

## Alternatives considered

**One bucket per environment, prefixes inside it.** Rejected. It removes one
lifecycle rule and one `mcli mb`, and costs every distinction in the table above:
expiry becomes a prefix rule that a later edit can widen, and a single
`rm --recursive --force` on the bucket takes the certificates with the imports.
The credential can already delete anything in its three buckets; the point of the
bucket boundary is to make the *irreplaceable* class not be in the same namespace as
the disposable one.

**One bucket per prefix (eight per environment).** Rejected. With one credential per
environment the extra boundaries are organisational only — nothing technical stops a
service writing another's prefix — and organisational boundaries that no mechanism
enforces are documentation that goes stale. It also triples the number of
`mcli mb` lines an operator has to get right. Fourteen buckets to express four
lifetimes is more surface than meaning.

**Per-service credentials despite the instruction.** Rejected because it contradicts
a direct requirement, and because the compensating control is adequate: services
never hand out credentials, and the only path to storage from outside is a presigned
URL whose signature binds the key. If a service boundary ever needs a permission
mechanism, the answer is a fourth bucket, not a fourth credential.

**Versioning on `records`.** Rejected. Silo's versioning would recover a deleted
certificate, but the data is one `local-path` claim on one disk — a lost disk loses
the versions too. It doubles the storage of overwritten objects to protect against
the failure the policy restriction already covers. A real backup is the answer
(X3), not versioning.

## Consequences

**Cost**

- **One credential spans three services.** Nothing technical prevents `activity`
  writing `imports/`; the guarantee is that `object_key` is always constructed by the
  owning service from its own table. That is a review-level control, and it is
  weaker than a policy would be. Recorded here rather than implied.
- **Six buckets is more to provision and more to explain** than one. It is justified
  by the expiry asymmetry, not by a wish for tidiness — an environment with a single
  bucket would need expiry on everything or on nothing.
- **`--prod` in a bucket name against namespace `myota`.** A reader has to learn the
  mapping. The alternative, `myota-imports`, is ambiguous with the project name, and
  the mistake that matters is writing test data into production.
- **The 30-day expiry weakens import replay.** Provenance keeps the source reference
  and the checksum, so it still says what was read and when; the bytes are gone and
  a byte-identical replay needs a re-fetch. Accepted, and recorded in the document.
- **Storage is unencrypted at rest**, because Silo's KMS key file is absent. That is
  not caused by this decision, but it is where the consequence lands, so it is
  tracked as X4.

## Verification

`python3 tools/provision_silo.py --check` — asserts the inventory, the two policy
files and the document agree: every bucket is named in the prose, every declared
prefix is reachable by its policy, no policy grants a forbidden action
(`s3:ListAllMyBuckets`, bucket create/delete, `PutObjectAcl`, `admin:*`), and no
policy can delete under `certificates/`.

`--print` emits the exact provisioning commands from the same inventory, and `--live`
compares the inventory with the running server, read-only.

Specification: [docs/architecture/object-storage.md](../architecture/object-storage.md).
Provisioning: [deploy/silo/README.md](../../deploy/silo/README.md).

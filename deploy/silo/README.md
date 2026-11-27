# deploy/silo — provisioning Myota's object storage

What has to exist on the cluster's Silo server before `geodata`, `activity` or `core`
can store a file, and how to create it. The reasoning is in
[object-storage.md](../../docs/architecture/object-storage.md); this is the procedure.

**Nothing described here has been applied.** As of this writing the Silo server holds
four buckets (`bminfo-raw`, `spainip-loki-data`, `spainip-loki-rules`, `test`) and two
users (`bminfo`, `spainip-loki`), and none of Myota's. `python3 tools/provision_silo.py --live`
prints exactly that.

```
deploy/silo/
  inventory.yaml            buckets, prefixes, retention — the source of truth
  policies/
    myota-test-only.json    what the test credential may do
    myota-prod-only.json    what the prod credential may do
  README.md                 this file
```

Silo is shared: `bminfo` and `spainip-loki` are tenants on the same server. Everything
below is scoped to Myota's own buckets, and no step requires — or grants — access to
anyone else's.

## Two hostnames, and the mistake that looks like success

| Host | Serves | Use for |
| --- | --- | --- |
| `https://silo-api.spainip.es` | the S3 API | `mcli` — buckets, policies, users, lifecycle |
| `https://silo.spainip.es` | the console (a single-page app) | a browser |
| `http://silo.silo.svc.cluster.local:9000` | the S3 API, in-cluster only | `mcli` run *inside* the pod |

**The console answers HTTP 200 with an HTML page for every path, including
`PUT /<bucket>`.** An S3 client pointed at it reads the 200 as success and the HTML as a
malformed XML body, so:

- `mcli mb` prints `Bucket created successfully` for a bucket that was never created;
- `mcli ilm` reports that `GetLifecycle` is unsupported for the `filesystem` backend,
  which is false — lifecycle works, as a probe against a scratch bucket confirmed;
- `mcli alias set` fails with `XML syntax error on line 35`, and every `mcli admin`
  command afterwards says `No valid configuration found`.

All three happened on the first real run of this runbook, from a script pointed at the
console host. The first is the dangerous one: it is a success message for work that did
not happen.

`python3 tools/provision_silo.py --print` therefore emits a preflight that refuses to run
unless the endpoint returns `application/xml`, and `--endpoint` rejects the console
outright.

## What gets created

| Environment | Bucket | Retention |
| --- | --- | --- |
| test (`myota-test`) | `myota-test-imports` | 30 days |
| test | `myota-test-content` | none |
| test | `myota-test-records` | none |
| prod (`myota`) | `myota-prod-imports` | 30 days |
| prod | `myota-prod-content` | none |
| prod | `myota-prod-records` | none |

| Environment | User | Policy |
| --- | --- | --- |
| test | `myota-test` | `myota-test-only` |
| prod | `myota-prod` | `myota-prod-only` |

In Silo a user's name *is* its access key — `bminfo` and `spainip-loki` are both named
that way — so only a secret is generated. Use one from a password manager: ≥ 40
characters, mixed, no shell metacharacters. Silo stores it hashed; nobody can read it
back, so losing it means `mcli admin user add` again with a fresh secret.

## Step 1 — create the buckets and users

The commands are generated from `inventory.yaml` so the runbook cannot drift from the
inventory:

```sh
python3 tools/provision_silo.py --print > /tmp/provision.sh
```

That emits a `set -euo pipefail` script with a preflight, then `mcli mb`, `mcli ilm add`,
`mcli admin policy create`, `mcli admin user add` and `mcli admin policy attach`, grouped
by environment. The endpoint defaults to the public API host; `--in-cluster` emits the
ClusterIP form for running inside the pod, and `--endpoint` overrides both.

Run it as a Silo administrator with the two secret variables set, from the repository root
(the policy files are read by relative path):

```sh
read -r -s -p "secret for myota-test: " MYOTA_TEST_S3_SECRET; echo
read -r -s -p "secret for myota-prod: " MYOTA_PROD_S3_SECRET; echo

export SILO_ADMIN_ACCESS_KEY=... SILO_ADMIN_SECRET_KEY=...   # a Silo root/admin credential
python3 tools/provision_silo.py --print > /tmp/provision.sh

# The policy files are read by path, so run from the repository root and copy them
# into the pod, or create the policies from wherever this shell can see them:
kubectl -n silo cp deploy/silo/policies/myota-test-only.json silo-0:/tmp/ 2>/dev/null || true
```

In practice the simplest path is to run the commands inside the Silo pod, which
already has `mcli` and the administrator credential in its environment:

```sh
kubectl -n silo exec -it deploy/silo -- sh
#   inside the pod:
#   $ mcli alias set local http://127.0.0.1:9000 "$MINIO_ROOT_USER" "$MINIO_ROOT_PASSWORD"
#   $ mcli mb --ignore-existing --region us-east-1 local/myota-test-imports
#   ... the rest of the printed commands, with `local` instead of `myota-silo`
```

`mb --ignore-existing` makes re-running harmless, so a partially applied step is fixed
by running it again. The policy and user commands are not idempotent in the same way:
`admin policy create` on an existing name replaces it, and `admin user add` on an
existing name sets a new secret. That is deliberate — it is also the rotation path.

## Step 2 — verify

```sh
python3 tools/provision_silo.py --live   # reports each bucket and user as exists/MISSING

mcli ls myota-test-imports
mcli admin user info myota-test          # the policy list is what matters
mcli ilm ls myota-test-imports           # one rule, 30 days
```

Expected after provisioning: 6 buckets and 2 users reported as `exists`, and one
lifecycle rule on each `*-imports` bucket.

### Verifying the policy is not too broad

As the Myota user, not as root. This is the check that matters — an attached policy proves
nothing until a request is refused.

```sh
mcli alias set probe http://127.0.0.1:9000 myota-test           # inside the pod
# or: mcli alias set probe https://silo-api.spainip.es myota-test   # from a host
#     (enter the secret from the SOPS file when prompted)

mcli ls probe                     # only myota-test-{content,imports,records}
mcli ls probe/myota-prod-imports  # Access Denied
mcli ls probe/bminfo-raw          # Access Denied
mcli stat probe/bminfo-raw        # Object does not exist
mcli ls probe/myota-test-imports  # succeeds
```

The first is worth understanding: `mcli ls ALIAS` calls `ListBuckets`, and Silo returns the
*buckets the caller can access* rather than refusing outright. So the assertion is not "it
fails" but "it lists exactly three buckets, none of them another tenant's" — which is the
stronger property anyway. There is no `s3:ListAllMyBuckets` in the policy, so the result
cannot include anything Myota does not own.

The certificate rule is the other one worth checking by hand, because it protects the one
irreplaceable class of bytes:

```sh
echo probe | mcli pipe probe/myota-test-records/certificates/probe   # allowed
mcli rm probe/myota-test-records/certificates/probe                  # Access Denied
```

## Step 3 — put the secret in the cluster

The credential never enters Git in plaintext and is never reconciled by Fleet
([ADR-0003](../../docs/decisions/0003-no-sealed-secrets.md)).

| Environment | Encrypted file in Git | Applied as |
| --- | --- | --- |
| test | `deploy/myota/secrets/s3-credentials.myota-test.sops.yaml` | Secret `s3-credentials` in `myota-test` |
| prod | `deploy/myota/secrets/s3-credentials.myota-prod.sops.yaml` | Secret `s3-credentials` in `myota` |

Two files, one per environment, because the two are different identities rather than
the same value provisioned twice. `deploy/.sops.yaml` carries the cluster's age
recipient; the files are created and edited with `sops`, which leaves the non-secret
scaffolding readable in a diff.

```sh
umask 077
MYOTA_TEST_S3_SECRET='...'  # the same secret you gave mcli

sops --encrypt --age "$(python3 -c "import yaml;print(yaml.safe_load(open('deploy/.sops.yaml'))['creation_rules'][0]['age'])")" \
  --input-type yaml --output-type yaml /dev/stdin \
  > deploy/myota/secrets/s3-credentials.myota-test.sops.yaml <<YAML
apiVersion: v1
kind: Secret
metadata:
  name: s3-credentials
  namespace: myota-test
type: Opaque
stringData:
  access-key: myota-test
  secret-key: ${MYOTA_TEST_S3_SECRET}
YAML

# Apply by hand, into the namespace the file names. Never `kubectl apply -f deploy/`.
sops --decrypt deploy/myota/secrets/s3-credentials.myota-test.sops.yaml \
  | kubectl apply -f -
```

Repeat with `myota-prod` / namespace `myota`.

The **configuration** — endpoint, signing endpoint, region, bucket names — is not a
secret and lives in a ConfigMap instead:

```
deploy/myota/config/myota-test/storage.yaml     → ConfigMap in myota-test
deploy/myota/config/myota/storage.yaml          → ConfigMap in myota
```

Those keys are listed in [object-storage.md](../../docs/architecture/object-storage.md#the-configuration-keys-every-service-reads).
Per environment, because the bucket names differ; a single file with both sets would
be a file where one wrong apply points test pods at production storage.

## Rotation

```sh
# 1. set a new secret on the same user — the old one stops working immediately
mcli admin user add local myota-test "$NEW_SECRET"
# 2. update the Secret and restart the workloads that mount it
sops --decrypt ... | kubectl apply -f -
kubectl -n myota-test rollout restart deploy
```

Two steps, one downtime-free window, no bucket or policy changes. Rotating by creating
a second user and deleting the first also works and avoids the window, at the cost of
the policy having to be attached twice.

## Cleanup, if an environment is ever retired

```sh
mcli admin user remove local myota-test
for b in imports content records; do
  mcli rm --recursive --force local/myota-test-$b
  mcli rb --force local/myota-test-$b
done
```

`rm --recursive --force` is the dangerous flag combination on this server: it will
happily remove `bminfo-raw` if you type it. Check the bucket name twice, and prefer
`mcli ls` first.

## What this directory does not do

- It does not create a Silo administrator. The root credential is provisioned with the
  Silo deployment itself, per `my-silo/README.md`, and is the only account that can run
  these commands.
- It does not configure Prometheus. Myota's buckets are visible in the existing Silo
  scrape; a per-bucket usage alert is not set up.
- It does not back anything up. `records` holds the only irreplaceable bytes in the
  system (issued certificates), and it lives on a single `local-path` claim. Tracked as
  **X3**.

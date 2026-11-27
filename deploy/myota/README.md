# deploy/myota

The three databases, their poolers, the chart that runs them, and the migrations that
define them. One chart, two environments: `myota-test` and `myota` differ only by
namespace and, later, by values overrides.

```
myota/
  Chart.yaml, values.yaml       the chart
  db/
    shared/
      000_ops_schema.sql        the ops schema, a copy of libs/ops/sql/ (every database)
      020_updated_at.sql        the updated_at trigger, for every table that has one
    core/010_baseline.sql       myota_core: identity, programmes, notifications
    geo/010_baseline.sql        myota_geo: the catalogue and import pipeline, with PostGIS
    activity/010_baseline.sql   myota_activity: activations, awards, statistics
  templates/
    statefulset-postgres.yaml  one StatefulSet and one headless Service per database
    configmap-postgres.yaml    the init script that creates the application role
    deployment-pgbouncer.yaml  one pooler Deployment and Service per database
    configmap-database.yaml    the endpoint each service reads — always the pooler
    configmap-migrations.yaml  the SQL, rendered into a ConfigMap per database
    configmap-migrate-script.yaml  the runner
    job-migrate.yaml           the Job that applies it
  secrets/                     SOPS-encrypted credentials, applied by hand
```

## The three databases

| Database | Owner | Image | Volume | Why separate |
| --- | --- | --- | --- | --- |
| `myota_core` | `core` | `postgres:16.4-alpine` | 10 Gi | identity, latency-critical, small |
| `myota_geo` | `geodata` | `postgis/postgis:16-3.4` | 20 Gi | the only spatial data; largest by bytes |
| `myota_activity` | `activity` | `postgres:16.4-alpine` | 10 Gi | append-heavy, largest by rows |

A service is defined by the database it exclusively owns (ADR-0001). Merging them would
let one service's load profile decide another's availability, and would delete the only
structural reason the service boundaries are where they are.

**No table appears in two databases.** `tools/check_db_schema.py` asserts that against
`docs/architecture/data-model.md` in both directions: every table the document names
exists, and no baseline invents one the document does not describe. The `ops` schema is the
deliberate exception — the same tables in all three, holding different rows, because the
outbox row must be written in the owning database's transaction.

51 domain tables: 25 in core, 15 in geo, 11 in activity.

## Pooler, not direct

```
service → myota-<db>-pgbouncer:6432 → myota-<db>-postgres:5432 → PVC
```

Every service reaches PostgreSQL through its pooler. The only two exceptions are the
**migration Job**, which needs one session for its advisory lock and one transaction for
its whole migration set, and **the pooler itself**.

Two mechanisms, because a rule that is only written down is a rule somebody will not have
read:

1. **`tools/check_deploy.py`** renders the chart and fails if any ConfigMap a workload reads
   names a `-postgres` host. A service cannot be *configured* to bypass the pooler.
2. **A NetworkPolicy per database** allows ingress only from its own pooler, from the
   migration Job, and from the node (kubelet probes are host traffic). A pod that is
   neither cannot reach the database even with the right host and password.

Verified on the cluster: a plain pod connecting to `myota-core-postgres:5432` reports
`DIRECT-BLOCKED`, while the same credentials through `myota-geo-pgbouncer:6432` return
`POOLER-OK 3.4 …`. The database pods stay `Ready` and the migration Job still succeeds under
the policy, which is the pair of things that would break if the selectors or the probe
exception were wrong.

The connection budget, also checked:

| Setting | Value | Where |
| --- | --- | --- |
| `max_connections` | 200 | PostgreSQL `-c` flag |
| `DEFAULT_POOL_SIZE` | 25 | PgBouncer, server connections per pool |
| `MAX_CLIENT_CONN` | 500 | PgBouncer, client ceiling |
| `POOL_MODE` | `transaction` | PgBouncer |

Transaction mode is safe only because the application keeps no session-scoped state: no
advisory lock held across requests, no prepared statement outside a transaction, no `SET`
that outlives a statement. The endpoint ConfigMap states `DATABASE_POOL_MODE` next to the
host so that constraint is visible where a driver is configured.

## Migrations

Per database, one `psql` invocation:

```sh
psql -v ON_ERROR_STOP=1 --single-transaction \
     -c "SET LOCAL lock_timeout = '60s'" \
     -c "SELECT pg_advisory_xact_lock($LOCK_KEY)" \
     -f /sql/<db>/000_ops_schema.sql -f /sql/<db>/010_baseline.sql -f /sql/<db>/020_updated_at.sql
```

- **One session, one transaction**, so the advisory lock is held for the whole set and
  released at COMMIT — which means a killed Job releases it.
- **A failure rolls back everything**, so a database is never half-migrated.
- **`IF NOT EXISTS` everywhere**, so running the set twice changes nothing.

Verified in the cluster — a second full run left identical object counts:

| Database | Tables | Indexes | Triggers |
| --- | --- | --- | --- |
| `myota_core` | 32 | 68 | 22 |
| `myota_geo` | 23 | 50 | 11 |
| `myota_activity` | 18 | 42 | 10 |

The Job runs as the **application role**, not the superuser. The first deployment used the
superuser and produced a schema the services could not read: every table was owned by
`postgres`, and `myota` saw none of them in `information_schema`. The runner now asserts, as
`myota`, that the tables are visible and that it can create one.

A replica never migrates. It verifies the schema version and refuses to start if behind
(AGENTS.md rule 12); `libs/ops` provides `VerifySchema` for that.

## Applying it

```sh
# 1. the namespace — F6 replaces this with the Fleet bundle
kubectl create namespace myota-test --dry-run=client -o yaml | kubectl apply -f -

# 2. the credentials, from the SOPS files. Never from a plaintext file in Git.
for db in core geo activity; do
  sops --decrypt deploy/myota/secrets/$db.sops.yaml | kubectl apply -f -
done

# 3. the release
helm install myota deploy/myota --namespace myota-test
```

`helm upgrade` re-runs the migration Job — it is a `post-install,post-upgrade` hook. The
Job is idempotent, so the usual case is a fast no-op.

## Verifying

```sh
python3 tools/check_db_schema.py --check       # schema files match the data model
python3 tools/check_db_schema.py --apply URL   # all three apply twice; PostGIS only on geo
python3 tools/check_deploy.py --check          # the chart renders and keeps its guarantees

kubectl -n myota-test exec sts/myota-geo-postgres -- psql -U postgres -c 'select postgis_version()'
kubectl -n myota-test exec sts/myota-core-postgres -- psql -U postgres -c 'select postgis_version()'
```

The first prints `3.4 USE_GEOS=1 USE_PROJ=1 USE_STATS=1`; the second must **fail**. That
asymmetry is the point — an extension present everywhere would also "work", and would mean
the spatial database is not the only spatial database.

`psql -U postgres` with no `-d` connects to the `postgres` maintenance database, so the geo
init script enables PostGIS there as well as in `myota_geo`. An operator asking "does this
instance have PostGIS?" should not have to remember a database name to get a truthful
answer.

## What is not here

- **No replication.** Three independent databases on one node, restored from the volume
  rather than from a standby. The PVCs use `retained-local-path`, whose reclaim policy is
  `Retain`, so deleting the StatefulSet does not delete the data — the honest protection
  for a cluster with one disk.
- **No automatic backup.** Tracked as X3 for object storage; the same reasoning applies to
  `myota_core` and `myota_activity`, which hold the only copies of accounts and logs.
- **No Fleet bundle.** F6 adds `deploy/fleet/myota-test/`, which points at this chart.

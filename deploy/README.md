# deploy

Charts, values, Fleet definitions and encrypted secrets. Two environments, one
chart.

```
deploy/
  .sops.yaml             the cluster's age recipient, one rule per secret path
  myota/                 the chart, its values, its migrations and its secrets
    Chart.yaml values.yaml
    templates/           three StatefulSets, three poolers, the migration Job
    db/                  per-database migrations (shared, core, geo, activity)
    secrets/             SOPS-encrypted Secrets — applied deliberately, never reconciled
                         core.sops.yaml            myota-core-db-credentials
                         core-signing.sops.yaml    myota-core-signing (JWT seed, MFA key)
                         geo.sops.yaml activity.sops.yaml
                         s3-credentials.myota-{test,prod}.sops.yaml
  silo/                  object-storage inventory, IAM policies and provisioning
  fleet/
    README.md            how a commit becomes a deploy, and the three silent failures
    myota/
      bootstrap.yaml     workspace namespace, mapping and GitRepo — applied once by hand
    myota-test/          bundle → namespace myota-test (F6)
      fleet.yaml values.yaml   pins the published chart by version
```

## Current state

**F5** adds the three databases, their baselines, the poolers and the migration Job —
see [myota/README.md](myota/README.md). **F8** adds the object-storage provisioning under
[silo/](silo/README.md). **F6** adds the Fleet bundles, and **F7** wires image publication
and digest pinning.

The namespace `myota-test` exists as of F5, because the databases' verification commands
name it. F6 replaces the manual apply with a Fleet bundle; it does not change the
namespace's contents.

The chart is authored once in `myota/`, packaged and pushed to
`oci://ghcr.io/myota-platform/charts/myota` by the `publish-chart` job in
[`deploy.yml`](../.github/workflows/deploy.yml), and pinned by version in each bundle's
`fleet.yaml`. No bundle carries chart files: `tools/check_fleet_bundles.py` fails when a
bundle vendors a chart or pins a version other than the one in `Chart.yaml`, and the
workflow refuses to overwrite a published version with different content.
[fleet/README.md](fleet/README.md) explains why — including the three silent non-deploys
that led here.

## Environments

| Environment | Namespace | Participant | Admin |
| --- | --- | --- | --- |
| test | `myota-test` | `myota-test.ea7klk.es` | `myota-admin-test.ea7klk.es` |
| production | `myota` | — | — |

Both in Fleet workspace `myota`. See `AGENTS.md` rule 8.

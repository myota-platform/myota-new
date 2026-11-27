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
  silo/                  object-storage inventory, IAM policies and provisioning
  fleet/
    myota-test/          bundle → namespace myota-test (F6)
    myota/               bundle → namespace myota, production (F6)
```

## Current state

**F5** adds the three databases, their baselines, the poolers and the migration Job —
see [myota/README.md](myota/README.md). **F8** adds the object-storage provisioning under
[silo/](silo/README.md). **F6** adds the Fleet bundles, and **F7** wires image publication
and digest pinning.

The namespace `myota-test` exists as of F5, because the databases' verification commands
name it. F6 replaces the manual apply with a Fleet bundle; it does not change the
namespace's contents.

## Environments

| Environment | Namespace | Participant | Admin |
| --- | --- | --- | --- |
| test | `myota-test` | `myota-test.ea7klk.es` | `myota-admin-test.ea7klk.es` |
| production | `myota` | — | — |

Both in Fleet workspace `myota`. See `AGENTS.md` rule 8.

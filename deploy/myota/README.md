# deploy/myota

The chart, its values, its configuration and its secrets. One chart, two
environments, selected by values file — `deploy/fleet/<environment>/` supplies
the namespace and points at this directory.

```
myota/
  Chart.yaml, values.yaml      the chart (F6)
  values-myota-test.yaml       test overrides (F6)
  config/                      ConfigMaps — plaintext, reconciled by Fleet (F6)
  secrets/                     SOPS-encrypted Secrets — applied by hand, never reconciled (F6)
  db/
    core/                      migrations for myota_core (F5)
    geo/                       migrations for myota_geo (F5)
    activity/                  migrations for myota_activity (F5)
```

## Current state

Chunk **F1** creates this directory and this file — nothing else. **F5** adds
`db/` and the PgBouncer wiring; **F6** adds the chart, the Fleet bundle, the
namespace and both hostnames; **F7** wires image publication and digest pinning.

## Why configuration and secrets are separate directories

Configuration is plaintext in Git and reconciled by Fleet, because a reviewable
diff is the point. Secrets are SOPS-encrypted, live in `secrets/`, and are
applied deliberately — a GitRepo with `clientSecretName` would otherwise be able
to decrypt and rotate them on every reconcile, which is authority nobody asked
for. ADR-0003 records the decision and `docs/architecture/secrets-and-config.md`
the mechanics.

`secrets/` is created by F6 together with `deploy/.sops.yaml`. It is not created
earlier because a directory nobody has put a secret in is a directory whose SOPS
rule is untested.

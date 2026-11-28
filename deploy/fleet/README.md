# Fleet definitions

One directory per environment. Each holds the `fleet.yaml` Fleet reads, the values for that
environment, and the chart copy that `fleet.yaml` renders. `deploy/fleet/myota/bootstrap.yaml`
holds the cluster objects that make the workspace exist.

- `myota-test/` — namespace `myota-test`, `https://myota-test.ea7klk.es` and
  `https://myota-admin-test.ea7klk.es`.
- `myota/` — production. Not deployed.

## The flow

A commit to `main` that touches `deploy/fleet/<environment>/` is a deploy. Fleet polls the
repository, builds a bundle from the `fleet.yaml` it finds under the GitRepo's `paths`, and
the cluster converges. Nothing applies manifests by hand to ship.

The GitRepo lives in the workspace namespace `myota`, not in the namespace it deploys to. That
is Fleet's model: the workspace holds the *intent*, the workloads land in `myota-test`.

## Three silent failures, and what each one looks like

All three produce a green GitRepo and no workload. They are recorded because each one cost
time, and because the display alone does not distinguish them.

**1. No `BundleNamespaceMapping`.** Fleet resolves target clusters inside the bundle's own
namespace plus the namespaces matched by every `BundleNamespaceMapping` in that namespace. The
local cluster is in `fleet-local`. Without a mapping, `targets: [{clusterName: local}]` matches
nothing: the GitRepo shows `0/0`, the bundle shows `Ready` with an empty partition, and no
`BundleDeployment` exists anywhere. The fix is the `local-cluster` mapping in
`myota/bootstrap.yaml` — and the bundle must also carry the
`fleet.spainip.es/workspace: myota` label the mapping selects on.

**2. `targets` in both `fleet.yaml` and the GitRepo.** Fleet appends a `fleet.yaml`'s targets to
the GitRepo's, so declaring `clusterName: local` in both yields two identical targets and again
no `BundleDeployment`, with no error and no event. Targeting belongs in the GitRepo, which is the
per-environment object.

**3. A packaged chart.** `helm.chart: myota-0.1.0.tgz` inside the bundle does not deploy. Fleet
stores the archive as an encoded blob, the agent cannot load it as a chart, and it falls back to
a synthesised chart with no manifests: Helm reports the release `deployed`, the bundle reports
`Ready`, and the namespace stays empty. `helm.chart` must name a chart *directory* inside the
bundle.

## Why the chart is a copy

Fleet resolves `helm.chart` relative to the `fleet.yaml` and ships only the files under the
GitRepo's path, so a bundle at `deploy/fleet/myota-test/` cannot reach the chart authored at
`deploy/myota/`. Every bundle therefore carries `chart/`, a copy made by

```sh
python3 tools/sync_fleet_chart.py          # write the copies
python3 tools/sync_fleet_chart.py --check  # fail when a copy has drifted
```

`make checks-deploy` runs `--check`, so a template edited in `deploy/myota/` without re-syncing
fails `make verify` rather than deploying yesterday's chart. `deploy/myota/secrets/` is never
copied: encrypted secret files stay out of every bundle (ADR-0003).

The copy costs 19 files per environment. The alternative that removes it is publishing the chart
as an OCI artifact to GHCR and pointing `helm.chart` at a version; that is worth doing when a
second environment exists, and it is recorded as an open item rather than done here.

## Deploying by hand, when you must

`bootstrap.yaml` is the only hand-applied file. It is idempotent:

```sh
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl apply -f deploy/fleet/myota/bootstrap.yaml
kubectl -n myota get gitrepo myota-test        # 1/1 when the bundle is ready
```

To make Fleet rebuild a bundle from the current commit — after changing `fleet.yaml`, the chart
copy, or the GitRepo's labels — bump `spec.forceSyncGeneration` on the GitRepo. Fleet treats an
unchanged commit as an unchanged bundle and otherwise keeps the bundle it already built, which
means a label change does not propagate.

## Secrets

Not here, by design. Database passwords and S3 credentials are SOPS-encrypted files in
`deploy/myota/secrets/`, applied by hand (ADR-0003). Configuration that is not secret goes
through `values.yaml` and the chart's ConfigMaps; a `Secret` object never enters a bundle this
workspace reconciles.

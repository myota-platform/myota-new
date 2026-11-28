# Fleet definitions

One directory per environment, holding the `fleet.yaml` Fleet reads and the values for that
environment. The chart is not here — it is published to GHCR and pinned by version.
`deploy/fleet/myota/bootstrap.yaml` holds the cluster objects that make the workspace exist.

- `myota-test/` — namespace `myota-test`, `https://myota-test.ea7klk.es` and
  `https://myota-admin-test.ea7klk.es`.
- `myota/` — production. Not deployed. It shares the name `myota` with the workspace itself,
  which is accepted and recorded in [ADR-0010](../../docs/decisions/0010-workspace-and-production-share-a-name.md).

## The flow

A commit to `main` that touches `deploy/myota/` publishes a chart; a commit that touches
`deploy/fleet/<environment>/` changes what is deployed. Fleet polls the repository, builds a
bundle from the `fleet.yaml` it finds under the GitRepo's `paths`, and the cluster converges.
Nothing applies manifests by hand to ship, and nothing is deployed to test that is not in a
commit.

The GitRepo lives in the workspace namespace `myota`, not in the namespace it deploys to. That
is Fleet's model: the workspace holds the *intent*, the workloads land in `myota-test`.

## The chart is published, not vendored

```yaml
helm:
  chart: oci://ghcr.io/myota-platform/charts/myota
  version: 0.1.0
  valuesFiles:
    - values.yaml
```

`deploy/myota` is packaged and pushed by the `publish-chart` job in
[`.github/workflows/deploy.yml`](../../.github/workflows/deploy.yml), using the `GITHUB_TOKEN`
the workflow already has. The package is public, so Fleet — and anyone else — pulls it without
a credential. Fleet downloads the OCI chart itself and unrolls it into the bundle, so
`valuesFiles` still come from the bundle directory.

**Publish it only from the workflow.** A package pushed from a laptop with a personal token
is created *private and unlinked*, and GitHub has no API left that can change a package's
visibility — REST exposes only `GET`, `DELETE`, `restore` and `versions`. The only remedy is to
delete the package and let the job recreate it. The job pulls the chart back with an empty
registry config after pushing, so a private package fails CI rather than a Fleet reconcile.

**A published version is immutable.** Re-publishing changed content under an existing version
fails the job with a message naming the two files to edit:

```
myota 0.1.0 is already published with different contents.
Bump version in deploy/myota/Chart.yaml and in deploy/fleet/*/fleet.yaml, then merge.
```

Re-running the job on unchanged content is a no-op instead of a failure, so a re-run or a force
push does not turn the branch red. `tools/check_fleet_bundles.py` then asserts that every
bundle's `version` equals `deploy/myota/Chart.yaml`, because a published chart nobody points at
is a deploy that never happens.

Why not a chart inside the bundle:

- **It is a second copy.** The deployment definition would exist once per environment, and the
  copy that matters is the one nobody edited.
- **A packaged `.tgz` does not deploy.** Fleet stores the archive as an encoded blob, the agent
  cannot load it as a chart, and it falls back to a synthesised chart with no manifests: Helm
  reports the release `deployed`, the bundle reports `Ready`, and the namespace stays empty.
- **A chart directory does deploy, and was what F6 shipped** — 19 files copied per environment
  by a script, guarded by a drift check. That worked and is gone: the registry does the same
  job without a copy, and the version pin is a stronger guarantee than a file comparison.

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
per-environment object. `tools/check_fleet_bundles.py` rejects `targets` in a bundle.

**3. A chart that cannot be loaded.** A packaged `.tgz` inside the bundle installs an empty
release; a chart path outside the bundle directory resolves in the clone but those files are
never shipped. Both report `Ready` with nothing running. The published OCI chart removes the
question — the reference either resolves in the registry or the bundle fails loudly.

## Deploying by hand, when you must

`bootstrap.yaml` is the only hand-applied file. It is idempotent:

```sh
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl apply -f deploy/fleet/myota/bootstrap.yaml
kubectl -n myota get gitrepo myota-test        # 1/1 when the bundle is ready
```

To make Fleet rebuild a bundle from the current commit — after changing `fleet.yaml`, the pinned
chart version, or the GitRepo's labels — bump `spec.forceSyncGeneration` on the GitRepo. Fleet
treats an unchanged commit as an unchanged bundle and otherwise keeps the bundle it already
built, which means a label change does not propagate.

## Two names for one namespace

The Fleet workspace and the production environment are both called `myota`. Production does not
exist yet; when it does, its workloads, its Helm release secrets and this workspace's GitRepos
will share one namespace. Accepted, with the consequences written down in
[ADR-0010](../../docs/decisions/0010-workspace-and-production-share-a-name.md). The operational
rule that follows from it: **never delete the namespace `myota`** — that is production and the
deploy intent in one command.

## Secrets

Not here, by design. Database passwords and S3 credentials are SOPS-encrypted files in
`deploy/myota/secrets/`, applied by hand (ADR-0003). Configuration that is not secret goes
through `values.yaml` and the chart's ConfigMaps; a `Secret` object never enters a bundle this
workspace reconciles.

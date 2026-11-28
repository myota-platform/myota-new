# ADR-0011 — The chart is a published OCI artifact, pinned by version

**Status:** accepted · 2026-11-28
**Provenance:** directed in part — "Resolve #30 but [solve] the packages with the best solution in your opinion" — with the publishing and immutability mechanics chosen here.

## Context

Fleet reads a chart from files **inside a bundle directory**. A bundle at
`deploy/fleet/myota-test/` cannot reach a chart authored at `deploy/myota/`, and a packaged
`.tgz` inside the bundle does not work either: the agent cannot load it, installs a synthesised
empty chart, and reports the release `deployed` while the namespace stays empty (F6).

The workaround F6 shipped was a copy: `tools/sync_fleet_chart.py` copied 19 chart files into
`deploy/fleet/myota-test/chart/`, and `make checks-deploy` failed when the copy drifted. That
is correct and it is duplication — the deployment definition exists once per environment, and
`AGENTS.md` opens by asking the repository to avoid exactly this. With production it becomes
two copies of the same nineteen files, drifting independently, with a script whose only purpose
is to keep them equal.

Issue #30 (X8) asked for the removal. The question is which mechanism replaces the copy.

## Decision

**Publish the chart to GHCR as an OCI artifact and pin it by version.**

```yaml
helm:
  chart: oci://ghcr.io/myota-platform/charts/myota
  version: 0.1.0        # equals deploy/myota/Chart.yaml
  valuesFiles: [values.yaml]
```

- **Publication** is the `publish-chart` job in `.github/workflows/deploy.yml`: it runs on
  `main` when the chart changed, packages `deploy/myota`, and pushes it with the `GITHUB_TOKEN`
  the workflow already has. No stored credential, no second workflow, no path filter to keep in
  step with the existing one.
- **Immutability** is enforced by the job: if the version already exists in the registry with
  different contents, the job fails and names the two files to edit (`Chart.yaml` and every
  `deploy/fleet/*/fleet.yaml`). Re-running on unchanged content is a no-op instead of a
  failure, so a re-run or a force push does not turn the branch red.
- **The pin is checked**, not remembered: `tools/check_fleet_bundles.py` fails `make verify`
  when a bundle's `version` differs from the authored chart, when a bundle vendors a chart, or
  when a bundle sets `targets` or `clientSecretName`.
- **The package is public**, like the repository and the images, so Fleet and anyone else pull
  it anonymously.

`deploy/fleet/*/chart/` and `tools/sync_fleet_chart.py` are gone.

## Alternatives considered

**Keep the vendored copy.** Rejected: it is the duplication this decision removes, and the
guard only proves two copies are equal — not that either is the chart that is meant to run.
The version pin says which chart is meant to run.

**A Helm HTTP repository on GitHub Pages** (`index.yaml` plus `.tgz`, `helm.repo:
https://myota-platform.github.io/...`). Rejected: the repository's Pages were removed
deliberately in this project, a static site is a second artifact pipeline to keep alive, and
`index.yaml` is a file that goes stale in a way the OCI reference cannot. It also publishes the
chart under a host whose content depends on a Pages deployment succeeding.

**Fleet `HelmOp` resources instead of a GitRepo bundle.** Rejected: `/docs` — HelmOps still
produces bundles, adds a CRD per release, and moves the deploy intent out of the Git-committed
`fleet.yaml` that rule 8 describes. It solves a problem this project does not have (deploying
charts that are not in the repository).

**A chart repository on the cluster** (ChartMuseum or `helm serve`). Rejected: another workload
to run, patch and back up, on a single node, to serve one chart.

**Keep the copy for test and publish for production.** Rejected: two mechanisms for one thing,
and the one that gets tested is the one that ships.

## Consequences

**Cost**

- **Two files carry one version.** Bumping the chart means editing `deploy/myota/Chart.yaml`
  and every `deploy/fleet/*/fleet.yaml`. A check makes the mismatch loud, but the edit is still
  manual. This is the fee for a pinned deploy.
- **The controller needs egress to `ghcr.io`** when it builds a bundle. The images already come
  from there, so the dependency is not new, but a registry outage now blocks a deploy at bundle
  build rather than at pod start.
- **Anonymous pull requires the package to stay public.** If it were made private, the
  workspace would need a `helmSecretName` credential — a secret in the cluster, which ADR-0003
  keeps out of the deploy path on purpose.
- **"Immutable" means content-immutable, not byte-immutable.** The guard compares extracted
  chart contents, so a repackaged identical chart is accepted; only a content change under an
  existing version fails. Two clusters therefore run the same *chart*, not necessarily the same
  gzip. Recorded rather than implied.
- **Publishing is coupled to the workflow.** A chart that has to ship from a laptop (an
  emergency) needs `helm registry login` with a token that has `write:packages`. That path
  exists and is not automated.

**Gains**

- **One definition.** `deploy/myota` is the chart, everywhere, at every environment.
- **A rollback is a version number.** Reverting a `fleet.yaml` pin to the previous version is a
  reviewable diff, and the old chart is still in the registry.
- **The pin is auditable.** `kubectl -n myota get gitrepo myota-test -o jsonpath` on the bundle
  options names the chart version that is running, without reading a chart directory out of a
  bundle.
- **CI exercises the real artifact.** The chart that passes `check_deploy` in the `chart` job is
  the chart the `publish` job pushes.

## Verification

- Anonymous pull proves the package is public:
  `helm pull oci://ghcr.io/myota-platform/charts/myota --version 0.1.0 --registry-config <empty>`
  succeeds with no credentials.
- `python3 tools/check_fleet_bundles.py --check` — every bundle pins the authored version, has
  no vendored chart, no `targets`, no `clientSecretName`.
- The immutability guard is exercised by re-running the workflow on unchanged content: it
  reports "already published with identical contents" and exits zero.
- On the cluster, the bundle reports `Ready` and no `chart/` directory exists in the repository:
  `git ls-files deploy/fleet` lists only `fleet.yaml`, `values.yaml`, `README.md` and
  `bootstrap.yaml`.

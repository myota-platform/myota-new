# ADR-0010 — The Fleet workspace and the production namespace are both `myota`

**Status:** accepted · 2026-11-28
**Provenance:** directed — "Resolve #31 accepting the collision" — with the consequences worked out here.

## Context

Fleet's model on this cluster is one **workspace namespace** per project, holding `GitRepo`,
`Bundle`, `BundleNamespaceMapping` and the `BundleDeployment` options secrets. The workspace
namespace is a real namespace: `myota` was created for it in F6 and holds `GitRepo/myota-test`.

`AGENTS.md` rule 8 also names the production **namespace** `myota`. One namespace cannot be two
names cleanly: when `deploy/fleet/myota/` is written, production StatefulSets, its Helm release
secrets, and the GitRepos that deploy it would all live in `myota`, mixed with nothing that
distinguishes intent from workload.

Issue #31 (X9) recorded three ways out: rename production to `myota-prod`, rename the workspace
to `myota-platform`, or keep both and say so. The question is not whether it works — it does,
today, because production does not exist — but whether it is a name collision a future reader
can survive.

## Decision

**Keep the collision** — `AGENTS.md` rule 8 and `deploy/fleet/README.md` document that the
workspace namespace also holds production workloads. This is the third option recorded in
issue #31, taken verbatim:

> Keep the collision and document that the workspace namespace also holds production
> workloads. Not recommended: it makes "delete the workspace" and "delete production" the
> same command.

The Fleet workspace is `myota`. The production namespace is `myota`. Production workloads,
their Helm release secrets and the workspace's Fleet objects share one namespace, and both
`AGENTS.md` rule 8 and `deploy/fleet/README.md` say so.

The operational rule that follows, and the reason the option was marked "not recommended":
**never `kubectl delete namespace myota`.** That command is production and the deploy intent
in one action. Recovery is `kubectl apply -f deploy/fleet/myota/bootstrap.yaml` plus a
re-reconcile, which rebuilds the intent but not the data — the PVCs go with the namespace.

## Alternatives considered

**Rename production to `myota-prod`.** The cheapest technically: nothing exists to migrate, and
`myota-test` / `myota-prod` reads as a pair. Rejected because it makes the *namespace* naming
inconsistent with the *host* naming — test is `myota-test.ea7klk.es`, and a production pair of
`myota-prod.ea7klk.es` / `myota-admin-prod.ea7klk.es` is one `-prod` more than anyone needs to
type. The collision is between two things a reader meets separately: a `kubectl -n myota` and a
`GitRepo` in the workspace list.

**Rename the workspace to `myota-platform`.** Cleanest separation of concerns, and it makes
"where does the deploy intent live" and "where does production run" different answers to
different questions. Rejected because the workspace name is already referenced in the F6 issue,
in this repository's deployment table, in the live `GitRepo`s and in the cluster's
`BundleNamespaceMapping`; renaming it now is a migration to buy clarity that the rule can give
for free. Worth revisiting if a second cluster is ever registered, because then the workspace
stops being one-to-one with production.

**A second cluster for production.** Rejected as disproportionate: a second k3s control plane,
a second Fleet agent, a second set of databases, for a project that has one node.

**Put production in `myota-test`'s sibling and the workspace elsewhere in `fleet-default`.**
Rejected: `fleet-default` is where Rancher's own infrastructure bundles live, and a project's
intent does not belong next to them.

## Consequences

**Cost**

- **One command can destroy production.** `kubectl delete ns myota` now means "delete the
  workspace *and* production". No mechanism prevents it; the namespace's
  `helm.sh/resource-policy: keep` protects it from `helm uninstall`, not from `kubectl delete`.
  This is the price of the decision, and it is why the rule is stated in two places.
- **Two audiences see one namespace.** Someone administering Fleet objects and someone
  administering production workloads have the same RBAC scope. There is one operator today, so
  the cost is latent; with a second person it becomes a real permission boundary that does not
  exist.
- **`kubectl -n myota get all` is ambiguous.** It lists workloads; `get gitrepo,bundle` lists
  intent. A reader has to know which objects they are looking for to know what they are seeing.
- **The workspace namespace gains a `Namespace` template.** The chart creates `myota` with
  `helm.sh/resource-policy: keep` for production, the same way it creates `myota-test`. Helm
  adopting the namespace that holds its own release is unusual, and it is documented here so
  nobody "fixes" it by removing the annotation.

**Gains**

- **One name to remember.** `myota` is the project, the workspace, and production. Test is
  `myota-test`, and the difference between them is the suffix people already read.
- **No migration.** Nothing in the cluster, in the repository or in the issues has to change.
- **The collision is now a decision rather than an accident.** A future contributor who notices
  it finds this record instead of re-litigating it, which is the whole point of writing it down.

## Verification

- `AGENTS.md` rule 8 names `myota` as both the workspace and the production namespace, with the
  "never delete" rule; `deploy/fleet/README.md` repeats it next to the deploy instructions.
- `python3 tools/check_adrs.py` confirms this record is complete, accepted, and referenced from
  `AGENTS.md` — the same gate every other decision passes.
- On the cluster, the workspace exists once and holds only Fleet objects:
  `kubectl -n myota get gitrepo,bundle,bundlenamespacemapping` returns the deploy intent, and
  `kubectl get ns myota -o jsonpath='{.metadata.annotations}'` shows
  `helm.sh/resource-policy: keep`.

#!/usr/bin/env python3
"""Check the database chart against the architecture, by rendering it.

    python3 tools/check_deploy.py --check      # what `make verify` runs

Rendering rather than reading the templates, because a template that looks right and
renders wrong is the failure mode Helm exists to have. `helm template` is the same
evaluation the cluster performs, minus the cluster.

What it asserts, and why each one is a rule rather than a preference:

  pooler only   no application-facing configuration names a `-postgres` host. The pooler is
                the sole path from a service to its database; a direct host in a config the
                services read would be a path that bypasses the connection budget and the
                one place connections are counted.
  one of each   every database has a StatefulSet, a Service, a pooler Deployment and a
                pooler Service. A database with no pooler is a database some service will
                reach directly.
  the numbers   `max_connections=200`, `DEFAULT_POOL_SIZE=25`, `POOL_MODE=transaction`,
                `MAX_PREPARED_STATEMENTS>0`,
                PVCs between 10 and 20 Gi. These are the values the issue names, and a
                chart edit that changes them is a change to the connection budget.
  migrations    a Job exists, it takes an advisory lock, and it runs the SQL in one
                transaction. A migration set split across transactions leaves a database
                neither at the old schema nor the new one.
  api surface   the gateway is reachable on both hosts for `/v1` **and** for its own
                surface — `/openapi.json`, `/docs`, `/event-schemas`. An API-first project
                whose playground is unreachable from its own hostname is a contract nobody
                can try; the first deploy shipped exactly that, with `/openapi.json`
                returning the front end's 404.
  no secrets    no Secret is templated. Credentials are applied out of band from
                `secrets/*.sops.yaml` (ADR-0003); a chart that can create a Secret is a
                chart that has a password in its values.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
CHART = REPO_ROOT / "deploy" / "myota"

DATABASES = ("core", "geo", "activity")
RELEASE = "myota"
NAMESPACE = "myota-test"

# The connection budget. Named here so a chart edit that changes one has to change this
# file too, and the change is visible in a diff rather than buried in a template.
EXPECTED_MAX_CONNECTIONS = 200
EXPECTED_POOL_SIZE = 25
EXPECTED_POOL_MODE = "transaction"
PVC_MIN_GIB, PVC_MAX_GIB = 10, 20


class Problem(Exception):
    def __init__(self, messages: list[str]):
        super().__init__("\n".join(messages))
        self.messages = messages


def render(values_file: Path | None = None) -> list[dict]:
    """Render the chart, optionally with an environment's values on top.

    The default render is the chart's own defaults, where every application service is
    off — the database-only shape F5 verified. The *deployed* shape comes from the test
    environment's values file, and some assertions (the API routes, the workloads) only
    exist there. Rendering both means an assertion cannot pass by rendering nothing.
    """
    if shutil.which("helm") is None:
        raise Problem(["helm is not on PATH; the chart cannot be rendered"])
    command = ["helm", "template", RELEASE, str(CHART), "--namespace", NAMESPACE]
    if values_file is not None:
        command += ["--values", str(values_file)]
    result = subprocess.run(command, capture_output=True, text=True, timeout=120)
    if result.returncode != 0:
        raise Problem([f"helm template failed: {result.stderr.strip()[:600]}"])
    return [doc for doc in yaml.safe_load_all(result.stdout) if doc]


def by_kind(objects: list[dict], kind: str) -> list[dict]:
    return [obj for obj in objects if obj.get("kind") == kind]


def name_of(obj: dict) -> str:
    return obj.get("metadata", {}).get("name", "")


def check(objects: list[dict]) -> list[str]:
    problems: list[str] = []

    statefulsets = {name_of(o): o for o in by_kind(objects, "StatefulSet")}
    services = {name_of(o): o for o in by_kind(objects, "Service")}
    deployments = {name_of(o): o for o in by_kind(objects, "Deployment")}
    jobs = by_kind(objects, "Job")
    configmaps = {name_of(o): o for o in by_kind(objects, "ConfigMap")}

    # ── one of each, per database ────────────────────────────────────────────────
    for database in DATABASES:
        expected = {
            "StatefulSet": f"{RELEASE}-{database}-postgres",
            "Deployment": f"{RELEASE}-{database}-pgbouncer",
        }
        if expected["StatefulSet"] not in statefulsets:
            problems.append(f"no StatefulSet {expected['StatefulSet']} for {database}")
        if expected["Deployment"] not in deployments:
            problems.append(f"no pooler Deployment {expected['Deployment']} for {database}")
        for service in (f"{RELEASE}-{database}-postgres", f"{RELEASE}-{database}-pgbouncer"):
            if service not in services:
                problems.append(f"no Service {service} for {database}")

    # ── sizes ───────────────────────────────────────────────────────────────────
    for name, statefulset in statefulsets.items():
        claims = (
            statefulset.get("spec", {})
            .get("volumeClaimTemplates", [])
        )
        if not claims:
            problems.append(f"{name} has no volumeClaimTemplate; a database without a volume is a test fixture")
            continue
        for claim in claims:
            size = claim.get("spec", {}).get("resources", {}).get("requests", {}).get("storage", "0Gi")
            match = re.match(r"^(\d+)Gi$", str(size))
            if not match:
                problems.append(f"{name}: storage {size!r} is not a whole number of Gi")
                continue
            gib = int(match.group(1))
            if not PVC_MIN_GIB <= gib <= PVC_MAX_GIB:
                problems.append(
                    f"{name}: storage {size} is outside the {PVC_MIN_GIB}–{PVC_MAX_GIB} Gi "
                    f"range the plan states"
                )

    # ── the connection budget ───────────────────────────────────────────────────
    for name, statefulset in statefulsets.items():
        containers = statefulset["spec"]["template"]["spec"]["containers"]
        args = " ".join(" ".join(c.get("args", [])) for c in containers)
        match = re.search(r"max_connections=(\d+)", args)
        if not match:
            problems.append(f"{name}: does not set max_connections")
        elif int(match.group(1)) != EXPECTED_MAX_CONNECTIONS:
            problems.append(
                f"{name}: max_connections={match.group(1)}, expected {EXPECTED_MAX_CONNECTIONS}"
            )

    # Only the poolers. Every Deployment in the chart is not a pooler: the front ends are
    # Deployments too, and treating them as poolers asked why nginx has no POOL_MODE.
    for name, deployment in deployments.items():
        if not name.endswith("-pgbouncer"):
            continue
        env = {
            entry["name"]: entry.get("value")
            for entry in deployment["spec"]["template"]["spec"]["containers"][0].get("env", [])
        }
        if env.get("POOL_MODE") != EXPECTED_POOL_MODE:
            problems.append(f"{name}: POOL_MODE={env.get('POOL_MODE')!r}, expected {EXPECTED_POOL_MODE!r}")
        # Transaction mode plus a driver that names its statements needs the pooler to
        # track them. Without it two replicas collide on `sqlx_s_1` and the second gets
        # `prepared statement "sqlx_s_1" already exists` — the failure that took the
        # first A1 deploy down, and one a chart edit could reintroduce silently.
        prepared = str(env.get("MAX_PREPARED_STATEMENTS", ""))
        if not prepared.isdigit() or int(prepared) <= 0:
            problems.append(
                f"{name}: MAX_PREPARED_STATEMENTS={prepared!r}; transaction pooling needs a "
                f"positive value or named prepared statements collide across clients"
            )
        if str(env.get("DEFAULT_POOL_SIZE")) != str(EXPECTED_POOL_SIZE):
            problems.append(
                f"{name}: DEFAULT_POOL_SIZE={env.get('DEFAULT_POOL_SIZE')!r}, "
                f"expected {EXPECTED_POOL_SIZE}"
            )
        # The pooler is the one component that may name the database host.
        if not str(env.get("DB_HOST", "")).endswith("-postgres"):
            problems.append(f"{name}: DB_HOST={env.get('DB_HOST')!r} does not name its database Service")

    # ── pooler only: no application-facing config names a database host ──────────
    #
    # The pooler Deployments and the migration Job are the two legitimate clients of the
    # database Service. Everything else must go through the pooler. This checks the
    # rendered ConfigMaps — the only configuration a workload reads — rather than the
    # templates, so a new ConfigMap is covered without an edit here.
    for name, configmap in configmaps.items():
        if name.endswith("-initdb") or name.endswith("-migrations") or name.endswith("-migrate-script"):
            # initdb runs inside the database; migrations are the documented exception.
            continue
        for key, value in (configmap.get("data") or {}).items():
            if "-postgres" in str(value):
                problems.append(
                    f"ConfigMap {name}/{key} names a database host directly; services reach "
                    f"PostgreSQL through the pooler, never the StatefulSet"
                )

    # ── the pooler rule, as a network mechanism ─────────────────────────────────
    #
    # A ConfigMap check stops the mistake being configured; a NetworkPolicy stops it being
    # made. Both, because the second is what holds when someone debugs with a shell or a new
    # service is written before this rule is read.
    policies = {name_of(o): o for o in by_kind(objects, "NetworkPolicy")}
    for database in DATABASES:
        policy_name = f"{RELEASE}-{database}-postgres-ingress"
        policy = policies.get(policy_name)
        if policy is None:
            problems.append(
                f"no NetworkPolicy {policy_name}; any pod could reach {database} directly"
            )
            continue
        rules = policy.get("spec", {}).get("ingress", [])
        peers = [peer for rule in rules for peer in rule.get("from", [])]
        pooler_selector = any(
            peer.get("podSelector", {}).get("matchLabels", {}).get("app.kubernetes.io/name")
            == f"{RELEASE}-{database}-pgbouncer"
            for peer in peers
        )
        migrate_selector = any(
            peer.get("podSelector", {}).get("matchLabels", {}).get("myota.role") == "migrate"
            for peer in peers
        )
        host_rule = any("ipBlock" in peer for peer in peers)
        if not pooler_selector:
            problems.append(f"{policy_name}: does not admit its own pooler, so nothing works")
        if not migrate_selector:
            problems.append(f"{policy_name}: does not admit the migration Job, so migrations fail")
        if not host_rule:
            problems.append(
                f"{policy_name}: has no ipBlock rule, so kubelet probes are refused and the "
                f"pod is restarted forever"
            )

    # ── migrations ──────────────────────────────────────────────────────────────
    if not jobs:
        problems.append("no migration Job; the schema would have to be applied by hand")
    for job in jobs:
        pods = job["spec"]["template"]["spec"]
        command = " ".join(
            str(part) for container in pods["containers"] for part in container.get("command", [])
        )
        # The ConfigMap is named by the *volume*, not by the mount: a mount says where the
        # volume appears, the volume says what it is. Reading the mounts finds nothing,
        # which is how the first version of this check reported a missing script that was
        # rendered correctly two hundred lines earlier.
        sources = {
            volume["configMap"]["name"]
            for volume in pods.get("volumes", [])
            if "configMap" in volume
        }
        script = next(
            (
                configmaps[source]
                for source in sources
                if source in configmaps and "run.sh" in (configmaps[source].get("data") or {})
            ),
            None,
        )
        if script is None:
            problems.append(f"Job {name_of(job)} has no runner script mounted")
            continue
        body = script["data"]["run.sh"]
        for required, why in (
            ("pg_advisory_xact_lock", "the advisory lock that serialises two migrators"),
            ("--single-transaction", "one transaction, so a failure cannot leave a half-migrated database"),
            ("ON_ERROR_STOP", "without it psql continues past a failed statement"),
        ):
            if required not in body:
                problems.append(f"Job {name_of(job)}: runner script is missing {required} ({why})")
        if not command:
            problems.append(f"Job {name_of(job)} runs no explicit command")

    # ── the API surface is reachable ────────────────────────────────────────────
    #
    # The gateway serves more than `/v1`: `/openapi.json` is the composed contract and
    # `/docs` is the playground. Both were unreachable on the deployed hostnames because
    # the route matched only `/v1`, so the front end answered its own 404 — an API-first
    # project whose contract cannot be fetched from its own host.
    #
    # Checked against a second render, with the test environment's values: the chart's
    # defaults have no gateway, so a check against them would pass by finding nothing.
    deployed_values = REPO_ROOT / "deploy" / "fleet" / "myota-test" / "values.yaml"
    if deployed_values.is_file():
        try:
            deployed = render(deployed_values)
        except Problem as problem:
            problems.extend(problem.messages)
            deployed = []

        api_routes = [
            route
            for route in by_kind(deployed, "IngressRoute")
            if any(
                service.get("name") == "gateway"
                for rule in route.get("spec", {}).get("routes", [])
                for service in rule.get("services", [])
            )
        ]
        if not api_routes:
            problems.append(
                "the deployed values route nothing to the gateway; the API and the playground "
                "would be unreachable from the hostnames that document them"
            )

        required = ("PathPrefix(`/v1`)", "Path(`/openapi.json`)", "PathPrefix(`/docs`)")
        for route in api_routes:
            name = route["metadata"]["name"]
            matches = " ".join(
                rule.get("match", "") for rule in route.get("spec", {}).get("routes", [])
            )
            for fragment in required:
                if fragment not in matches:
                    problems.append(
                        f"{name}: the gateway route does not match {fragment}; the contract "
                        f"and the playground would be served by the front end's 404"
                    )

    # ── no secrets in the chart ─────────────────────────────────────────────────
    if by_kind(objects, "Secret"):
        problems.append(
            "the chart renders a Secret; credentials are applied from secrets/*.sops.yaml, "
            "never templated into a chart"
        )

    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="render and validate the chart (default)")
    args = parser.parse_args()
    # Default to checking, so the repository's glob can run this with no arguments.
    args.check = True

    try:
        objects = render()
        problems = check(objects)
        if problems:
            raise Problem(problems)
    except Problem as error:
        for problem in error.messages:
            print(f"  error  {problem}", file=sys.stderr)
        return 1

    counts = ", ".join(
        f"{len(by_kind(objects, kind))} {kind}"
        for kind in ("StatefulSet", "Deployment", "Service", "ConfigMap", "Job")
    )
    print(f"OK: chart renders {counts}; pooler-only access, connection budget and migrations intact")
    return 0


if __name__ == "__main__":
    sys.exit(main())

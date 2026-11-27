# CI credentials and GitHub Actions secrets

How a workflow authenticates, what it is allowed to hold, and why the answer is
"almost nothing".

## The rule that governs this document

**CI holds credentials for CI. It does not hold credentials for the running
system.** A pipeline builds, tests, and publishes. It does not reach into the
cluster, and therefore it does not need a cluster credential.

That single boundary removes most of the secret surface a CI system usually
accumulates.

## Three stores, three purposes — never merged

| Store | Holds | In Git? | Who applies it | Who can read it |
| --- | --- | --- | --- | --- |
| **GitHub Actions secrets** | credentials for the pipeline itself | no | GitHub at run time | workflow runs in this repository |
| **Fleet ConfigMaps** | non-secret configuration for the cluster | **yes, plaintext** | Fleet, automatically | anyone with repo read |
| **SOPS + age files** | secrets for the cluster | **yes, encrypted** | an operator workflow | anyone with the age key |

These are three different mechanisms solving three different problems. Mixing
them is the failure mode this document exists to prevent:

- A config value in GitHub Actions secrets means the cluster's behaviour is
  invisible to review, and a change needs a UI click instead of a pull request.
- A secret in a Fleet ConfigMap is plaintext in Git.
- A cluster secret in GitHub Actions secrets means the pipeline holds credentials
  it never uses, and a compromised workflow compromises production.

## What CI actually needs

Enumerated honestly, because the list is short:

| Need | Credential | Stored as a secret? |
| --- | --- | --- |
| Push images to GHCR | `GITHUB_TOKEN` with `packages: write` | **No** — minted per run |
| Read the repository | `GITHUB_TOKEN` (`contents: read`) | **No** |
| Publish a release | `GITHUB_TOKEN` (`contents: write`) | **No** |
| Deploy to the cluster | nothing — the pipeline pushes a commit, Fleet reconciles | **No** |

**CI needs zero stored secrets to build, test, and publish this system.** That is
the design, not an accident: GitOps means the deploy trigger is a commit, and
`GITHUB_TOKEN` covers registry authentication.

A stored secret is only justified when a workflow must reach something GitHub
cannot mint. Realistic cases here:

| Case | Secret | Where |
| --- | --- | --- |
| Integration test against a real SMTP account | `SMTP_TEST_PASSWORD` | repo, environment `integration` |
| Integration test against a hosted Postgres with PostGIS | `TEST_DB_URL` | repo, environment `integration` |
| A private Go/Rust module proxy | `PROXY_TOKEN` | org, shared by all repos |

If a proposed secret does not fit one of those rows, the first question is why CI
needs it at all.

## How to put a secret into GitHub Actions

### Repository level

```sh
# the value is read from stdin, never as an argument, so it stays out of shell history
gh secret set SMTP_TEST_PASSWORD --repo myota-platform/myota-new < /path/to/value

# or interactively (prompted, not echoed)
gh secret set SMTP_TEST_PASSWORD --repo myota-platform/myota-new
```

List what exists without revealing values:

```sh
gh secret list --repo myota-platform/myota-new
```

### Organisation level

For a credential several repositories share (a module proxy, a shared test
account):

```sh
# every repository in the org
gh secret set PROXY_TOKEN --org myota-platform --visibility all

# or only named repositories — --visibility selected is what --repos implies
gh secret set PROXY_TOKEN --org myota-platform --repos myota-new,myota-docs
```

`--visibility` takes `all`, `private` or `selected`. `--repos` is the `selected`
case, and passing both a visibility and a repository list is contradictory.

An org secret is inherited by every repository in its visibility scope. Prefer
repository-level unless the credential genuinely is shared — the blast radius of a
leaked org secret is every repository that inherits it.

### Environment level (preferred for test credentials)

An environment adds a protection boundary: it can require a reviewer, restrict
which branches may deploy to it, and it is only readable by jobs that declare it.

```sh
gh api -X PUT repos/myota-platform/myota-new/environments/integration
gh secret set TEST_DB_URL --repo myota-platform/myota-new --env integration
```

```yaml
jobs:
  integration:
    runs-on: ubuntu-24.04
    environment: integration        # ← makes env secrets available
    steps:
      - run: ./run-integration-tests.sh
        env:
          TEST_DB_URL: ${{ secrets.TEST_DB_URL }}
```

Use an environment for anything that touches a real external system. Repository
secrets are readable by any job in any workflow; environment secrets are readable
only by a job that names the environment and satisfies its rules.

### Consuming a secret in a workflow

```yaml
permissions:
  contents: read
  packages: write          # GHCR push, no stored secret needed

jobs:
  publish:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
        with:
          persist-credentials: false     # do not leave the token in .git/config

      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}   # minted per run, expires with it
```

## Rules for workflow authors

- **Never `echo` a secret.** GitHub masks exact matches in logs, but a
  transformed value (base64, split, concatenated) is not masked.
- **Never pass a secret as a command-line argument.** It appears in the process
  list and often in error output. Use an environment variable or stdin.
- **Never write a secret to a file in the workspace** unless the tool requires a
  file; use `$RUNNER_TEMP`, and `trap` its removal.
- **`persist-credentials: false` on checkout** unless the job needs to push.
- **Never use `pull_request_target` with a checkout of the PR head.** That
  combination gives untrusted code access to secrets. Fork pull requests receive
  **no secrets at all** by default, and that default must not be worked around.
- **Declare the minimum `permissions:`** at job level, not workflow level, so a
  compromised step is bounded.
- **Pin third-party actions to a commit SHA**, not a tag. A mutable tag is a
  supply-chain path into the credentials the job holds.
- **Do not build a deploy path that needs a cluster credential.** If a workflow
  seems to need one, the deploy design is wrong — see below.

## Why CI does not deploy

```
CI: build → test → push image to GHCR → push a commit bumping the image digest
                                                    │
Fleet: observes the commit ─────────────────────────┘
       applies the new digest to the cluster
```

The pipeline's last act is a git commit. It never talks to the Kubernetes API, so
it needs no kubeconfig, no service-account token, and no cluster secret. Two
consequences:

- **A compromised workflow cannot reach production.** It can push an image and
  open a commit; both are reviewable.
- **The cluster credential does not exist**, so it cannot leak.

This is the same reasoning as [ADR-0003](../decisions/0003-no-sealed-secrets.md):
remove the credential rather than guard it.

## Rotation

| Secret | Rotation | Mechanism |
| --- | --- | --- |
| `GITHUB_TOKEN` | per run | automatic |
| Repository/environment secrets | 180 days, or on staff change | `gh secret set` with the new value; GitHub keeps no history |
| Organisation secrets | 180 days | as above, then re-check inherited repositories |

Rotation is a `gh secret set` away, which is one reason to keep the list short. A
credential that is painful to rotate is a credential that will not be rotated.

## What is never allowed

- A secret committed to the repository in any form — enforced by `gitleaks` in
  pre-commit and CI, not by discipline.
- A secret in a workflow file, even base64-encoded or split across lines.
- A secret in a Fleet ConfigMap.
- A cluster credential in GitHub Actions.
- A `pull_request_target` job that checks out and runs PR code.

Configuration lives in `deploy/myota/config/`, is plaintext, is reviewed in pull
requests, and is reconciled by Fleet. Secrets live elsewhere and are applied by a
deliberate step. Keeping that line sharp is cheaper than any amount of masking.

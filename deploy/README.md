# deploy

Charts, values, Fleet definitions and encrypted secrets. Two environments, one
chart.

```
deploy/
  myota/                 the chart, values, configuration and secrets
    config/              ConfigMaps — plaintext, reconciled by Fleet
    secrets/             SOPS-encrypted Secrets — applied deliberately, never reconciled
    db/                  per-database migrations (core, geo, activity)
  fleet/
    myota-test/          bundle → namespace myota-test
    myota/               bundle → namespace myota (production)
```

## Current state

Chunk **F1** creates the directory and this file. **F5** adds the databases and
PgBouncer; **F6** adds the Fleet bundle, the namespace and both hostnames; **F7**
wires image publication and digest pinning.

## Environments

| Environment | Namespace | Participant | Admin |
| --- | --- | --- | --- |
| test | `myota-test` | `myota-test.ea7klk.es` | `myota-admin-test.ea7klk.es` |
| production | `myota` | — | — |

Both in Fleet workspace `myota`. See `AGENTS.md` rule 8.

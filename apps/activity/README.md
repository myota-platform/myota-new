# activity

MyOTA activity service.

| | |
| --- | --- |
| Language | Go 1.24 (Echo, pgx, chunk P5) |
| Owns | myota_activity |
| Run modes | api · worker · relay (chunk F1 provides none) |
| Chunk | **P5** implements it |

**Owned database: myota_activity.** It holds activations, QSOs and log imports, award definitions and issuance, statistics, and the `ops` schema.

No other service connects to it, and it connects to no other service's database.
That rule is the whole of ADR-0001: a service *is* the database it exclusively
owns.

## Current state

Scaffolding from chunk F1. The binary builds, prints one line and exits. There is
no HTTP listener, no database connection and no Dockerfile yet — `P5` adds
them.

```sh
go run .      # prints the scaffold status line
```

## Layout

- `main.go` — entry point, `run` and `describe`.
- `main_test.go` — pins the one behaviour that exists.

## What P5 adds

Activations, live QSO logging, ADIF import, award definitions and evaluation, statistics and leaderboards.

## Why it is not in the other language

Throughput and worker-pool concurrency for QSO ingest and award fan-out. There is no CPU-bound parsing and no compile-time schema argument, so ADR-0002 keeps it in Go beside the gateway.

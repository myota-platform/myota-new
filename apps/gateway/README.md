# gateway

MyOTA API gateway.

| | |
| --- | --- |
| Language | Go 1.24 (Chi, chunk F3) |
| Owns | no database |
| Run modes | api · worker · relay (chunk F1 provides none) |
| Chunk | **F3** implements it |

**Owned database: no database.** The gateway is stateless by design: it routes, serves the composed contract and enforces the session audience, and it holds no state of its own. A gateway with a database would be a fourth service, not a router.

No other service connects to it, and it connects to no other service's database.
That rule is the whole of ADR-0001: a service *is* the database it exclusively
owns.

## Current state

Scaffolding from chunk F1. The binary builds, prints one line and exits. There is
no HTTP listener, no database connection and no Dockerfile yet — `F3` adds
them.

```sh
go run .      # prints the scaffold status line
```

## Layout

- `main.go` — entry point, `run` and `describe`.
- `main_test.go` — pins the one behaviour that exists.

## What F3 adds

The prefix route table, the embedded contract with `/openapi.json` and `/docs`, the audience check per route class, `/healthz`, `/readyz` and `/metrics`.

## Why it is not in the other language

The gateway multiplexes I/O and terminates nothing. A single static binary with the contracts in `embed.FS` and no runtime to warm up is the whole requirement, which is why ADR-0002 puts it in Go.
